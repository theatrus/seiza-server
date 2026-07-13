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
    pub id: JobId,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub original_filename: String,
    pub solution: Option<SolutionResponse>,
    pub error: Option<String>,
}

impl From<&JobRecord> for JobResponse {
    fn from(job: &JobRecord) -> Self {
        Self {
            id: job.id,
            status: job.status,
            created_at: job.created_at,
            started_at: job.started_at,
            completed_at: job.completed_at,
            original_filename: job.original_filename.clone(),
            solution: job.solution.clone(),
            error: job.error.clone(),
        }
    }
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
