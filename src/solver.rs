use crate::models::{SolutionResponse, SolveOptions, WcsResponse};
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use seiza::{
    DetectConfig,
    blind::{BlindIndex, BlindParams, solve_blind},
    catalog::TileCatalog,
    detect_stars,
    solve::{SolveHint, solve},
};
use std::{path::Path, sync::Arc};

#[derive(Clone)]
pub struct SolverEngine {
    catalog: Option<Arc<TileCatalog>>,
}

impl SolverEngine {
    pub fn from_catalog_path(path: Option<&Path>) -> Self {
        let catalog = path.and_then(|path| match TileCatalog::open(path) {
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
        },
    })
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
