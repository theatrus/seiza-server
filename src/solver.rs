use crate::models::{SolutionResponse, SolveOptions, WcsResponse};
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
use std::{io::Cursor, path::Path, sync::Arc};

#[derive(Clone)]
pub struct SolverEngine {
    catalog: Option<Arc<TileCatalog>>,
}

impl SolverEngine {
    pub fn from_catalog_path(star_path: Option<&Path>) -> Self {
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
        Self { catalog }
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
        tokio::task::spawn_blocking(move || solve_bytes(&catalog, &bytes, &filename, &options))
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

fn solve_bytes(
    catalog: &TileCatalog,
    bytes: &[u8],
    filename: &str,
    options: &SolveOptions,
) -> Result<SolutionResponse> {
    options.validate().map_err(anyhow::Error::msg)?;
    let image = decode_image(bytes, filename)?;
    let dimensions = (image.width(), image.height());
    if dimensions.0 == 0 || dimensions.1 == 0 {
        bail!("image has invalid dimensions");
    }
    let detected = detect_stars(
        &image,
        &DetectConfig {
            sigma: options.sigma,
            ignore_border: options.ignore_border,
            max_stars: options.max_stars.clamp(16, 2_000),
            ..Default::default()
        },
    );
    tracing::info!(
        stars = detected.len(),
        width = dimensions.0,
        height = dimensions.1,
        "detected stars for queued solve"
    );

    let solution = match (
        options.center_ra_deg,
        options.center_dec_deg,
        options.scale_arcsec_per_pixel,
    ) {
        (Some(ra), Some(dec), Some(scale)) => solve(
            &detected,
            catalog,
            &SolveHint {
                center: (ra, dec),
                radius_deg: options.radius_deg.unwrap_or(2.0).clamp(0.1, 180.0),
                scale_arcsec_px: scale,
                scale_tolerance: options.scale_tolerance,
            },
            dimensions,
        )
        .context("hinted Seiza solve failed")?,
        _ => {
            let params = BlindParams {
                min_scale_arcsec_px: options.min_scale_arcsec_per_pixel,
                max_scale_arcsec_px: options.max_scale_arcsec_per_pixel,
                ..Default::default()
            };
            // The index is tied to the accepted scale range. It is created in
            // the bounded worker rather than on the request path, ensuring a
            // blind job cannot make HTTP handling interactive.
            let index = BlindIndex::build(catalog, &params);
            solve_blind(&detected, catalog, &index, &params, dimensions)
                .context("blind Seiza solve failed")?
        }
    };
    let (center_ra_deg, center_dec_deg) = solution
        .wcs
        .pixel_to_world(dimensions.0 as f64 / 2.0, dimensions.1 as f64 / 2.0);
    let footprint = solution
        .wcs
        .footprint(dimensions.0, dimensions.1)
        .map(|(ra, dec)| [ra, dec]);
    Ok(SolutionResponse {
        center_ra_deg,
        center_dec_deg,
        pixel_scale_arcsec_per_pixel: solution.wcs.scale_arcsec_per_px(),
        matched_stars: solution.matched_stars,
        rms_arcsec: solution.rms_arcsec,
        image_width: dimensions.0,
        image_height: dimensions.1,
        wcs: WcsResponse {
            crval: [solution.wcs.crval.0, solution.wcs.crval.1],
            crpix: [solution.wcs.crpix.0, solution.wcs.crpix.1],
            cd: solution.wcs.cd,
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            cunit: ["deg".into(), "deg".into()],
            radesys: "ICRS".into(),
            equinox: 2000.0,
        },
        footprint,
        objects: Vec::new(),
        catalog_version: None,
        capture_time: options.capture_time,
    })
}

pub fn capture_time_from_bytes(
    bytes: &[u8],
    filename: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let looks_like_fits = filename.rsplit('.').next().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("fits")
            || extension.eq_ignore_ascii_case("fit")
            || extension.eq_ignore_ascii_case("fts")
    }) || bytes.starts_with(b"SIMPLE  ");
    if !looks_like_fits {
        return None;
    }
    for card in bytes.chunks_exact(80).take(1440) {
        let keyword = std::str::from_utf8(&card[..8]).ok()?.trim();
        if keyword == "END" {
            break;
        }
        if keyword != "DATE-OBS" {
            continue;
        }
        let raw = std::str::from_utf8(&card[10..]).ok()?.trim();
        let value = if let Some(quoted) = raw.strip_prefix('\'') {
            quoted.split('\'').next()?.trim()
        } else {
            raw.split('/').next()?.trim()
        };
        return parse_capture_time(value);
    }
    None
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
    let looks_like_fits = filename.rsplit('.').next().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("fits")
            || extension.eq_ignore_ascii_case("fit")
            || extension.eq_ignore_ascii_case("fts")
    }) || bytes.starts_with(b"SIMPLE  ");
    if looks_like_fits {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_capture_time_from_fits_date_obs() {
        let mut header = vec![b' '; 2_880];
        for (index, card) in [
            "SIMPLE  =                    T",
            "DATE-OBS= '2026-07-13T04:05:06.250Z'",
            "END",
        ]
        .into_iter()
        .enumerate()
        {
            header[index * 80..index * 80 + card.len()].copy_from_slice(card.as_bytes());
        }
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
}
