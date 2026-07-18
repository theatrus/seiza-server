use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type JobId = Uuid;
pub type LegacyJobId = u64;
pub type AstrometryId = u64;

pub fn astrometry_id_for_job(job_id: JobId) -> AstrometryId {
    let bytes = job_id.as_bytes();
    let value = u64::from_be_bytes(bytes[8..16].try_into().expect("UUID tail is eight bytes"))
        & i64::MAX as u64;
    value.max(1)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Solving,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SolveHintSource {
    Explicit,
    FitsHeader,
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
    /// SIP distortion polynomial order. Zero or one keeps a linear TAN
    /// solution; orders 2 through 5 fit SIP when the result improves enough.
    pub sip_order: u8,
    /// Acquisition time used to scope transients and propagate minor bodies.
    /// FITS uploads populate this automatically from DATE-OBS when omitted.
    pub capture_time: Option<DateTime<Utc>>,
    /// Server-resolved provenance for the position and scale hint. Incoming
    /// values are ignored and replaced while the upload is prepared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint_source: Option<SolveHintSource>,
    /// FITS keywords used to derive an automatic hint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hint_keywords: Vec<String>,
}

impl Default for SolveOptions {
    fn default() -> Self {
        Self {
            center_ra_deg: None,
            center_dec_deg: None,
            radius_deg: Some(2.0),
            scale_arcsec_per_pixel: None,
            scale_tolerance: 0.2,
            min_scale_arcsec_per_pixel: 0.1,
            max_scale_arcsec_per_pixel: 20.0,
            sigma: 4.0,
            ignore_border: 0,
            max_stars: 500,
            sip_order: 0,
            capture_time: None,
            hint_source: None,
            hint_keywords: Vec::new(),
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
        if self.sip_order > 5 {
            return Err("sip_order must be between 0 and 5".into());
        }
        Ok(())
    }
}

/// One explicit SIP coefficient record: polynomial exponents `(p, q)` and
/// the associated value. Keeping exponents on the wire avoids coupling the
/// durable API to Seiza's internal vector ordering.
pub type SipCoefficient = (u8, u8, f64);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SipResponse {
    pub order: u8,
    pub a: Vec<SipCoefficient>,
    pub b: Vec<SipCoefficient>,
    pub ap: Vec<SipCoefficient>,
    pub bp: Vec<SipCoefficient>,
}

impl SipResponse {
    pub fn from_seiza(sip: &seiza::Sip) -> Self {
        let coefficients = |terms: Vec<(u8, u8)>, values: &[f64]| {
            terms
                .into_iter()
                .zip(values)
                .map(|((p, q), &value)| (p, q, value))
                .collect()
        };
        Self {
            order: sip.order,
            a: coefficients(seiza::Sip::forward_terms(sip.order), &sip.a),
            b: coefficients(seiza::Sip::forward_terms(sip.order), &sip.b),
            ap: coefficients(seiza::Sip::inverse_terms(sip.order), &sip.ap),
            bp: coefficients(seiza::Sip::inverse_terms(sip.order), &sip.bp),
        }
    }

    fn to_seiza(&self) -> seiza::Sip {
        let values = |terms: Vec<(u8, u8)>, coefficients: &[SipCoefficient]| {
            terms
                .into_iter()
                .map(|term| {
                    coefficients
                        .iter()
                        .find_map(|&(p, q, value)| ((p, q) == term).then_some(value))
                        .unwrap_or(0.0)
                })
                .collect()
        };
        seiza::Sip {
            order: self.order,
            a: values(seiza::Sip::forward_terms(self.order), &self.a),
            b: values(seiza::Sip::forward_terms(self.order), &self.b),
            ap: values(seiza::Sip::inverse_terms(self.order), &self.ap),
            bp: values(seiza::Sip::inverse_terms(self.order), &self.bp),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if !(2..=5).contains(&self.order) {
            return Err("SIP order must be between 2 and 5".into());
        }
        let validate_coefficients =
            |name: &str, expected: Vec<(u8, u8)>, values: &[SipCoefficient]| {
                if values.len() != expected.len() {
                    return Err(format!(
                        "SIP {name} requires {} coefficients for order {}",
                        expected.len(),
                        self.order
                    ));
                }
                for &(p, q) in &expected {
                    let matches = values
                        .iter()
                        .filter(|&&(actual_p, actual_q, _)| (actual_p, actual_q) == (p, q))
                        .collect::<Vec<_>>();
                    if matches.len() != 1 {
                        return Err(format!(
                            "SIP {name} must contain coefficient {name}_{p}_{q} exactly once"
                        ));
                    }
                    if !matches[0].2.is_finite() {
                        return Err(format!("SIP {name}_{p}_{q} must be finite"));
                    }
                }
                Ok(())
            };
        let forward = seiza::Sip::forward_terms(self.order);
        let inverse = seiza::Sip::inverse_terms(self.order);
        validate_coefficients("A", forward.clone(), &self.a)?;
        validate_coefficients("B", forward, &self.b)?;
        validate_coefficients("AP", inverse.clone(), &self.ap)?;
        validate_coefficients("BP", inverse, &self.bp)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sip: Option<SipResponse>,
}

impl WcsResponse {
    pub fn from_seiza(wcs: &seiza::Wcs) -> Self {
        let has_sip = wcs.sip.is_some();
        Self {
            crval: [wcs.crval.0, wcs.crval.1],
            crpix: [wcs.crpix.0, wcs.crpix.1],
            cd: wcs.cd,
            ctype: if has_sip {
                ["RA---TAN-SIP".into(), "DEC--TAN-SIP".into()]
            } else {
                default_ctype()
            },
            cunit: default_cunit(),
            radesys: default_radesys(),
            equinox: default_equinox(),
            sip: wcs.sip.as_ref().map(SipResponse::from_seiza),
        }
    }

    pub fn to_seiza(&self) -> seiza::Wcs {
        seiza::Wcs {
            crval: (self.crval[0], self.crval[1]),
            crpix: (self.crpix[0], self.crpix[1]),
            cd: self.cd,
            sip: self.sip.as_ref().map(SipResponse::to_seiza),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self
            .crval
            .iter()
            .chain(&self.crpix)
            .chain(self.cd.iter().flatten())
            .any(|value| !value.is_finite())
        {
            return Err("WCS coordinates and CD matrix must be finite".into());
        }
        if let Some(sip) = &self.sip {
            sip.validate()?;
            if self.ctype != ["RA---TAN-SIP", "DEC--TAN-SIP"] {
                return Err("a SIP solution requires RA---TAN-SIP / DEC--TAN-SIP axes".into());
            }
        }
        Ok(())
    }
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
pub struct OverlayContour {
    pub closed: bool,
    pub points: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayOutline {
    pub geometry_id: String,
    pub source_record_id: String,
    pub role: String,
    pub quality: String,
    pub level: Option<String>,
    pub contours: Vec<OverlayContour>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayObject {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_id: Option<String>,
    pub name: String,
    pub common_name: String,
    pub kind: String,
    pub mag: Option<f32>,
    pub x: f64,
    pub y: f64,
    pub semi_major_px: f64,
    pub semi_minor_px: f64,
    pub angle_deg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_source: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternate_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternate_sources: Vec<String>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outlines: Vec<OverlayOutline>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SolveMode {
    Blind,
    Hinted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolveStatistics {
    /// Wall-clock time spent inside the solver pipeline, including decode,
    /// detection, matching, and WCS result construction.
    pub total_ms: f64,
    pub decode_ms: f64,
    pub detection_ms: f64,
    pub search_ms: f64,
    pub mode: SolveMode,
    pub detected_stars: usize,
    pub catalog_stars: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blind_index_patterns: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint_source: Option<SolveHintSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hint_keywords: Vec<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statistics: Option<SolveStatistics>,
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
    pub fn validate(&self) -> Result<(), String> {
        self.wcs.validate()
    }

    pub fn fits_wcs_header(&self) -> String {
        let wcs = &self.wcs;
        let mut cards = vec![
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
        ];
        if let Some(sip) = &wcs.sip {
            cards.extend([
                format!(
                    "{:<8}= {:>20} / SIP forward polynomial order",
                    "A_ORDER", sip.order
                ),
                format!(
                    "{:<8}= {:>20} / SIP forward polynomial order",
                    "B_ORDER", sip.order
                ),
            ]);
            for (name, coefficients) in [("A", &sip.a), ("B", &sip.b)] {
                cards.extend(coefficients.iter().map(|&(p, q, value)| {
                    let keyword = format!("{name}_{p}_{q}");
                    format!("{keyword:<8}= {value:>20.12E} / SIP coefficient")
                }));
            }
            cards.extend([
                format!(
                    "{:<8}= {:>20} / SIP inverse polynomial order",
                    "AP_ORDER", sip.order
                ),
                format!(
                    "{:<8}= {:>20} / SIP inverse polynomial order",
                    "BP_ORDER", sip.order
                ),
            ]);
            for (name, coefficients) in [("AP", &sip.ap), ("BP", &sip.bp)] {
                cards.extend(coefficients.iter().map(|&(p, q, value)| {
                    let keyword = format!("{name}_{p}_{q}");
                    format!("{keyword:<8}= {value:>20.12E} / SIP inverse coefficient")
                }));
            }
        }
        cards.extend([
            format!(
                "RADESYS  = '{:<18}' / Celestial reference frame",
                wcs.radesys
            ),
            format!("EQUINOX  = {:>20.8} / Equinox of coordinates", wcs.equinox),
            "END".into(),
        ]);
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
pub struct ValidationDonation {
    pub object_key: String,
    pub comment: Option<String>,
    pub solve_is_invalid: bool,
    pub license_version: String,
    pub donated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationDonationResponse {
    pub comment: Option<String>,
    pub solve_is_invalid: bool,
    pub license_version: String,
    pub donated_at: DateTime<Utc>,
}

impl From<&ValidationDonation> for ValidationDonationResponse {
    fn from(donation: &ValidationDonation) -> Self {
        Self {
            comment: donation.comment.clone(),
            solve_is_invalid: donation.solve_is_invalid,
            license_version: donation.license_version.clone(),
            donated_at: donation.donated_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobRecord {
    pub id: JobId,
    pub astrometry_id: AstrometryId,
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
    pub validation_donation: Option<ValidationDonation>,
}

impl JobRecord {
    pub fn input_object_key(&self) -> &str {
        self.validation_donation
            .as_ref()
            .map_or(&self.object_key, |donation| &donation.object_key)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResponse {
    /// Opaque UUIDv4 capability used by public result and artifact URLs.
    pub id: String,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// End-to-end time between a worker claiming the job and persisting its
    /// completion. Available for both successful and failed attempts.
    pub solve_time_ms: Option<u64>,
    pub original_filename: String,
    pub options: SolveOptions,
    pub input_expires_at: DateTime<Utc>,
    pub input_available: bool,
    pub preview_url: Option<String>,
    pub overlay_url: Option<String>,
    pub annotations_url: Option<String>,
    pub wcs_url: Option<String>,
    pub solution: Option<SolutionResponse>,
    pub error: Option<String>,
    pub validation_donation: Option<ValidationDonationResponse>,
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
    fn legacy_options_and_wcs_records_default_to_linear_solves() {
        let options: SolveOptions = serde_json::from_str("{}").unwrap();
        let wcs: WcsResponse = serde_json::from_str(
            r#"{
                "crval":[10.0,20.0],
                "crpix":[49.0,39.0],
                "cd":[[0.001,0.0],[0.0,0.001]]
            }"#,
        )
        .unwrap();

        assert_eq!(options.sip_order, 0);
        assert!(wcs.sip.is_none());
        assert_eq!(wcs.ctype, default_ctype());
    }

    #[test]
    fn sip_records_round_trip_with_explicit_exponents() {
        let sip = seiza::Sip {
            order: 2,
            a: vec![1.0e-7, 2.0e-7, 3.0e-7],
            b: vec![-1.0e-7, -2.0e-7, -3.0e-7],
            ap: vec![0.0; 6],
            bp: vec![0.0; 6],
        };
        let mut source =
            seiza::Wcs::from_center_scale_rotation((10.0, 20.0), (49.0, 39.0), 1.0, 0.0, false);
        source.sip = Some(sip);
        let encoded = serde_json::to_string(&WcsResponse::from_seiza(&source)).unwrap();
        let decoded: WcsResponse = serde_json::from_str(&encoded).unwrap();

        decoded.validate().unwrap();
        assert_eq!(decoded.sip.as_ref().unwrap().a[0], (0, 2, 1.0e-7));
        assert_eq!(decoded.sip.as_ref().unwrap().a[2], (2, 0, 3.0e-7));
        assert_eq!(
            decoded.to_seiza().sip.unwrap().a,
            vec![1.0e-7, 2.0e-7, 3.0e-7]
        );
    }

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
                sip: None,
            },
            footprint: [[0.0; 2]; 4],
            objects: Vec::new(),
            catalog_version: None,
            capture_time: None,
            statistics: None,
        };

        let header = solution.fits_wcs_header();

        assert!(header.contains("CRPIX1   =      50.000000000000"));
        assert!(header.contains("CRPIX2   =      40.000000000000"));
        assert!(header.lines().all(|line| line.len() == 80));
    }

    #[test]
    fn wcs_download_includes_explicit_sip_keywords() {
        let sip = seiza::Sip {
            order: 2,
            a: vec![1.0e-7, 2.0e-7, 3.0e-7],
            b: vec![-1.0e-7, -2.0e-7, -3.0e-7],
            ap: vec![0.0; 6],
            bp: vec![0.0; 6],
        };
        let mut wcs =
            seiza::Wcs::from_center_scale_rotation((10.0, 20.0), (49.0, 39.0), 1.0, 0.0, false);
        wcs.sip = Some(sip);
        let solution = SolutionResponse {
            center_ra_deg: 10.0,
            center_dec_deg: 20.0,
            pixel_scale_arcsec_per_pixel: 1.0,
            matched_stars: 12,
            rms_arcsec: 0.2,
            image_width: 100,
            image_height: 80,
            wcs: WcsResponse::from_seiza(&wcs),
            footprint: [[0.0; 2]; 4],
            objects: Vec::new(),
            catalog_version: None,
            capture_time: None,
            statistics: None,
        };

        solution.validate().unwrap();
        let header = solution.fits_wcs_header();

        assert!(header.contains("CTYPE1   = 'RA---TAN-SIP"));
        assert!(header.contains("A_ORDER =                    2"));
        assert!(header.contains("A_0_2"));
        assert!(header.contains("AP_ORDER=                    2"));
        assert!(header.lines().all(|line| line.len() == 80));
    }
}
