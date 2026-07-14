use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub type JobId = u64;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Solving,
    Succeeded,
    Failed,
}

impl JobStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Solving => "solving",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "queued" => Ok(Self::Queued),
            "solving" => Ok(Self::Solving),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            _ => Err(format!("unknown job status `{value}`")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SolveOptions {
    /// ICRS position hint in degrees. A hint is used only when both center
    /// values and a pixel scale are supplied.
    pub center_ra_deg: Option<f64>,
    pub center_dec_deg: Option<f64>,
    pub radius_deg: Option<f64>,
    pub scale_arcsec_per_pixel: Option<f64>,
    pub scale_tolerance: f64,
    /// Bounds for blind solving, in arcseconds/pixel.
    pub min_scale_arcsec_per_pixel: f64,
    pub max_scale_arcsec_per_pixel: f64,
    pub sigma: f32,
    pub ignore_border: u32,
    pub max_stars: usize,
    /// Acquisition time used to scope transients and propagate minor bodies.
    /// FITS uploads populate this automatically from DATE-OBS when omitted.
    pub capture_time: Option<DateTime<Utc>>,
}

impl Default for SolveOptions {
    fn default() -> Self {
        Self {
            center_ra_deg: None,
            center_dec_deg: None,
            radius_deg: Some(2.0),
            scale_arcsec_per_pixel: None,
            scale_tolerance: 0.2,
            min_scale_arcsec_per_pixel: 0.3,
            max_scale_arcsec_per_pixel: 20.0,
            sigma: 4.0,
            ignore_border: 0,
            max_stars: 600,
            capture_time: None,
        }
    }
}

impl SolveOptions {
    pub fn validate(&self) -> Result<(), String> {
        if !(self.sigma.is_finite() && self.sigma > 0.0) {
            return Err("sigma must be positive".into());
        }
        if !(self.scale_tolerance.is_finite() && (0.01..=1.0).contains(&self.scale_tolerance)) {
            return Err("scale_tolerance must be between 0.01 and 1.0".into());
        }
        if !(self.min_scale_arcsec_per_pixel.is_finite()
            && self.max_scale_arcsec_per_pixel.is_finite()
            && self.min_scale_arcsec_per_pixel > 0.0
            && self.max_scale_arcsec_per_pixel >= self.min_scale_arcsec_per_pixel)
        {
            return Err("blind scale bounds are invalid".into());
        }
        let has_center = self.center_ra_deg.is_some() || self.center_dec_deg.is_some();
        if has_center
            && (self.center_ra_deg.is_none()
                || self.center_dec_deg.is_none()
                || self.scale_arcsec_per_pixel.is_none())
        {
            return Err(
                "a hinted solve requires center_ra_deg, center_dec_deg, and scale_arcsec_per_pixel"
                    .into(),
            );
        }
        if let Some(ra) = self.center_ra_deg
            && !(0.0..=360.0).contains(&ra)
        {
            return Err("center_ra_deg must be between 0 and 360".into());
        }
        if let Some(dec) = self.center_dec_deg
            && !(-90.0..=90.0).contains(&dec)
        {
            return Err("center_dec_deg must be between -90 and 90".into());
        }
        if let Some(scale) = self.scale_arcsec_per_pixel
            && !(scale.is_finite() && scale > 0.0)
        {
            return Err("scale_arcsec_per_pixel must be positive".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WcsResponse {
    pub crval: [f64; 2],
    pub crpix: [f64; 2],
    pub cd: [[f64; 2]; 2],
    #[serde(default = "default_ctype")]
    pub ctype: [String; 2],
    #[serde(default = "default_cunit")]
    pub cunit: [String; 2],
    #[serde(default = "default_radesys")]
    pub radesys: String,
    #[serde(default = "default_equinox")]
    pub equinox: f64,
}

fn default_ctype() -> [String; 2] {
    ["RA---TAN".into(), "DEC--TAN".into()]
}

fn default_cunit() -> [String; 2] {
    ["deg".into(), "deg".into()]
}

fn default_radesys() -> String {
    "ICRS".into()
}

const fn default_equinox() -> f64 {
    2000.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayObject {
    pub name: String,
    pub common_name: String,
    pub kind: String,
    pub mag: Option<f32>,
    pub x: f64,
    pub y: f64,
    pub semi_major_px: f64,
    pub semi_minor_px: f64,
    pub angle_deg: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ra_deg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dec_deg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near_capture: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_au: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction_pa_deg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction_angle_deg: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolutionResponse {
    pub center_ra_deg: f64,
    pub center_dec_deg: f64,
    pub pixel_scale_arcsec_per_pixel: f64,
    pub matched_stars: usize,
    pub rms_arcsec: f64,
    pub image_width: u32,
    pub image_height: u32,
    pub wcs: WcsResponse,
    #[serde(default)]
    pub footprint: [[f64; 2]; 4],
    #[serde(default)]
    pub objects: Vec<OverlayObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_time: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationResponse {
    pub job_id: String,
    pub catalog_version: String,
    pub capture_time: Option<DateTime<Utc>>,
    pub available: std::collections::BTreeMap<String, bool>,
    pub counts: std::collections::BTreeMap<String, usize>,
    pub objects: Vec<OverlayObject>,
}

impl SolutionResponse {
    pub fn fits_wcs_header(&self) -> String {
        let wcs = &self.wcs;
        let cards = [
            "WCSAXES =                    2 / Number of WCS axes".into(),
            "NAXIS    =                    2 / Number of image axes".into(),
            format!("NAXIS1   = {:>20} / Image width", self.image_width),
            format!("NAXIS2   = {:>20} / Image height", self.image_height),
            format!("CTYPE1   = '{:<18}' / Axis 1 projection", wcs.ctype[0]),
            format!("CTYPE2   = '{:<18}' / Axis 2 projection", wcs.ctype[1]),
            format!("CUNIT1   = '{:<18}' / Axis 1 unit", wcs.cunit[0]),
            format!("CUNIT2   = '{:<18}' / Axis 2 unit", wcs.cunit[1]),
            format!(
                "CRVAL1   = {:>20.12} / Reference right ascension",
                wcs.crval[0]
            ),
            format!("CRVAL2   = {:>20.12} / Reference declination", wcs.crval[1]),
            // Seiza uses zero-indexed pixel coordinates internally. FITS CRPIX
            // is one-indexed, so add one only in this standards-facing file.
            format!(
                "CRPIX1   = {:>20.12} / Reference pixel, FITS convention",
                wcs.crpix[0] + 1.0
            ),
            format!(
                "CRPIX2   = {:>20.12} / Reference pixel, FITS convention",
                wcs.crpix[1] + 1.0
            ),
            format!("CD1_1    = {:>20.12E} / Degrees per pixel", wcs.cd[0][0]),
            format!("CD1_2    = {:>20.12E} / Degrees per pixel", wcs.cd[0][1]),
            format!("CD2_1    = {:>20.12E} / Degrees per pixel", wcs.cd[1][0]),
            format!("CD2_2    = {:>20.12E} / Degrees per pixel", wcs.cd[1][1]),
            format!(
                "RADESYS  = '{:<18}' / Celestial reference frame",
                wcs.radesys
            ),
            format!("EQUINOX  = {:>20.8} / Equinox of coordinates", wcs.equinox),
            "END".into(),
        ];
        cards
            .into_iter()
            .map(|mut card| {
                card.truncate(80);
                format!("{card:<80}")
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }
}

#[derive(Debug, Clone)]
pub struct JobRecord {
    pub id: JobId,
    pub owner: String,
    pub queue_weight: f64,
    pub object_key: String,
    pub original_filename: String,
    pub content_type: Option<String>,
    pub options: SolveOptions,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub solution: Option<SolutionResponse>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResponse {
    /// Opaque public locator. Its queue sequence is never sufficient without
    /// the independent random token.
    pub id: String,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub original_filename: String,
    pub input_expires_at: DateTime<Utc>,
    pub input_available: bool,
    pub preview_url: Option<String>,
    pub overlay_url: Option<String>,
    pub annotations_url: Option<String>,
    pub wcs_url: Option<String>,
    pub solution: Option<SolutionResponse>,
    pub error: Option<String>,
}

/// A short-lived, exclusive worker reservation. The lease token is required
/// to fetch the original and report completion, so stale workers cannot write
/// over a retried job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobLease {
    pub job_id: JobId,
    pub lease_token: String,
    pub lease_expires_at: DateTime<Utc>,
    pub original_filename: String,
    pub options: SolveOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerCompletion {
    pub lease_token: String,
    pub solution: Option<SolutionResponse>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wcs_download_uses_fits_pixel_convention_and_eighty_column_cards() {
        let solution = SolutionResponse {
            center_ra_deg: 10.0,
            center_dec_deg: 20.0,
            pixel_scale_arcsec_per_pixel: 1.0,
            matched_stars: 12,
            rms_arcsec: 0.2,
            image_width: 100,
            image_height: 80,
            wcs: WcsResponse {
                crval: [10.0, 20.0],
                crpix: [49.0, 39.0],
                cd: [[0.001, 0.0], [0.0, 0.001]],
                ctype: default_ctype(),
                cunit: default_cunit(),
                radesys: default_radesys(),
                equinox: default_equinox(),
            },
            footprint: [[0.0; 2]; 4],
            objects: Vec::new(),
            catalog_version: None,
            capture_time: None,
        };

        let header = solution.fits_wcs_header();

        assert!(header.contains("CRPIX1   =      50.000000000000"));
        assert!(header.contains("CRPIX2   =      40.000000000000"));
        assert!(header.lines().all(|line| line.len() == 80));
    }
}
