use crate::models::{
    SatelliteMetadataSource, SolutionResponse, SolveHintSource, SolveMode, SolveOptions,
    SolveStatistics, WcsResponse,
};
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use image::ImageFormat;
use seiza::{
    DetectConfig,
    blind::{BlindIndex, BlindParams, solve_blind},
    catalog::TileCatalog,
    detect_stars,
    solve::{SolveHint, solve},
};
use std::{
    collections::BTreeMap,
    io::Cursor,
    path::Path,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

pub const FITS_HEADER_PROBE_BYTES: usize = 80 * 1_440;

pub(crate) struct MonochromeImage {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u16>,
    pub adu_per_stored_unit: f64,
}

#[derive(Clone)]
pub struct SolverEngine {
    catalog: Option<Arc<TileCatalog>>,
    blind_index: Arc<OnceLock<Arc<BlindIndex>>>,
}

impl SolverEngine {
    pub fn from_catalog_paths(star_path: Option<&Path>, blind_index_path: Option<&Path>) -> Self {
        let catalog = star_path.and_then(|path| match TileCatalog::open(path) {
            Ok(catalog) => {
                tracing::info!(path = %path.display(), stars = catalog.star_count(), "opened Seiza star catalog");
                Some(Arc::new(catalog))
            }
            Err(error) => {
                tracing::error!(path = %path.display(), %error, "could not open Seiza star catalog");
                None
            }
        });
        let blind_index = Arc::new(OnceLock::new());
        if let (Some(catalog), Some(path)) = (&catalog, blind_index_path) {
            match BlindIndex::open(path) {
                Ok(index) => {
                    let source_stars = index.source_star_count();
                    let catalog_stars = catalog.star_count();
                    if source_stars != 0 && source_stars != catalog_stars {
                        tracing::error!(
                            path = %path.display(),
                            source_stars,
                            catalog_stars,
                            "Seiza blind index was built from a different star catalog; ignoring it"
                        );
                    } else {
                        tracing::info!(
                            path = %path.display(),
                            patterns = index.pattern_count(),
                            index_mag_limit = index.index_mag_limit(),
                            max_pattern_deg = index.max_pattern_deg(),
                            "memory-mapped Seiza blind index"
                        );
                        assert!(
                            blind_index.set(Arc::new(index)).is_ok(),
                            "blind index is initialized only once"
                        );
                    }
                }
                Err(error) => {
                    tracing::error!(
                        path = %path.display(),
                        %error,
                        "could not open Seiza blind index; a legacy index will be built on the first blind solve"
                    );
                }
            }
        }
        Self {
            catalog,
            blind_index,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.catalog.is_some()
    }

    pub(crate) fn catalog(&self) -> Option<Arc<TileCatalog>> {
        self.catalog.clone()
    }

    pub async fn solve(
        &self,
        bytes: Bytes,
        filename: String,
        options: SolveOptions,
    ) -> Result<SolutionResponse> {
        let catalog = self.catalog.clone().context(
            "solver is not configured: set SEIZA_STAR_DATA to a Seiza star tile catalog",
        )?;
        let blind_index = self.blind_index.clone();
        tokio::task::spawn_blocking(move || {
            solve_bytes(&catalog, &blind_index, &bytes, &filename, &options)
        })
        .await
        .context("solver worker panicked")?
    }
}

pub async fn preview_png(bytes: Bytes, filename: String) -> Result<Bytes> {
    encode_png(bytes, filename, true).await
}

pub async fn full_png(bytes: Bytes, filename: String) -> Result<Bytes> {
    encode_png(bytes, filename, false).await
}

async fn encode_png(bytes: Bytes, filename: String, thumbnail: bool) -> Result<Bytes> {
    tokio::task::spawn_blocking(move || {
        let image = decode_image(&bytes, &filename)?;
        let output_image = if thumbnail {
            image.thumbnail(1_800, 1_800)
        } else {
            image
        };
        let mut output = Cursor::new(Vec::new());
        output_image
            .write_to(&mut output, ImageFormat::Png)
            .context("encoding rendered PNG")?;
        Ok(Bytes::from(output.into_inner()))
    })
    .await
    .context("PNG worker panicked")?
}

pub fn dimensions_from_bytes(bytes: &[u8], filename: &str) -> Result<(u32, u32)> {
    let image = decode_image(bytes, filename)?;
    Ok((image.width(), image.height()))
}

pub(crate) fn decode_monochrome_u16(bytes: &[u8], filename: &str) -> Result<MonochromeImage> {
    if looks_like_fits(bytes, filename) {
        let fits = seiza_fits::FitsImage::from_bytes(bytes)
            .map_err(|error| anyhow::anyhow!("invalid FITS image: {error}"))?;
        let adu_per_stored_unit = match &fits.pixels {
            seiza_fits::Pixels::U8(_) => 1.0 / 256.0,
            seiza_fits::Pixels::U16(_) => 1.0,
            seiza_fits::Pixels::I32(values) => {
                scaled_adu_per_stored_unit(values.iter().map(|&value| value as f64))
            }
            seiza_fits::Pixels::F32(values) => {
                scaled_adu_per_stored_unit(values.iter().map(|&value| value as f64))
            }
            seiza_fits::Pixels::F64(values) => scaled_adu_per_stored_unit(values.iter().copied()),
        };
        return Ok(MonochromeImage {
            width: fits.width,
            height: fits.height,
            pixels: fits.to_u16().into_owned(),
            adu_per_stored_unit,
        });
    }

    let image = image::load_from_memory(bytes)
        .context("unsupported or corrupt image; submit FITS, PNG, JPEG, TIFF, or WebP")?;
    let eight_bit = matches!(
        image,
        image::DynamicImage::ImageLuma8(_)
            | image::DynamicImage::ImageLumaA8(_)
            | image::DynamicImage::ImageRgb8(_)
            | image::DynamicImage::ImageRgba8(_)
    );
    let width = image.width() as usize;
    let height = image.height() as usize;
    Ok(MonochromeImage {
        width,
        height,
        pixels: image.to_luma16().into_raw(),
        adu_per_stored_unit: if eight_bit { 1.0 / 257.0 } else { 1.0 },
    })
}

fn scaled_adu_per_stored_unit(values: impl Iterator<Item = f64>) -> f64 {
    let (minimum, maximum) = values.filter(|value| value.is_finite()).fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(minimum, maximum), value| (minimum.min(value), maximum.max(value)),
    );
    let span = maximum - minimum;
    if span.is_finite() && span > 0.0 {
        span / u16::MAX as f64
    } else {
        1.0
    }
}

fn solve_bytes(
    catalog: &TileCatalog,
    blind_index: &OnceLock<Arc<BlindIndex>>,
    bytes: &[u8],
    filename: &str,
    options: &SolveOptions,
) -> Result<SolutionResponse> {
    let total_started = Instant::now();
    options.validate().map_err(anyhow::Error::msg)?;
    let decode_started = Instant::now();
    let image = decode_image(bytes, filename)?;
    let decode_duration = decode_started.elapsed();
    let dimensions = (image.width(), image.height());
    if dimensions.0 == 0 || dimensions.1 == 0 {
        bail!("image has invalid dimensions");
    }
    let detection_started = Instant::now();
    let detected = detect_stars(
        &image,
        &DetectConfig {
            sigma: options.sigma,
            ignore_border: options.ignore_border,
            max_stars: options.max_stars.clamp(16, 2_000),
            ..Default::default()
        },
    );
    let detection_duration = detection_started.elapsed();
    tracing::info!(
        stars = detected.len(),
        width = dimensions.0,
        height = dimensions.1,
        "detected stars for queued solve"
    );

    let search_started = Instant::now();
    let (solution, mode, blind_index_patterns) = match (
        options.center_ra_deg,
        options.center_dec_deg,
        options.scale_arcsec_per_pixel,
    ) {
        (Some(ra), Some(dec), Some(scale)) => (
            solve(
                &detected,
                catalog,
                &SolveHint {
                    center: (ra, dec),
                    radius_deg: options.radius_deg.unwrap_or(2.0).clamp(0.1, 180.0),
                    scale_arcsec_px: scale,
                    scale_tolerance: options.scale_tolerance,
                    sip_order: options.sip_order,
                },
                dimensions,
            )
            .context("hinted Seiza solve failed")?,
            SolveMode::Hinted,
            None,
        ),
        _ => {
            let index = blind_index.get_or_init(|| {
                let params = BlindParams::default();
                tracing::warn!(
                    index_mag_limit = params.index_mag_limit,
                    "no prebuilt Seiza blind index is configured; building a legacy index once for this worker"
                );
                let index = BlindIndex::build(catalog, &params);
                tracing::info!(
                    patterns = index.pattern_count(),
                    "built and cached legacy Seiza blind index"
                );
                Arc::new(index)
            });
            let params = BlindParams {
                min_scale_arcsec_px: options.min_scale_arcsec_per_pixel,
                max_scale_arcsec_px: options.max_scale_arcsec_per_pixel,
                index_mag_limit: index.index_mag_limit(),
                max_pattern_deg: index.max_pattern_deg(),
                sip_order: options.sip_order,
                ..Default::default()
            };
            (
                solve_blind(&detected, catalog, index, &params, dimensions)
                    .context("blind Seiza solve failed")?,
                SolveMode::Blind,
                Some(index.pattern_count()),
            )
        }
    };
    let search_duration = search_started.elapsed();
    let (center_ra_deg, center_dec_deg) = solution
        .wcs
        .pixel_to_world(dimensions.0 as f64 / 2.0, dimensions.1 as f64 / 2.0);
    let footprint = solution
        .wcs
        .footprint(dimensions.0, dimensions.1)
        .map(|(ra, dec)| [ra, dec]);
    let total_duration = total_started.elapsed();
    let statistics = SolveStatistics {
        total_ms: duration_ms(total_duration),
        decode_ms: duration_ms(decode_duration),
        detection_ms: duration_ms(detection_duration),
        search_ms: duration_ms(search_duration),
        mode,
        detected_stars: detected.len(),
        catalog_stars: catalog.star_count(),
        blind_index_patterns,
        hint_source: options.hint_source,
        hint_keywords: options.hint_keywords.clone(),
    };
    tracing::info!(
        mode = ?statistics.mode,
        total_ms = statistics.total_ms,
        decode_ms = statistics.decode_ms,
        detection_ms = statistics.detection_ms,
        search_ms = statistics.search_ms,
        detected_stars = statistics.detected_stars,
        matched_stars = solution.matched_stars,
        "completed Seiza solve pipeline"
    );
    Ok(SolutionResponse {
        center_ra_deg,
        center_dec_deg,
        pixel_scale_arcsec_per_pixel: solution.wcs.scale_arcsec_per_px(),
        matched_stars: solution.matched_stars,
        rms_arcsec: solution.rms_arcsec,
        image_width: dimensions.0,
        image_height: dimensions.1,
        wcs: WcsResponse::from_seiza(&solution.wcs),
        footprint,
        objects: Vec::new(),
        catalog_version: None,
        capture_time: options.capture_time,
        statistics: Some(statistics),
    })
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

pub fn capture_time_from_bytes(
    bytes: &[u8],
    filename: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    fits_headers(bytes, filename)?
        .get("DATE-OBS")?
        .as_str()
        .and_then(parse_capture_time)
}

/// Promote acquisition metadata from a FITS header into solve options. User
/// supplied hints always win; automatic hinted solving is enabled only when a
/// complete center and pixel scale can be derived safely.
pub fn prepare_solve_options(options: &mut SolveOptions, bytes: &[u8], filename: &str) {
    options.hint_source = None;
    options.hint_keywords.clear();
    options.satellite_metadata_keywords.clear();
    let explicit_satellite_metadata = satellite_metadata_present(options);
    options.satellite_metadata_source =
        explicit_satellite_metadata.then_some(SatelliteMetadataSource::Explicit);

    let has_complete_explicit_hint = options.center_ra_deg.is_some()
        && options.center_dec_deg.is_some()
        && options.scale_arcsec_per_pixel.is_some();
    if has_complete_explicit_hint {
        options.hint_source = Some(SolveHintSource::Explicit);
    }

    let Some(headers) = fits_headers(bytes, filename) else {
        return;
    };
    prepare_satellite_metadata(options, &headers, explicit_satellite_metadata);
    if has_complete_explicit_hint
        || options.center_ra_deg.is_some()
        || options.center_dec_deg.is_some()
        || options.scale_arcsec_per_pixel.is_some()
    {
        return;
    }

    let Some((ra, dec, center_keywords)) = fits_center(&headers) else {
        return;
    };
    let Some((scale, scale_keywords)) = fits_pixel_scale(&headers) else {
        return;
    };
    if !(ra.is_finite()
        && (0.0..=360.0).contains(&ra)
        && dec.is_finite()
        && (-90.0..=90.0).contains(&dec)
        && scale.is_finite()
        && scale > 0.0)
    {
        return;
    }

    options.center_ra_deg = Some(ra);
    options.center_dec_deg = Some(dec);
    options.scale_arcsec_per_pixel = Some(scale);
    options.hint_source = Some(SolveHintSource::FitsHeader);
    options.hint_keywords = center_keywords
        .into_iter()
        .chain(scale_keywords)
        .map(str::to_owned)
        .collect();
}

fn satellite_metadata_present(options: &SolveOptions) -> bool {
    options.capture_time.is_some()
        || options.exposure_seconds.is_some()
        || options.observer_latitude_deg.is_some()
        || options.observer_longitude_deg.is_some()
        || options.observer_altitude_m.is_some()
        || options.observer_itrf_m.is_some()
}

fn prepare_satellite_metadata(
    options: &mut SolveOptions,
    headers: &BTreeMap<String, seiza_fits::HeaderValue>,
    explicit_satellite_metadata: bool,
) {
    let explicit_time = options.capture_time.is_some();
    let explicit_duration = options.exposure_seconds.is_some();
    let explicit_observer = options.observer_itrf_m.is_some()
        || options.observer_latitude_deg.is_some()
        || options.observer_longitude_deg.is_some();
    let header_duration = ["XPOSURE", "EXPTIME", "EXPOSURE"]
        .into_iter()
        .find_map(|key| {
            header_f64(headers, key)
                .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                .map(|seconds| (seconds, key))
        });
    let header_time_is_usable = headers
        .get("TIMESYS")
        .and_then(seiza_fits::HeaderValue::as_str)
        .is_none_or(|value| value.eq_ignore_ascii_case("UTC"))
        && headers
            .get("TREFPOS")
            .and_then(seiza_fits::HeaderValue::as_str)
            .is_none_or(|value| value.to_ascii_uppercase().starts_with("TOP"));

    if !explicit_duration && let Some((seconds, keyword)) = header_duration {
        options.exposure_seconds = Some(seconds);
        options.satellite_metadata_keywords.push(keyword.into());
    }
    if explicit_time
        && options.exposure_seconds.is_none()
        && let (Some(start), Some(end)) = (
            fits_time(headers, "DATE-BEG"),
            fits_time(headers, "DATE-END"),
        )
        && let Some(microseconds) = (end - start).num_microseconds()
        && microseconds > 0
    {
        options.exposure_seconds = Some(microseconds as f64 / 1e6);
        options
            .satellite_metadata_keywords
            .extend(["DATE-BEG", "DATE-END"].map(str::to_owned));
    }

    if !explicit_time && header_time_is_usable {
        let duration = options.exposure_seconds;
        let date_beg = fits_time(headers, "DATE-BEG");
        let date_end = fits_time(headers, "DATE-END");
        let date_avg = fits_time(headers, "DATE-AVG");
        let date_obs = fits_time(headers, "DATE-OBS");
        let resolved = if let (Some(start), Some(end)) = (date_beg, date_end) {
            let seconds = (end - start)
                .num_microseconds()
                .map(|value| value as f64 / 1e6);
            seconds
                .filter(|seconds| *seconds > 0.0)
                .map(|seconds| (start, seconds, vec!["DATE-BEG", "DATE-END"]))
        } else if let (Some(midpoint), Some(seconds)) = (date_avg, duration) {
            subtract_seconds(midpoint, seconds / 2.0)
                .map(|start| (start, seconds, vec!["DATE-AVG"]))
        } else if let (Some(start), Some(seconds)) = (date_obs.or(date_beg), duration) {
            let keyword = if date_obs.is_some() {
                "DATE-OBS"
            } else {
                "DATE-BEG"
            };
            Some((start, seconds, vec![keyword]))
        } else if let (Some(end), Some(seconds)) = (date_end, duration) {
            subtract_seconds(end, seconds).map(|start| (start, seconds, vec!["DATE-END"]))
        } else {
            None
        };
        if let Some((start, seconds, keywords)) = resolved {
            options.capture_time = Some(start);
            options.exposure_seconds = Some(seconds);
            for keyword in keywords {
                push_keyword(&mut options.satellite_metadata_keywords, keyword);
            }
        } else if let Some(capture_time) = date_obs {
            // A lone DATE-OBS is still useful for transient scoping and
            // minor-body propagation, even though it is insufficient for a
            // satellite track without one exposure duration.
            options.capture_time = Some(capture_time);
            push_keyword(&mut options.satellite_metadata_keywords, "DATE-OBS");
        }
    }

    if !explicit_observer {
        let itrf = [
            header_f64(headers, "OBSGEO-X"),
            header_f64(headers, "OBSGEO-Y"),
            header_f64(headers, "OBSGEO-Z"),
        ];
        if let [Some(x), Some(y), Some(z)] = itrf {
            options.observer_itrf_m = Some([x, y, z]);
            options
                .satellite_metadata_keywords
                .extend(["OBSGEO-X", "OBSGEO-Y", "OBSGEO-Z"].map(str::to_owned));
        } else if let (Some(latitude), Some(longitude), Some(altitude)) = (
            header_f64(headers, "OBSGEO-B"),
            header_f64(headers, "OBSGEO-L"),
            header_f64(headers, "OBSGEO-H"),
        ) {
            options.observer_latitude_deg = Some(latitude);
            options.observer_longitude_deg = Some(longitude);
            options.observer_altitude_m = Some(altitude);
            options
                .satellite_metadata_keywords
                .extend(["OBSGEO-B", "OBSGEO-L", "OBSGEO-H"].map(str::to_owned));
        } else if let (Some(latitude), Some(longitude)) = (
            header_f64(headers, "SITELAT"),
            header_f64(headers, "SITELONG"),
        ) {
            options.observer_latitude_deg = Some(latitude);
            options.observer_longitude_deg = Some(longitude);
            options.observer_altitude_m = Some(header_f64(headers, "SITEALT").unwrap_or(0.0));
            options
                .satellite_metadata_keywords
                .extend(["SITELAT", "SITELONG"].map(str::to_owned));
            if headers.contains_key("SITEALT") {
                options.satellite_metadata_keywords.push("SITEALT".into());
            }
        }
    }

    if !options.satellite_metadata_keywords.is_empty() {
        options.satellite_metadata_source = Some(if explicit_satellite_metadata {
            SatelliteMetadataSource::Explicit
        } else {
            SatelliteMetadataSource::FitsHeader
        });
    }
}

fn fits_time(
    headers: &BTreeMap<String, seiza_fits::HeaderValue>,
    key: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    headers
        .get(key)
        .and_then(seiza_fits::HeaderValue::as_str)
        .and_then(parse_capture_time)
}

fn subtract_seconds(
    time: chrono::DateTime<chrono::Utc>,
    seconds: f64,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if !seconds.is_finite() || seconds <= 0.0 || seconds > i64::MAX as f64 / 1e6 {
        return None;
    }
    time.checked_sub_signed(chrono::TimeDelta::microseconds(
        (seconds * 1e6).round() as i64
    ))
}

fn push_keyword(keywords: &mut Vec<String>, keyword: &str) {
    if !keywords.iter().any(|existing| existing == keyword) {
        keywords.push(keyword.to_owned());
    }
}

fn fits_headers(bytes: &[u8], filename: &str) -> Option<BTreeMap<String, seiza_fits::HeaderValue>> {
    let looks_like_fits = filename.rsplit('.').next().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("fits")
            || extension.eq_ignore_ascii_case("fit")
            || extension.eq_ignore_ascii_case("fts")
    }) || bytes.starts_with(b"SIMPLE  ");
    if !looks_like_fits || !bytes.starts_with(b"SIMPLE  ") {
        return None;
    }

    let mut headers = BTreeMap::new();
    for card in bytes.chunks_exact(80).take(FITS_HEADER_PROBE_BYTES / 80) {
        let keyword = std::str::from_utf8(&card[..8]).ok()?.trim();
        if keyword == "END" {
            return Some(headers);
        }
        if keyword.is_empty() || &card[8..10] != b"= " {
            continue;
        }
        let raw = std::str::from_utf8(&card[10..]).ok()?;
        headers.insert(keyword.to_owned(), seiza_fits::parse_header_value(raw));
    }
    None
}

fn fits_center(
    headers: &BTreeMap<String, seiza_fits::HeaderValue>,
) -> Option<(f64, f64, Vec<&'static str>)> {
    for (ra_key, dec_key) in [("CRVAL1", "CRVAL2"), ("RA", "DEC"), ("OBJCTRA", "OBJCTDEC")] {
        let Some(ra) = headers
            .get(ra_key)
            .and_then(|value| parse_fits_angle(value, true))
        else {
            continue;
        };
        let Some(dec) = headers
            .get(dec_key)
            .and_then(|value| parse_fits_angle(value, false))
        else {
            continue;
        };
        if (0.0..=360.0).contains(&ra) && (-90.0..=90.0).contains(&dec) {
            return Some((ra, dec, vec![ra_key, dec_key]));
        }
    }
    None
}

fn parse_fits_angle(value: &seiza_fits::HeaderValue, right_ascension: bool) -> Option<f64> {
    if let Some(value) = value.as_f64() {
        return Some(value);
    }
    let raw = value.as_str()?.trim();
    let normalized = raw.replace([':', 'h', 'H', 'd', 'D', 'm', 'M', 's', 'S'], " ");
    let components = normalized
        .split_whitespace()
        .map(str::parse::<f64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if !(2..=3).contains(&components.len()) {
        return None;
    }
    let sign = if raw.starts_with('-') { -1.0 } else { 1.0 };
    let mut angle = components[0].abs()
        + components[1].abs() / 60.0
        + components.get(2).copied().unwrap_or(0.0).abs() / 3_600.0;
    if right_ascension {
        angle *= 15.0;
    } else {
        angle *= sign;
    }
    Some(angle)
}

fn fits_pixel_scale(
    headers: &BTreeMap<String, seiza_fits::HeaderValue>,
) -> Option<(f64, Vec<&'static str>)> {
    for key in ["PIXSCALE", "SECPIX"] {
        if let Some(scale) = headers.get(key).and_then(seiza_fits::HeaderValue::as_f64)
            && scale.is_finite()
            && scale > 0.0
        {
            return Some((scale, vec![key]));
        }
    }

    if let (Some(cd11), Some(cd22)) = (header_f64(headers, "CD1_1"), header_f64(headers, "CD2_2")) {
        let cd12 = header_f64(headers, "CD1_2").unwrap_or(0.0);
        let cd21 = header_f64(headers, "CD2_1").unwrap_or(0.0);
        let scale = (cd11 * cd22 - cd12 * cd21).abs().sqrt() * 3_600.0;
        if scale.is_finite() && scale > 0.0 {
            let keywords = ["CD1_1", "CD1_2", "CD2_1", "CD2_2"]
                .into_iter()
                .filter(|key| headers.contains_key(*key))
                .collect();
            return Some((scale, keywords));
        }
    }

    if let (Some(cdelt1), Some(cdelt2)) =
        (header_f64(headers, "CDELT1"), header_f64(headers, "CDELT2"))
    {
        let scale = (cdelt1 * cdelt2).abs().sqrt() * 3_600.0;
        if scale.is_finite() && scale > 0.0 {
            return Some((scale, vec!["CDELT1", "CDELT2"]));
        }
    }

    let pixel_size_um = header_f64(headers, "XPIXSZ")?;
    let focal_length_mm = header_f64(headers, "FOCALLEN")?;
    let binning = header_f64(headers, "XBINNING").unwrap_or(1.0);
    let scale = 206.264_806_247 * pixel_size_um * binning / focal_length_mm;
    if scale.is_finite() && scale > 0.0 {
        let mut keywords = vec!["XPIXSZ", "FOCALLEN"];
        if headers.contains_key("XBINNING") {
            keywords.push("XBINNING");
        }
        return Some((scale, keywords));
    }
    None
}

fn header_f64(headers: &BTreeMap<String, seiza_fits::HeaderValue>, key: &str) -> Option<f64> {
    headers.get(key)?.as_f64()
}

pub fn parse_capture_time(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{NaiveDate, NaiveDateTime};
    let value = value.trim();
    if let Ok(value) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(value.with_timezone(&chrono::Utc));
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(value) = NaiveDateTime::parse_from_str(value.trim_end_matches('Z'), format) {
            return Some(value.and_utc());
        }
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|value| value.and_hms_opt(0, 0, 0))
        .map(|value| value.and_utc())
}

fn decode_image(bytes: &[u8], filename: &str) -> Result<image::DynamicImage> {
    if looks_like_fits(bytes, filename) {
        let fits = seiza_fits::FitsImage::from_bytes(bytes)
            .map_err(|error| anyhow::anyhow!("invalid FITS image: {error}"))?;
        let pixels = fits.stretch_to_u8(&seiza_fits::StretchParams::default());
        let buffer = image::GrayImage::from_raw(fits.width as u32, fits.height as u32, pixels)
            .context("FITS dimensions do not match decoded pixels")?;
        return Ok(image::DynamicImage::ImageLuma8(buffer));
    }
    image::load_from_memory(bytes)
        .context("unsupported or corrupt image; submit FITS, PNG, JPEG, TIFF, or WebP")
}

fn looks_like_fits(bytes: &[u8], filename: &str) -> bool {
    filename.rsplit('.').next().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("fits")
            || extension.eq_ignore_ascii_case("fit")
            || extension.eq_ignore_ascii_case("fts")
    }) || bytes.starts_with(b"SIMPLE  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fits_header(cards: &[&str]) -> Vec<u8> {
        let mut header = vec![b' '; 2_880];
        for (index, card) in cards.iter().enumerate() {
            header[index * 80..index * 80 + card.len()].copy_from_slice(card.as_bytes());
        }
        header
    }

    #[test]
    fn decodes_eight_bit_images_for_pixel_trail_alignment_without_changing_adu_scale() {
        let pixels = image::GrayImage::from_raw(2, 2, vec![0, 64, 128, 255]).unwrap();
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(pixels)
            .write_to(&mut encoded, ImageFormat::Png)
            .unwrap();

        let decoded = decode_monochrome_u16(encoded.get_ref(), "trail.png").unwrap();

        assert_eq!((decoded.width, decoded.height), (2, 2));
        assert_eq!(decoded.pixels, [0, 64 * 257, 128 * 257, u16::MAX]);
        assert_eq!(decoded.adu_per_stored_unit, 1.0 / 257.0);
    }

    #[test]
    fn reads_capture_time_from_fits_date_obs() {
        let header = fits_header(&[
            "SIMPLE  =                    T",
            "DATE-OBS= '2026-07-13T04:05:06.250Z'",
            "END",
        ]);
        assert_eq!(
            capture_time_from_bytes(&header, "capture.fits")
                .unwrap()
                .to_rfc3339(),
            "2026-07-13T04:05:06.250+00:00"
        );
    }

    #[test]
    fn parses_timezone_free_fits_timestamp_as_utc() {
        assert_eq!(
            parse_capture_time("2026-07-13T04:05:06")
                .unwrap()
                .to_rfc3339(),
            "2026-07-13T04:05:06+00:00"
        );
    }

    #[test]
    fn promotes_fits_coordinates_and_pixel_scale_to_a_hinted_solve() {
        let header = fits_header(&[
            "SIMPLE  =                    T",
            "RA      =          202.4695750",
            "DEC     =           47.1952580",
            "PIXSCALE=                 1.35",
            "DATE-OBS= '2026-07-13T04:05:06Z'",
            "END",
        ]);
        let mut options = SolveOptions::default();

        prepare_solve_options(&mut options, &header, "capture.fits");

        assert_eq!(options.center_ra_deg, Some(202.469575));
        assert_eq!(options.center_dec_deg, Some(47.195258));
        assert_eq!(options.scale_arcsec_per_pixel, Some(1.35));
        assert_eq!(options.hint_source, Some(SolveHintSource::FitsHeader));
        assert_eq!(options.hint_keywords, ["RA", "DEC", "PIXSCALE"]);
        assert!(options.capture_time.is_some());
    }

    #[test]
    fn promotes_single_exposure_bounds_and_geodetic_observer_from_fits() {
        let header = fits_header(&[
            "SIMPLE  =                    T",
            "DATE-BEG= '2026-07-19T04:05:06Z'",
            "DATE-END= '2026-07-19T04:05:36Z'",
            "OBSGEO-B=                 37.3",
            "OBSGEO-L=               -122.0",
            "OBSGEO-H=                 50.0",
            "END",
        ]);
        let mut options = SolveOptions::default();

        prepare_solve_options(&mut options, &header, "capture.fits");

        assert_eq!(
            options.capture_time.unwrap().to_rfc3339(),
            "2026-07-19T04:05:06+00:00"
        );
        assert_eq!(options.exposure_seconds, Some(30.0));
        assert_eq!(options.observer_latitude_deg, Some(37.3));
        assert_eq!(options.observer_longitude_deg, Some(-122.0));
        assert_eq!(options.observer_altitude_m, Some(50.0));
        assert_eq!(
            options.satellite_metadata_source,
            Some(SatelliteMetadataSource::FitsHeader)
        );
        assert_eq!(
            options.satellite_metadata_keywords,
            ["DATE-BEG", "DATE-END", "OBSGEO-B", "OBSGEO-L", "OBSGEO-H"]
        );
    }

    #[test]
    fn date_avg_is_normalized_to_shutter_open_time() {
        let header = fits_header(&[
            "SIMPLE  =                    T",
            "DATE-AVG= '2026-07-19T04:05:21Z'",
            "EXPTIME =                 30.0",
            "SITELAT =                 37.3",
            "SITELONG=               -122.0",
            "END",
        ]);
        let mut options = SolveOptions::default();

        prepare_solve_options(&mut options, &header, "capture.fits");

        assert_eq!(
            options.capture_time.unwrap().to_rfc3339(),
            "2026-07-19T04:05:06+00:00"
        );
        assert_eq!(options.exposure_seconds, Some(30.0));
        assert!(
            options
                .satellite_metadata_keywords
                .contains(&"DATE-AVG".into())
        );
    }

    #[test]
    fn non_utc_fits_time_is_not_used_for_satellite_prediction() {
        let header = fits_header(&[
            "SIMPLE  =                    T",
            "TIMESYS = 'TAI'",
            "DATE-OBS= '2026-07-19T04:05:06'",
            "EXPTIME =                 30.0",
            "SITELAT =                 37.3",
            "SITELONG=               -122.0",
            "END",
        ]);
        let mut options = SolveOptions::default();

        prepare_solve_options(&mut options, &header, "capture.fits");

        assert_eq!(options.capture_time, None);
        assert_eq!(options.exposure_seconds, Some(30.0));
        assert_eq!(options.observer_latitude_deg, Some(37.3));
    }

    #[test]
    fn derives_fits_hint_from_wcs_matrix() {
        let header = fits_header(&[
            "SIMPLE  =                    T",
            "CRVAL1  =                 10.5",
            "CRVAL2  =                -20.5",
            "CD1_1   =  -0.000277777777778",
            "CD1_2   =                  0.0",
            "CD2_1   =                  0.0",
            "CD2_2   =   0.000277777777778",
            "END",
        ]);
        let mut options = SolveOptions::default();

        prepare_solve_options(&mut options, &header, "solved.fit");

        assert_eq!(options.center_ra_deg, Some(10.5));
        assert_eq!(options.center_dec_deg, Some(-20.5));
        assert!((options.scale_arcsec_per_pixel.unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(options.hint_source, Some(SolveHintSource::FitsHeader));
        assert_eq!(
            options.hint_keywords,
            ["CRVAL1", "CRVAL2", "CD1_1", "CD1_2", "CD2_1", "CD2_2"]
        );
    }

    #[test]
    fn parses_sexagesimal_object_coordinates_and_camera_geometry() {
        let header = fits_header(&[
            "SIMPLE  =                    T",
            "OBJCTRA = '13:29:52.7'",
            "OBJCTDEC= '-47:11:43'",
            "XPIXSZ  =                 3.76",
            "FOCALLEN=                400.0",
            "XBINNING=                    2",
            "END",
        ]);
        let mut options = SolveOptions::default();

        prepare_solve_options(&mut options, &header, "capture.fts");

        assert!((options.center_ra_deg.unwrap() - 202.46958333333333).abs() < 1e-9);
        assert!((options.center_dec_deg.unwrap() + 47.195277777777775).abs() < 1e-9);
        assert!((options.scale_arcsec_per_pixel.unwrap() - 3.8777783574436).abs() < 1e-9);
        assert_eq!(options.hint_source, Some(SolveHintSource::FitsHeader));
    }

    #[test]
    fn explicit_hints_win_over_fits_metadata() {
        let header = fits_header(&[
            "SIMPLE  =                    T",
            "RA      =                 10.0",
            "DEC     =                 20.0",
            "PIXSCALE=                  1.0",
            "END",
        ]);
        let mut options = SolveOptions {
            center_ra_deg: Some(30.0),
            center_dec_deg: Some(40.0),
            scale_arcsec_per_pixel: Some(2.0),
            ..SolveOptions::default()
        };

        prepare_solve_options(&mut options, &header, "capture.fits");

        assert_eq!(options.center_ra_deg, Some(30.0));
        assert_eq!(options.center_dec_deg, Some(40.0));
        assert_eq!(options.scale_arcsec_per_pixel, Some(2.0));
        assert_eq!(options.hint_source, Some(SolveHintSource::Explicit));
        assert!(options.hint_keywords.is_empty());
    }

    #[test]
    fn fits_position_without_scale_remains_a_blind_solve() {
        let header = fits_header(&[
            "SIMPLE  =                    T",
            "RA      =                 10.0",
            "DEC     =                 20.0",
            "END",
        ]);
        let mut options = SolveOptions::default();

        prepare_solve_options(&mut options, &header, "capture.fits");

        assert_eq!(options.center_ra_deg, None);
        assert_eq!(options.center_dec_deg, None);
        assert_eq!(options.scale_arcsec_per_pixel, None);
        assert_eq!(options.hint_source, None);
    }
}
