use crate::models::{
    PixelCoordinatesResponse, SatelliteMetadataSource, SolutionResponse, SolveHintSource,
    SolveMode, SolveOptions, SolveStatistics, WcsResponse,
};
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use image::ImageFormat;
use seiza::{
    DetectConfig,
    blind::{BlindIndex, BlindParams, solve_blind},
    catalog::TileCatalog,
    detect_stars,
    raster::{CaptureTimeSource, PhotoMetadata, PixelCoordinates, ScaleSearch},
    solve::{SolveHint, solve},
};
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
    path::Path,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

pub const FITS_HEADER_PROBE_BYTES: usize = 80 * 1_440;
const XISF_PREAMBLE_BYTES: usize = 16;
const XISF_MAX_HEADER_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
enum ImageHeaderSource {
    Fits,
    Xisf,
}

impl ImageHeaderSource {
    const fn hint_source(self) -> SolveHintSource {
        match self {
            Self::Fits => SolveHintSource::FitsHeader,
            Self::Xisf => SolveHintSource::XisfHeader,
        }
    }

    const fn satellite_source(self) -> SatelliteMetadataSource {
        match self {
            Self::Fits => SatelliteMetadataSource::FitsHeader,
            Self::Xisf => SatelliteMetadataSource::XisfHeader,
        }
    }
}

/// EXIF tag that supplies the 35 mm-equivalent focal length behind a
/// blind-solve scale range.
const EXIF_FOCAL_LENGTH_TAG: &str = "FocalLengthIn35mmFilm";

/// The pixel frame an ordinary raster (JPEG, PNG, TIFF, WebP) is decoded in.
/// FITS and XISF images have no EXIF orientation and ignore it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFrame {
    /// The upright image a viewer shows, after the EXIF Orientation tag.
    Oriented,
    /// The pixel rows as stored in the file.
    Stored,
}

impl PixelFrame {
    /// The frame a job's solution was fitted in. Raster solutions saved
    /// before Seiza Server 0.5.0 carry no `pixel_coordinates` and used the
    /// stored rows; previews and pixel trails for them must keep that frame.
    /// A job without a solution is shown upright.
    pub fn for_solution(solution: Option<&SolutionResponse>) -> Self {
        match solution {
            Some(solution) if solution.pixel_coordinates.is_none() => Self::Stored,
            _ => Self::Oriented,
        }
    }
}

struct DecodedImage {
    pixels: image::DynamicImage,
    /// `None` for FITS and XISF images.
    coordinates: Option<PixelCoordinates>,
}

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

pub async fn preview_png(bytes: Bytes, filename: String, frame: PixelFrame) -> Result<Bytes> {
    encode_png(bytes, filename, frame, true).await
}

pub async fn full_png(bytes: Bytes, filename: String, frame: PixelFrame) -> Result<Bytes> {
    encode_png(bytes, filename, frame, false).await
}

async fn encode_png(
    bytes: Bytes,
    filename: String,
    frame: PixelFrame,
    thumbnail: bool,
) -> Result<Bytes> {
    tokio::task::spawn_blocking(move || {
        let image = decode_image(&bytes, &filename, frame)?.pixels;
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

/// Width and height of the image Seiza solves: after EXIF orientation for
/// an ordinary raster.
pub fn dimensions_from_bytes(bytes: &[u8], filename: &str) -> Result<(u32, u32)> {
    let image = decode_image(bytes, filename, PixelFrame::Oriented)?.pixels;
    Ok((image.width(), image.height()))
}

pub(crate) fn decode_monochrome_u16(
    bytes: &[u8],
    filename: &str,
    frame: PixelFrame,
) -> Result<MonochromeImage> {
    if let Some(fits) = decode_astronomy_image(bytes, filename)? {
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

    let image = decode_raster(bytes, frame)?.pixels;
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
    let DecodedImage {
        pixels: image,
        coordinates,
    } = decode_image(bytes, filename, PixelFrame::Oriented)?;
    // Only an ordinary raster carries EXIF; its focal length can narrow a
    // blind solve's scale range.
    let photo = if coordinates.is_some() {
        PhotoMetadata::from_bytes(bytes)
    } else {
        PhotoMetadata::default()
    };
    for warning in &photo.warnings {
        tracing::debug!(%warning, "photo metadata warning");
    }
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
    let blind = |options: &SolveOptions| {
        solve_blind_with_options(&detected, catalog, blind_index, options, dimensions, &photo)
    };
    let (solution, mode, blind_search) = match (
        options.center_ra_deg,
        options.center_dec_deg,
        options.scale_arcsec_per_pixel,
    ) {
        (Some(ra), Some(dec), Some(scale)) => match solve(
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
        ) {
            Ok(solution) => (solution, SolveMode::Hinted, None),
            Err(hinted_error) if automatic_hint_allows_blind_fallback(options.hint_source) => {
                tracing::warn!(
                    %hinted_error,
                    hint_source = ?options.hint_source,
                    "automatic header hint failed; retrying as a broad blind solve"
                );
                let (solution, search) = blind(options).with_context(|| {
                    format!("hinted Seiza solve failed: {hinted_error}; blind fallback also failed")
                })?;
                (solution, SolveMode::Blind, Some(search))
            }
            Err(error) => return Err(error).context("hinted Seiza solve failed"),
        },
        _ => {
            let (solution, search) = blind(options)?;
            (solution, SolveMode::Blind, Some(search))
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
    let (hint_source, hint_keywords) = match &blind_search {
        Some(search) if search.from_exif && options.hint_source.is_none() => (
            Some(SolveHintSource::Exif),
            vec![EXIF_FOCAL_LENGTH_TAG.to_owned()],
        ),
        _ => (options.hint_source, options.hint_keywords.clone()),
    };
    let statistics = SolveStatistics {
        total_ms: duration_ms(total_duration),
        decode_ms: duration_ms(decode_duration),
        detection_ms: duration_ms(detection_duration),
        search_ms: duration_ms(search_duration),
        mode,
        detected_stars: detected.len(),
        catalog_stars: catalog.star_count(),
        blind_index_patterns: blind_search.as_ref().map(|search| search.index_patterns),
        hint_source,
        hint_keywords,
        blind_scale_range: blind_search.as_ref().map(|search| search.range.into()),
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
        pixel_coordinates: coordinates.as_ref().map(PixelCoordinatesResponse::from),
    })
}

fn automatic_hint_allows_blind_fallback(hint_source: Option<SolveHintSource>) -> bool {
    matches!(
        hint_source,
        Some(SolveHintSource::FitsHeader | SolveHintSource::XisfHeader)
    )
}

/// How a blind solve found its answer.
#[derive(Debug, PartialEq)]
struct BlindSearch {
    index_patterns: usize,
    /// The pixel-scale range that solved, in arcseconds/pixel.
    range: (f64, f64),
    /// Whether that range came from the EXIF focal length.
    from_exif: bool,
}

/// The pixel-scale ranges to search, in order. Explicit bounds always hold;
/// a missing bound comes from a photo's EXIF focal length when it has one,
/// followed by a wide fallback in case the photo was cropped or shot
/// through an eyepiece. Without EXIF this is the single 0.1–20"/px default.
fn blind_scale_search(
    options: &SolveOptions,
    photo: &PhotoMetadata,
    dimensions: (u32, u32),
) -> Result<ScaleSearch> {
    ScaleSearch::new(
        photo,
        dimensions,
        options.min_scale_arcsec_per_pixel,
        options.max_scale_arcsec_per_pixel,
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}

/// Try each range in turn until one solves. Only a failure to find a
/// solution moves on to the next range; any other error is returned.
fn solve_over_ranges<T>(
    ranges: &[(f64, f64)],
    mut solve: impl FnMut((f64, f64)) -> std::result::Result<T, seiza::Error>,
) -> std::result::Result<(T, usize), seiza::Error> {
    let mut last = None;
    for (attempt, &range) in ranges.iter().enumerate() {
        if attempt > 0 {
            tracing::info!(
                min_scale = range.0,
                max_scale = range.1,
                "no blind solution in the EXIF focal-length scale range; retrying a wider range"
            );
        }
        match solve(range) {
            Ok(value) => return Ok((value, attempt)),
            Err(seiza::Error::Solve(message)) => last = Some(seiza::Error::Solve(message)),
            Err(error) => return Err(error),
        }
    }
    Err(last.unwrap_or_else(|| seiza::Error::Solve("no pixel-scale range to search".into())))
}

fn solve_blind_with_options(
    detected: &[seiza::DetectedStar],
    catalog: &TileCatalog,
    blind_index: &OnceLock<Arc<BlindIndex>>,
    options: &SolveOptions,
    dimensions: (u32, u32),
    photo: &PhotoMetadata,
) -> Result<(seiza::solve::Solution, BlindSearch)> {
    let search = blind_scale_search(options, photo, dimensions)?;
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
    let mut params = BlindParams {
        index_mag_limit: index.index_mag_limit(),
        max_pattern_deg: index.max_pattern_deg(),
        sip_order: options.sip_order,
        ..Default::default()
    };
    let (solution, attempt) = solve_over_ranges(&search.ranges, |(min, max)| {
        params.min_scale_arcsec_px = min;
        params.max_scale_arcsec_px = max;
        solve_blind(detected, catalog, index, &params, dimensions)
    })
    .context("blind Seiza solve failed")?;
    Ok((
        solution,
        BlindSearch {
            index_patterns: index.pattern_count(),
            range: search.ranges[attempt],
            from_exif: search.from_exif && attempt == 0,
        },
    ))
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

pub fn capture_time_from_bytes(
    bytes: &[u8],
    filename: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if looks_like_fits(bytes, filename) || looks_like_xisf(bytes, filename) {
        return image_headers(bytes, filename, bytes.len() as u64)?
            .0
            .get("DATE-OBS")?
            .as_str()
            .and_then(parse_capture_time);
    }
    PhotoMetadata::from_bytes(bytes)
        .capture_time_utc
        .as_deref()
        .and_then(parse_capture_time)
}

/// Promote acquisition metadata from an astronomy image header into solve options. User
/// supplied hints always win; automatic hinted solving is enabled only when a
/// complete center and pixel scale can be derived safely.
pub fn prepare_solve_options(options: &mut SolveOptions, bytes: &[u8], filename: &str) {
    prepare_solve_options_from_prefix(options, bytes, filename, bytes.len() as u64);
}

/// Promote image metadata when `bytes` may contain only the header prefix of a
/// larger upload.
pub fn prepare_solve_options_from_prefix(
    options: &mut SolveOptions,
    bytes: &[u8],
    filename: &str,
    file_size: u64,
) {
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

    if !looks_like_fits(bytes, filename) && !looks_like_xisf(bytes, filename) {
        // An ordinary raster. Its EXIF sits at the start of the file, inside
        // the probed prefix. The focal length is applied later, at solve
        // time, once the oriented dimensions are known.
        prepare_exif_metadata(
            options,
            &PhotoMetadata::from_bytes(bytes),
            explicit_satellite_metadata,
        );
        return;
    }
    let Some((headers, header_source)) = image_headers(bytes, filename, file_size) else {
        return;
    };
    prepare_satellite_metadata(
        options,
        &headers,
        header_source,
        explicit_satellite_metadata,
    );
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
    options.hint_source = Some(header_source.hint_source());
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
    header_source: ImageHeaderSource,
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
            header_source.satellite_source()
        });
    }
}

/// Promote a photo's EXIF capture time and GPS position. Explicit values
/// always win. ExposureTime and GPSAltitude are never used: a phone may
/// store a multi-frame composite whose ExposureTime is not one shutter-open
/// interval, and EXIF altitude is not an ellipsoid height.
fn prepare_exif_metadata(
    options: &mut SolveOptions,
    photo: &PhotoMetadata,
    explicit_satellite_metadata: bool,
) {
    if options.capture_time.is_none()
        && let Some(time) = photo
            .capture_time_utc
            .as_deref()
            .and_then(parse_capture_time)
    {
        options.capture_time = Some(time);
        let tags: &[&str] = match photo.capture_time_source {
            Some(CaptureTimeSource::DateTimeOriginal) if photo.sub_sec_time_original.is_some() => {
                &[
                    "DateTimeOriginal",
                    "SubSecTimeOriginal",
                    "OffsetTimeOriginal",
                ]
            }
            Some(CaptureTimeSource::DateTimeOriginal) => {
                &["DateTimeOriginal", "OffsetTimeOriginal"]
            }
            Some(CaptureTimeSource::Gps) | None => &["GPSDateStamp", "GPSTimeStamp"],
        };
        for tag in tags {
            push_keyword(&mut options.satellite_metadata_keywords, tag);
        }
    }

    let explicit_observer = options.observer_itrf_m.is_some()
        || options.observer_latitude_deg.is_some()
        || options.observer_longitude_deg.is_some();
    if !explicit_observer
        && let (Some(latitude), Some(longitude)) = (photo.gps_latitude_deg, photo.gps_longitude_deg)
    {
        options.observer_latitude_deg = Some(latitude);
        options.observer_longitude_deg = Some(longitude);
        for tag in [
            "GPSLatitude",
            "GPSLatitudeRef",
            "GPSLongitude",
            "GPSLongitudeRef",
        ] {
            push_keyword(&mut options.satellite_metadata_keywords, tag);
        }
    }

    if !options.satellite_metadata_keywords.is_empty() {
        options.satellite_metadata_source = Some(if explicit_satellite_metadata {
            SatelliteMetadataSource::Explicit
        } else {
            SatelliteMetadataSource::Exif
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

fn image_headers(
    bytes: &[u8],
    filename: &str,
    file_size: u64,
) -> Option<(BTreeMap<String, seiza_fits::HeaderValue>, ImageHeaderSource)> {
    if looks_like_xisf(bytes, filename) {
        let header_bytes = xisf_header_prefix_bytes(bytes)?;
        if bytes.len() < header_bytes || file_size < header_bytes as u64 {
            return None;
        }
        let mut temporary = tempfile::NamedTempFile::new().ok()?;
        temporary.write_all(&bytes[..header_bytes]).ok()?;
        temporary.flush().ok()?;
        temporary.as_file_mut().set_len(file_size).ok()?;
        let headers = seiza_xisf::read_header(temporary.path()).ok()?;
        return Some((headers.into_iter().collect(), ImageHeaderSource::Xisf));
    }

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
            return Some((headers, ImageHeaderSource::Fits));
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
    // XPIXSZ is the image pixel width after binning. Capture programs such as
    // N.I.N.A. therefore write both XPIXSZ=4.63 and XBINNING=2 for the native
    // 4144x2822 mode of an ASI294MM. Multiplying the two again doubles the
    // scale hint and can leave an otherwise valid field outside the solver's
    // tolerance.
    let scale = 206.264_806_247 * pixel_size_um / focal_length_mm;
    if scale.is_finite() && scale > 0.0 {
        return Some((scale, vec!["XPIXSZ", "FOCALLEN"]));
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

fn decode_image(bytes: &[u8], filename: &str, frame: PixelFrame) -> Result<DecodedImage> {
    if let Some(fits) = decode_astronomy_image(bytes, filename)? {
        let pixels = fits.stretch_to_u8(&seiza_fits::StretchParams::default());
        let buffer = image::GrayImage::from_raw(fits.width as u32, fits.height as u32, pixels)
            .context("FITS dimensions do not match decoded pixels")?;
        return Ok(DecodedImage {
            pixels: image::DynamicImage::ImageLuma8(buffer),
            coordinates: None,
        });
    }
    decode_raster(bytes, frame)
}

/// Decode an ordinary raster. Every raster decode goes through here so the
/// solve, footprint, overlays, previews and satellite-trail pixels share
/// one frame.
fn decode_raster(bytes: &[u8], frame: PixelFrame) -> Result<DecodedImage> {
    const UNSUPPORTED: &str =
        "unsupported or corrupt image; submit FITS, XISF, PNG, JPEG, TIFF, or WebP";
    match frame {
        PixelFrame::Oriented => {
            let raster = seiza::raster::decode_oriented(bytes)
                .map_err(|error| anyhow::anyhow!("{error}"))
                .context(UNSUPPORTED)?;
            Ok(DecodedImage {
                pixels: raster.pixels,
                coordinates: Some(raster.coordinates),
            })
        }
        PixelFrame::Stored => Ok(DecodedImage {
            pixels: image::load_from_memory(bytes).context(UNSUPPORTED)?,
            coordinates: None,
        }),
    }
}

fn decode_astronomy_image(bytes: &[u8], filename: &str) -> Result<Option<seiza_fits::FitsImage>> {
    if looks_like_fits(bytes, filename) {
        return seiza_fits::FitsImage::from_bytes(bytes)
            .map(Some)
            .map_err(|error| anyhow::anyhow!("invalid FITS image: {error}"));
    }
    if looks_like_xisf(bytes, filename) {
        return seiza_xisf::from_bytes(bytes)
            .map(Some)
            .map_err(|error| anyhow::anyhow!("invalid XISF image: {error}"));
    }
    Ok(None)
}

fn looks_like_fits(bytes: &[u8], filename: &str) -> bool {
    filename.rsplit('.').next().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("fits")
            || extension.eq_ignore_ascii_case("fit")
            || extension.eq_ignore_ascii_case("fts")
    }) || bytes.starts_with(b"SIMPLE  ")
}

fn looks_like_xisf(bytes: &[u8], filename: &str) -> bool {
    filename
        .rsplit('.')
        .next()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xisf"))
        || bytes.starts_with(b"XISF0100")
}

fn xisf_header_prefix_bytes(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < XISF_PREAMBLE_BYTES || !bytes.starts_with(b"XISF0100") {
        return None;
    }
    let header_bytes = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
    if header_bytes == 0 || header_bytes > XISF_MAX_HEADER_BYTES {
        return None;
    }
    XISF_PREAMBLE_BYTES.checked_add(header_bytes)
}

pub fn image_header_probe_bytes(bytes: &[u8], filename: &str) -> usize {
    if looks_like_xisf(bytes, filename) {
        xisf_header_prefix_bytes(bytes).unwrap_or(XISF_PREAMBLE_BYTES)
    } else {
        FITS_HEADER_PROBE_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_automatic_header_hints_fall_back_to_blind_solving() {
        assert!(automatic_hint_allows_blind_fallback(Some(
            SolveHintSource::FitsHeader
        )));
        assert!(automatic_hint_allows_blind_fallback(Some(
            SolveHintSource::XisfHeader
        )));
        assert!(!automatic_hint_allows_blind_fallback(Some(
            SolveHintSource::Explicit
        )));
        assert!(!automatic_hint_allows_blind_fallback(None));
    }

    fn fits_header(cards: &[&str]) -> Vec<u8> {
        let mut header = vec![b' '; 2_880];
        for (index, card) in cards.iter().enumerate() {
            header[index * 80..index * 80 + card.len()].copy_from_slice(card.as_bytes());
        }
        header
    }

    fn xisf_image(headers: &[seiza_fits::WriteHeaderCard]) -> Vec<u8> {
        let mut encoded = Vec::new();
        seiza_xisf::write_f32_image_to(
            &mut encoded,
            2,
            2,
            seiza_fits::F32ImageData::Mono(&[0.0, 0.25, 0.5, 1.0]),
            headers,
        )
        .unwrap();
        encoded
    }

    #[test]
    fn decodes_eight_bit_images_for_pixel_trail_alignment_without_changing_adu_scale() {
        let pixels = image::GrayImage::from_raw(2, 2, vec![0, 64, 128, 255]).unwrap();
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(pixels)
            .write_to(&mut encoded, ImageFormat::Png)
            .unwrap();

        let decoded =
            decode_monochrome_u16(encoded.get_ref(), "trail.png", PixelFrame::Oriented).unwrap();

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
    fn decodes_xisf_pixels_and_dimensions() {
        let encoded = xisf_image(&[]);

        assert_eq!(
            dimensions_from_bytes(&encoded, "capture.xisf").unwrap(),
            (2, 2)
        );
        let decoded =
            decode_monochrome_u16(&encoded, "capture.xisf", PixelFrame::Oriented).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 2));
        assert_eq!(decoded.pixels, [0, 16_383, 32_767, u16::MAX]);
    }

    #[test]
    fn promotes_xisf_metadata_to_a_hinted_solve() {
        use seiza_fits::{HeaderValue, WriteHeaderCard};

        let encoded = xisf_image(&[
            WriteHeaderCard::new("RA", HeaderValue::Float(202.469575)),
            WriteHeaderCard::new("DEC", HeaderValue::Float(47.195258)),
            WriteHeaderCard::new("PIXSCALE", HeaderValue::Float(1.35)),
            WriteHeaderCard::new(
                "DATE-OBS",
                HeaderValue::String("2026-07-13T04:05:06Z".into()),
            ),
            WriteHeaderCard::new("EXPTIME", HeaderValue::Float(30.0)),
            WriteHeaderCard::new("OBSGEO-B", HeaderValue::Float(37.3)),
            WriteHeaderCard::new("OBSGEO-L", HeaderValue::Float(-122.0)),
            WriteHeaderCard::new("OBSGEO-H", HeaderValue::Float(0.0)),
        ]);
        let header_bytes =
            image_header_probe_bytes(&encoded[..XISF_PREAMBLE_BYTES], "capture.xisf");
        assert!(header_bytes < encoded.len());
        let mut options = SolveOptions::default();

        prepare_solve_options_from_prefix(
            &mut options,
            &encoded[..header_bytes],
            "capture.xisf",
            encoded.len() as u64,
        );

        assert_eq!(options.center_ra_deg, Some(202.469575));
        assert_eq!(options.center_dec_deg, Some(47.195258));
        assert_eq!(options.scale_arcsec_per_pixel, Some(1.35));
        assert_eq!(options.hint_source, Some(SolveHintSource::XisfHeader));
        assert_eq!(options.hint_keywords, ["RA", "DEC", "PIXSCALE"]);
        assert_eq!(options.exposure_seconds, Some(30.0));
        assert_eq!(options.observer_latitude_deg, Some(37.3));
        assert_eq!(options.observer_longitude_deg, Some(-122.0));
        assert_eq!(
            options.satellite_metadata_source,
            Some(SatelliteMetadataSource::XisfHeader)
        );
    }

    /// A 32x16 grey JPEG, bright in its stored top-left 8x8 corner, with
    /// `fields` as EXIF.
    fn exif_jpeg(fields: &[seiza::raster::test_support::Field]) -> Vec<u8> {
        let pixels = image::GrayImage::from_fn(32, 16, |x, y| {
            image::Luma([if x < 8 && y < 8 { 250 } else { 10 }])
        });
        seiza::raster::test_support::jpeg_with_exif(
            &image::DynamicImage::ImageLuma8(pixels),
            fields,
        )
    }

    fn rotated_jpeg() -> Vec<u8> {
        use seiza::raster::test_support::{Tag, Value, field};
        // Orientation 6: display the stored rows rotated 90° clockwise.
        exif_jpeg(&[field(Tag::Orientation, Value::Short(vec![6]))])
    }

    fn phone_jpeg(extra: &[seiza::raster::test_support::Field]) -> Vec<u8> {
        use seiza::raster::test_support::{Tag, Value, ascii, field, rationals};
        let mut fields = vec![
            ascii(Tag::DateTimeOriginal, "2026:10:03 19:08:11"),
            ascii(Tag::SubSecTimeOriginal, "026"),
            ascii(Tag::OffsetTimeOriginal, "-07:00"),
            field(Tag::GPSLatitude, rationals(&[(37, 1), (18, 1), (0, 1)])),
            ascii(Tag::GPSLatitudeRef, "N"),
            field(Tag::GPSLongitude, rationals(&[(122, 1), (1, 1), (30, 1)])),
            ascii(Tag::GPSLongitudeRef, "W"),
            field(Tag::GPSAltitude, rationals(&[(120, 1)])),
            field(Tag::GPSAltitudeRef, Value::Byte(vec![0])),
            field(Tag::ExposureTime, rationals(&[(1, 10)])),
            field(Tag::FocalLengthIn35mmFilm, Value::Short(vec![26])),
        ];
        fields.extend_from_slice(extra);
        exif_jpeg(&fields)
    }

    #[test]
    fn raster_decodes_apply_exif_orientation() {
        let jpeg = rotated_jpeg();

        assert_eq!(dimensions_from_bytes(&jpeg, "phone.jpg").unwrap(), (16, 32));
        let decoded = decode_image(&jpeg, "phone.jpg", PixelFrame::Oriented).unwrap();
        assert_eq!(
            decoded
                .coordinates
                .as_ref()
                .map(PixelCoordinatesResponse::from),
            Some(PixelCoordinatesResponse {
                original_dimensions: [32, 16],
                oriented_dimensions: [16, 32],
                orientation_applied: 6,
                convention: "zero-based pixel centers in the EXIF-oriented image".into(),
            })
        );

        // The stored top-left corner is the displayed top-right corner, in the
        // pixels used for satellite-trail alignment as well.
        let upright = decode_monochrome_u16(&jpeg, "phone.jpg", PixelFrame::Oriented).unwrap();
        assert_eq!((upright.width, upright.height), (16, 32));
        let at = |image: &MonochromeImage, x: usize, y: usize| image.pixels[y * image.width + x];
        assert!(at(&upright, 12, 4) > 50_000, "{}", at(&upright, 12, 4));
        assert!(at(&upright, 3, 4) < 10_000, "{}", at(&upright, 3, 4));

        let stored = decode_monochrome_u16(&jpeg, "phone.jpg", PixelFrame::Stored).unwrap();
        assert_eq!((stored.width, stored.height), (32, 16));
        assert!(at(&stored, 4, 4) > 50_000);
        assert!(
            decode_image(&jpeg, "phone.jpg", PixelFrame::Stored)
                .unwrap()
                .coordinates
                .is_none()
        );
    }

    #[tokio::test]
    async fn previews_use_the_frame_the_solution_was_fitted_in() {
        let jpeg = Bytes::from(rotated_jpeg());
        let size = |png: Bytes| {
            let image = image::load_from_memory(&png).unwrap();
            (image.width(), image.height())
        };

        let upright = preview_png(jpeg.clone(), "phone.jpg".into(), PixelFrame::Oriented)
            .await
            .unwrap();
        // Previews fit 1,800 px, keeping the upright portrait shape.
        assert_eq!(size(upright), (900, 1_800));
        let full = full_png(jpeg.clone(), "phone.jpg".into(), PixelFrame::Oriented)
            .await
            .unwrap();
        assert_eq!(size(full), (16, 32));
        let legacy = preview_png(jpeg, "phone.jpg".into(), PixelFrame::Stored)
            .await
            .unwrap();
        assert_eq!(size(legacy), (1_800, 900));
    }

    #[test]
    fn legacy_raster_solutions_keep_the_stored_frame() {
        let mut solution: SolutionResponse = serde_json::from_value(serde_json::json!({
            "center_ra_deg": 10.0,
            "center_dec_deg": 20.0,
            "pixel_scale_arcsec_per_pixel": 1.0,
            "matched_stars": 12,
            "rms_arcsec": 0.2,
            "image_width": 32,
            "image_height": 16,
            "wcs": {"crval": [10.0, 20.0], "crpix": [16.0, 8.0], "cd": [[0.001, 0.0], [0.0, 0.001]]},
        }))
        .unwrap();

        assert_eq!(PixelFrame::for_solution(None), PixelFrame::Oriented);
        assert_eq!(
            PixelFrame::for_solution(Some(&solution)),
            PixelFrame::Stored
        );
        solution.pixel_coordinates =
            decode_image(&rotated_jpeg(), "phone.jpg", PixelFrame::Oriented)
                .unwrap()
                .coordinates
                .as_ref()
                .map(PixelCoordinatesResponse::from);
        assert_eq!(
            PixelFrame::for_solution(Some(&solution)),
            PixelFrame::Oriented
        );
        let encoded = serde_json::to_value(&solution).unwrap();
        assert_eq!(encoded["pixel_coordinates"]["orientation_applied"], 6);
        assert_eq!(
            encoded["pixel_coordinates"]["original_dimensions"],
            serde_json::json!([32, 16])
        );
    }

    #[test]
    fn promotes_exif_capture_time_and_gps_but_not_exposure_or_altitude() {
        let jpeg = phone_jpeg(&[]);
        let mut options = SolveOptions::default();

        prepare_solve_options(&mut options, &jpeg, "phone.jpg");

        assert_eq!(
            options.capture_time.unwrap().to_rfc3339(),
            "2026-10-04T02:08:11.026+00:00"
        );
        assert!((options.observer_latitude_deg.unwrap() - 37.3).abs() < 1e-9);
        assert!((options.observer_longitude_deg.unwrap() + 122.025).abs() < 1e-9);
        // A phone may merge several frames; ExposureTime is not one
        // shutter-open interval, and EXIF altitude is not an ellipsoid height.
        assert_eq!(options.exposure_seconds, None);
        assert_eq!(options.observer_altitude_m, None);
        assert_eq!(
            options.satellite_metadata_source,
            Some(SatelliteMetadataSource::Exif)
        );
        assert_eq!(
            options.satellite_metadata_keywords,
            [
                "DateTimeOriginal",
                "SubSecTimeOriginal",
                "OffsetTimeOriginal",
                "GPSLatitude",
                "GPSLatitudeRef",
                "GPSLongitude",
                "GPSLongitudeRef",
            ]
        );
        // The focal length informs the blind scale range at solve time, not
        // a hinted solve.
        assert_eq!(options.hint_source, None);
        assert_eq!(options.scale_arcsec_per_pixel, None);
        options.validate().unwrap();

        // An upload prepared from its probed prefix reads the same EXIF.
        let mut from_prefix = SolveOptions::default();
        let probe = image_header_probe_bytes(&jpeg, "phone.jpg").min(jpeg.len());
        prepare_solve_options_from_prefix(
            &mut from_prefix,
            &jpeg[..probe],
            "phone.jpg",
            jpeg.len() as u64,
        );
        assert_eq!(from_prefix.capture_time, options.capture_time);
        assert_eq!(
            capture_time_from_bytes(&jpeg, "phone.jpg"),
            options.capture_time
        );
    }

    #[test]
    fn exif_gps_time_is_used_when_the_offset_is_missing() {
        use seiza::raster::test_support::{Tag, ascii, field, rationals};
        let jpeg = exif_jpeg(&[
            ascii(Tag::DateTimeOriginal, "2026:10:03 19:08:11"),
            ascii(Tag::GPSDateStamp, "2026:10:04"),
            field(Tag::GPSTimeStamp, rationals(&[(2, 1), (8, 1), (9, 1)])),
        ]);
        let mut options = SolveOptions::default();

        prepare_solve_options(&mut options, &jpeg, "phone.jpg");

        assert_eq!(
            options.capture_time.unwrap().to_rfc3339(),
            "2026-10-04T02:08:09+00:00"
        );
        assert_eq!(
            options.satellite_metadata_keywords,
            ["GPSDateStamp", "GPSTimeStamp"]
        );
        assert_eq!(options.observer_latitude_deg, None);
    }

    #[test]
    fn explicit_time_and_site_win_over_exif() {
        let jpeg = phone_jpeg(&[]);
        let explicit_time = parse_capture_time("2026-10-04T03:00:00Z");
        let mut options = SolveOptions {
            capture_time: explicit_time,
            observer_latitude_deg: Some(-33.9),
            observer_longitude_deg: Some(18.4),
            ..SolveOptions::default()
        };

        prepare_solve_options(&mut options, &jpeg, "phone.jpg");

        assert_eq!(options.capture_time, explicit_time);
        assert_eq!(options.observer_latitude_deg, Some(-33.9));
        assert_eq!(options.observer_longitude_deg, Some(18.4));
        assert!(options.satellite_metadata_keywords.is_empty());
        assert_eq!(
            options.satellite_metadata_source,
            Some(SatelliteMetadataSource::Explicit)
        );

        // A site given as ITRF coordinates also keeps GPS out, while the EXIF
        // time still fills the missing capture time.
        let mut options = SolveOptions {
            observer_itrf_m: Some([-2_700_000.0, -4_300_000.0, 3_850_000.0]),
            ..SolveOptions::default()
        };
        prepare_solve_options(&mut options, &jpeg, "phone.jpg");
        assert_eq!(options.observer_latitude_deg, None);
        assert!(options.capture_time.is_some());
        assert_eq!(
            options.satellite_metadata_keywords,
            [
                "DateTimeOriginal",
                "SubSecTimeOriginal",
                "OffsetTimeOriginal"
            ]
        );
        assert_eq!(
            options.satellite_metadata_source,
            Some(SatelliteMetadataSource::Explicit)
        );
        options.validate().unwrap();
    }

    #[test]
    fn exif_focal_length_plans_a_narrow_scale_range_then_a_wide_fallback() {
        let photo = PhotoMetadata::from_bytes(&phone_jpeg(&[]));
        let dimensions = (4_032, 3_024);
        let hint = photo.scale_hint(dimensions).unwrap();

        let search = blind_scale_search(&SolveOptions::default(), &photo, dimensions).unwrap();
        assert!(search.from_exif);
        assert_eq!(
            search.ranges,
            [
                (hint.min_arcsec_per_pixel, hint.max_arcsec_per_pixel),
                (0.1, hint.max_arcsec_per_pixel.max(20.0)),
            ]
        );
        // A 26 mm-equivalent phone field is far coarser than telescope images.
        assert!(hint.min_arcsec_per_pixel > 20.0, "{hint:?}");

        // Bounds a client sends always hold, and both replace the EXIF range.
        let explicit = SolveOptions {
            min_scale_arcsec_per_pixel: Some(0.5),
            max_scale_arcsec_per_pixel: Some(2.0),
            ..SolveOptions::default()
        };
        let search = blind_scale_search(&explicit, &photo, dimensions).unwrap();
        assert_eq!(
            (search.ranges.as_slice(), search.from_exif),
            (&[(0.5, 2.0)][..], false)
        );

        // An explicit bound that contradicts the EXIF range drops it.
        let conflicting = SolveOptions {
            max_scale_arcsec_per_pixel: Some(5.0),
            ..SolveOptions::default()
        };
        let search = blind_scale_search(&conflicting, &photo, dimensions).unwrap();
        assert_eq!(
            (search.ranges.as_slice(), search.from_exif),
            (&[(0.1, 5.0)][..], false)
        );

        // Without EXIF (FITS, XISF, or a bare PNG) the old default holds.
        let none = PhotoMetadata::default();
        let search = blind_scale_search(&SolveOptions::default(), &none, dimensions).unwrap();
        assert_eq!(
            (search.ranges.as_slice(), search.from_exif),
            (&[(0.1, 20.0)][..], false)
        );
        let min_only = SolveOptions {
            min_scale_arcsec_per_pixel: Some(1.0),
            ..SolveOptions::default()
        };
        let search = blind_scale_search(&min_only, &none, dimensions).unwrap();
        assert_eq!(search.ranges, [(1.0, 20.0)]);
        let too_coarse = SolveOptions {
            min_scale_arcsec_per_pixel: Some(30.0),
            ..SolveOptions::default()
        };
        assert!(blind_scale_search(&too_coarse, &none, dimensions).is_err());
    }

    #[test]
    fn scale_ranges_are_retried_only_when_no_solution_is_found() {
        let ranges = [(30.0, 120.0), (0.1, 120.0)];
        let mut tried = Vec::new();
        let (solved, attempt) = solve_over_ranges(&ranges, |range| {
            tried.push(range);
            if tried.len() == 1 {
                Err(seiza::Error::Solve("no match".into()))
            } else {
                Ok("solved")
            }
        })
        .unwrap();
        assert_eq!((solved, attempt), ("solved", 1));
        assert_eq!(tried, ranges);

        let mut calls = 0;
        let error = solve_over_ranges(&ranges, |_| -> std::result::Result<(), _> {
            calls += 1;
            Err(seiza::Error::Catalog("unreadable tile".into()))
        })
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(matches!(error, seiza::Error::Catalog(_)));

        let error = solve_over_ranges(&ranges, |_| -> std::result::Result<(), _> {
            Err(seiza::Error::Solve("no match".into()))
        })
        .unwrap_err();
        assert!(matches!(error, seiza::Error::Solve(_)));
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
    fn treats_xpixsz_as_the_effective_binned_pixel_size() {
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
        assert!((options.scale_arcsec_per_pixel.unwrap() - 1.9388891787218).abs() < 1e-9);
        assert_eq!(options.hint_source, Some(SolveHintSource::FitsHeader));
        assert_eq!(
            options.hint_keywords,
            ["OBJCTRA", "OBJCTDEC", "XPIXSZ", "FOCALLEN"]
        );
    }

    #[test]
    fn derives_nina_asi294_scale_without_double_counting_binning() {
        let header = fits_header(&[
            "SIMPLE  =                    T",
            "NAXIS1  =                 4144",
            "NAXIS2  =                 2822",
            "RA      =     315.147297524169",
            "DEC     =     45.1289918743763",
            "XPIXSZ  =                 4.63",
            "YPIXSZ  =                 4.63",
            "XBINNING=                    2",
            "YBINNING=                    2",
            "FOCALLEN=               1000.0",
            "INSTRUME= 'ZWO ASI294MM Pro'",
            "SWCREATE= 'N.I.N.A. 3.2.0.9001 (x64)'",
            "END",
        ]);
        let mut options = SolveOptions::default();

        prepare_solve_options(&mut options, &header, "capture.fits");

        assert!((options.scale_arcsec_per_pixel.unwrap() - 0.955_006_052_923_61).abs() < 1e-12);
        assert_eq!(options.hint_source, Some(SolveHintSource::FitsHeader));
        assert_eq!(options.hint_keywords, ["RA", "DEC", "XPIXSZ", "FOCALLEN"]);
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
