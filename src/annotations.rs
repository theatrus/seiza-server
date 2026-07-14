use crate::models::{AnnotationResponse, JobId, OverlayObject, SolutionResponse};
use chrono::{DateTime, NaiveDate, Utc};
use seiza::{
    catalog::{StarCatalog, TileCatalog, angular_separation_deg},
    minor_bodies::{MinorBodyCatalog, MinorBodyKind},
    objects::{ObjectCatalog, ObjectKind},
    wcs::Wcs,
};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub struct AnnotationOptions {
    pub deep_sky: bool,
    pub named_stars: bool,
    pub field_stars: bool,
    pub transients: bool,
    pub minor_bodies: bool,
    pub historical_transients: bool,
    pub field_star_mag_limit: f32,
    pub max_field_stars: usize,
}

impl Default for AnnotationOptions {
    fn default() -> Self {
        Self {
            deep_sky: true,
            named_stars: true,
            field_stars: false,
            transients: true,
            minor_bodies: true,
            historical_transients: false,
            field_star_mag_limit: 10.0,
            max_field_stars: 300,
        }
    }
}

#[derive(Clone)]
pub struct AnnotationEngine {
    stars: Option<Arc<TileCatalog>>,
    star_version: Option<String>,
    objects: Option<ReloadingCatalog<ObjectCatalog>>,
    transients: Option<ReloadingCatalog<ObjectCatalog>>,
    minor_bodies: Option<ReloadingCatalog<MinorBodyCatalog>>,
}

impl AnnotationEngine {
    pub fn new(
        stars: Option<Arc<TileCatalog>>,
        star_path: Option<&Path>,
        object_path: Option<&Path>,
        transient_path: Option<&Path>,
        minor_body_path: Option<&Path>,
    ) -> Self {
        let engine = Self {
            stars,
            star_version: star_path
                .and_then(catalog_signature)
                .map(|value| value.version()),
            objects: object_path.map(|path| {
                ReloadingCatalog::new(path.to_owned(), "deep-sky", ObjectCatalog::open)
            }),
            transients: transient_path.map(|path| {
                ReloadingCatalog::new(path.to_owned(), "transient", ObjectCatalog::open)
            }),
            minor_bodies: minor_body_path.map(|path| {
                ReloadingCatalog::new(path.to_owned(), "minor-body", MinorBodyCatalog::open)
            }),
        };
        engine.warm_catalogs();
        engine
    }

    pub fn is_configured(&self) -> bool {
        self.objects.is_some() || self.transients.is_some() || self.minor_bodies.is_some()
    }

    pub fn annotate(
        &self,
        job_id: JobId,
        solution: &SolutionResponse,
        capture_time: Option<DateTime<Utc>>,
        options: &AnnotationOptions,
    ) -> AnnotationResponse {
        let wcs = Wcs {
            crval: (solution.wcs.crval[0], solution.wcs.crval[1]),
            crpix: (solution.wcs.crpix[0], solution.wcs.crpix[1]),
            cd: solution.wcs.cd,
        };
        let dimensions = (solution.image_width, solution.image_height);
        let mut objects = if self.is_configured() {
            Vec::new()
        } else {
            solution.objects.clone()
        };
        let mut versions = Vec::new();

        if let Some(version) = &self.star_version {
            versions.push(format!("stars:{version}"));
        }
        if let Some(catalog) = &self.objects
            && let Some((catalog, version)) = catalog.current()
        {
            versions.push(format!("objects:{version}"));
            append_object_catalog(
                &mut objects,
                &catalog,
                &wcs,
                dimensions,
                capture_time,
                options,
                false,
            );
        }
        if let Some(catalog) = &self.transients
            && let Some((catalog, version)) = catalog.current()
        {
            versions.push(format!("transients:{version}"));
            if options.transients {
                append_object_catalog(
                    &mut objects,
                    &catalog,
                    &wcs,
                    dimensions,
                    capture_time,
                    options,
                    true,
                );
            }
        }
        if options.field_stars {
            append_field_stars(
                &mut objects,
                self.stars.as_deref(),
                &wcs,
                dimensions,
                options,
            );
        }
        if let Some(catalog) = &self.minor_bodies
            && let Some((catalog, version)) = catalog.current()
        {
            versions.push(format!("minor-bodies:{version}"));
            if options.minor_bodies
                && let Some(capture_time) = capture_time
            {
                append_minor_bodies(&mut objects, &catalog, &wcs, dimensions, capture_time);
            }
        }

        let mut counts = BTreeMap::new();
        for object in &objects {
            *counts
                .entry(layer_name(&object.kind).to_owned())
                .or_insert(0) += 1;
            if object.kind == "transient" && object.near_capture == Some(false) {
                *counts.entry("historical_transients".into()).or_insert(0) += 1;
            }
        }
        AnnotationResponse {
            job_id,
            catalog_version: if versions.is_empty() {
                "unconfigured".into()
            } else {
                versions.join(";")
            },
            capture_time,
            counts,
            objects,
        }
    }

    fn warm_catalogs(&self) {
        if let Some(catalog) = &self.objects {
            let _ = catalog.current();
        }
        if let Some(catalog) = &self.transients {
            let _ = catalog.current();
        }
        if let Some(catalog) = &self.minor_bodies {
            let _ = catalog.current();
        }
    }
}

fn append_object_catalog(
    output: &mut Vec<OverlayObject>,
    catalog: &ObjectCatalog,
    wcs: &Wcs,
    dimensions: (u32, u32),
    capture_time: Option<DateTime<Utc>>,
    options: &AnnotationOptions,
    force_transient: bool,
) {
    for placed in catalog.objects_in_footprint(wcs, dimensions) {
        let transient = force_transient || placed.object.kind == ObjectKind::Transient;
        let named_star = matches!(
            placed.object.kind,
            ObjectKind::Star | ObjectKind::DoubleStar
        );
        if (transient && !options.transients)
            || (named_star && !options.named_stars)
            || (!transient && !named_star && !options.deep_sky)
        {
            continue;
        }
        let discovered = transient
            .then(|| transient_discovery_date(&placed.object.common_name))
            .flatten();
        let near_capture =
            transient.then(|| transient_near_capture(discovered.as_deref(), capture_time));
        if transient && near_capture == Some(false) && !options.historical_transients {
            continue;
        }
        output.push(OverlayObject {
            name: placed.object.name,
            common_name: placed.object.common_name,
            kind: if force_transient {
                "transient".into()
            } else {
                placed.object.kind.as_str().into()
            },
            mag: placed.object.mag,
            x: placed.x,
            y: placed.y,
            semi_major_px: placed.semi_major_px,
            semi_minor_px: placed.semi_minor_px,
            angle_deg: placed.angle_deg,
            source: Some(if transient { "transient" } else { "deep_sky" }.into()),
            ra_deg: Some(placed.object.ra),
            dec_deg: Some(placed.object.dec),
            discovered,
            near_capture,
            distance_au: None,
            direction_pa_deg: None,
            direction_angle_deg: None,
        });
    }
}

fn append_field_stars(
    output: &mut Vec<OverlayObject>,
    catalog: Option<&TileCatalog>,
    wcs: &Wcs,
    dimensions: (u32, u32),
    options: &AnnotationOptions,
) {
    let Some(catalog) = catalog else { return };
    let center = wcs.pixel_to_world(dimensions.0 as f64 / 2.0, dimensions.1 as f64 / 2.0);
    let radius = wcs
        .footprint(dimensions.0, dimensions.1)
        .into_iter()
        .map(|point| angular_separation_deg(center.0, center.1, point.0, point.1))
        .fold(0.0_f64, f64::max)
        * 1.05;
    let limit = options.max_field_stars.clamp(1, 2_000);
    let mut field_count = 0;
    for star in catalog.cone_search(center.0, center.1, radius, limit * 3) {
        if star.mag > options.field_star_mag_limit {
            continue;
        }
        let Some((x, y)) = wcs.world_to_pixel(star.ra, star.dec) else {
            continue;
        };
        if x < 0.0 || y < 0.0 || x >= dimensions.0 as f64 || y >= dimensions.1 as f64 {
            continue;
        }
        output.push(OverlayObject {
            name: String::new(),
            common_name: String::new(),
            kind: "field-star".into(),
            mag: Some(star.mag),
            x,
            y,
            semi_major_px: 0.0,
            semi_minor_px: 0.0,
            angle_deg: 0.0,
            source: Some("star_catalog".into()),
            ra_deg: Some(star.ra),
            dec_deg: Some(star.dec),
            discovered: None,
            near_capture: None,
            distance_au: None,
            direction_pa_deg: None,
            direction_angle_deg: None,
        });
        field_count += 1;
        if field_count >= limit {
            break;
        }
    }
}

fn append_minor_bodies(
    output: &mut Vec<OverlayObject>,
    catalog: &MinorBodyCatalog,
    wcs: &Wcs,
    dimensions: (u32, u32),
    capture_time: DateTime<Utc>,
) {
    let jd = 2_440_587.5 + capture_time.timestamp_millis() as f64 / 86_400_000.0;
    for placed in catalog.objects_in_footprint(wcs, dimensions, jd, 18.0) {
        let kind = match placed.body.kind {
            MinorBodyKind::Comet => "comet",
            MinorBodyKind::Asteroid => "asteroid",
        };
        output.push(OverlayObject {
            name: placed.body.name,
            common_name: format!("V~{:.1}, {:.2} AU", placed.mag, placed.delta_au),
            kind: kind.into(),
            mag: Some(placed.mag as f32),
            x: placed.x,
            y: placed.y,
            semi_major_px: 0.0,
            semi_minor_px: 0.0,
            angle_deg: 0.0,
            source: Some("minor_body".into()),
            ra_deg: Some(placed.ra),
            dec_deg: Some(placed.dec),
            discovered: None,
            near_capture: Some(true),
            distance_au: Some(placed.delta_au),
            direction_pa_deg: placed.direction_pa_deg,
            direction_angle_deg: placed
                .direction_pa_deg
                .and_then(|angle| direction_image_angle(wcs, placed.ra, placed.dec, angle)),
        });
    }
}

fn direction_image_angle(wcs: &Wcs, ra: f64, dec: f64, pa_deg: f64) -> Option<f64> {
    let (x, y) = wcs.world_to_pixel(ra, dec)?;
    let epsilon = 1.0 / 60.0;
    let north = wcs.world_to_pixel(ra, (dec + epsilon).min(90.0))?;
    let east = wcs.world_to_pixel(ra + epsilon / dec.to_radians().cos().abs().max(1e-6), dec)?;
    let normalize = |point: (f64, f64)| {
        let vector = (point.0 - x, point.1 - y);
        let length = vector.0.hypot(vector.1).max(1e-12);
        (vector.0 / length, vector.1 / length)
    };
    let north = normalize(north);
    let east = normalize(east);
    let (sin, cos) = pa_deg.to_radians().sin_cos();
    Some(
        (north.1 * cos + east.1 * sin)
            .atan2(north.0 * cos + east.0 * sin)
            .to_degrees(),
    )
}

fn transient_discovery_date(details: &str) -> Option<String> {
    let raw = details
        .split(", ")
        .find_map(|part| part.strip_prefix("disc. "))?;
    let mut parts = raw.split('/');
    let year: i32 = parts.next()?.trim().parse().ok()?;
    let month: u32 = parts.next()?.trim().parse().ok()?;
    let day: u32 = parts.next()?.trim().parse().ok()?;
    NaiveDate::from_ymd_opt(year, month, day).map(|value| value.format("%Y-%m-%d").to_string())
}

fn transient_near_capture(discovered: Option<&str>, capture: Option<DateTime<Utc>>) -> bool {
    let (Some(discovered), Some(capture)) = (discovered, capture) else {
        return true;
    };
    let Ok(discovered) = NaiveDate::parse_from_str(discovered, "%Y-%m-%d") else {
        return true;
    };
    let capture = capture.date_naive();
    discovered >= capture - chrono::Duration::days(365)
        && discovered <= capture + chrono::Duration::days(30)
}

fn layer_name(kind: &str) -> &'static str {
    match kind {
        "field-star" => "field_stars",
        "star" | "double-star" => "named_stars",
        "transient" => "transients",
        "comet" | "asteroid" => "minor_bodies",
        _ => "deep_sky",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CatalogSignature {
    len: u64,
    modified: SystemTime,
}

impl CatalogSignature {
    fn version(self) -> String {
        let modified = self.modified.duration_since(UNIX_EPOCH).unwrap_or_default();
        format!(
            "{}:{}.{:09}",
            self.len,
            modified.as_secs(),
            modified.subsec_nanos()
        )
    }
}

fn catalog_signature(path: &Path) -> Option<CatalogSignature> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(CatalogSignature {
        len: metadata.len(),
        modified: metadata.modified().ok()?,
    })
}

struct LoadedCatalog<T> {
    signature: CatalogSignature,
    catalog: Arc<T>,
}

struct ReloadingCatalog<T> {
    path: PathBuf,
    label: &'static str,
    open: fn(&Path) -> io::Result<T>,
    state: Arc<RwLock<Option<LoadedCatalog<T>>>>,
}

impl<T> Clone for ReloadingCatalog<T> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            label: self.label,
            open: self.open,
            state: self.state.clone(),
        }
    }
}

impl<T> ReloadingCatalog<T> {
    fn new(path: PathBuf, label: &'static str, open: fn(&Path) -> io::Result<T>) -> Self {
        Self {
            path,
            label,
            open,
            state: Arc::new(RwLock::new(None)),
        }
    }

    fn current(&self) -> Option<(Arc<T>, String)> {
        let signature = catalog_signature(&self.path)?;
        if let Some(loaded) = self.state.read().ok()?.as_ref()
            && loaded.signature == signature
        {
            return Some((loaded.catalog.clone(), signature.version()));
        }
        let catalog = match (self.open)(&self.path) {
            Ok(catalog) => Arc::new(catalog),
            Err(error) => {
                tracing::warn!(path = %self.path.display(), catalog = self.label, %error, "could not reload annotation catalog");
                return self
                    .state
                    .read()
                    .ok()?
                    .as_ref()
                    .map(|loaded| (loaded.catalog.clone(), loaded.signature.version()));
            }
        };
        tracing::info!(path = %self.path.display(), catalog = self.label, version = %signature.version(), "loaded annotation catalog");
        *self.state.write().ok()? = Some(LoadedCatalog {
            signature,
            catalog: catalog.clone(),
        });
        Some((catalog, signature.version()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::WcsResponse;
    use seiza::objects::SkyObject;

    #[test]
    fn transient_dates_are_scoped_around_capture_time() {
        let capture = DateTime::parse_from_rfc3339("2026-07-13T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(transient_near_capture(Some("2026-07-08"), Some(capture)));
        assert!(!transient_near_capture(Some("2020-01-01"), Some(capture)));
        assert!(transient_near_capture(None, Some(capture)));
    }

    #[test]
    fn extracts_transient_discovery_date() {
        assert_eq!(
            transient_discovery_date("type II, disc. 2026/07/08, in NGC 3310"),
            Some("2026-07-08".into())
        );
    }

    #[test]
    fn catalog_replacement_reprojects_without_a_new_solution() {
        let path = std::env::temp_dir().join(format!(
            "seiza-server-annotations-{}.bin",
            uuid::Uuid::now_v7()
        ));
        let object = |name: &str, ra: f64| SkyObject {
            kind: ObjectKind::Galaxy,
            ra,
            dec: 20.0,
            mag: Some(8.0),
            major_arcmin: Some(2.0),
            minor_arcmin: Some(1.0),
            position_angle_deg: Some(0.0),
            name: name.into(),
            common_name: String::new(),
        };
        ObjectCatalog::new(vec![object("M 1", 10.0)])
            .write_to(&path)
            .unwrap();
        let engine = AnnotationEngine::new(None, None, Some(&path), None, None);
        let solution = SolutionResponse {
            center_ra_deg: 10.0,
            center_dec_deg: 20.0,
            pixel_scale_arcsec_per_pixel: 3.6,
            matched_stars: 10,
            rms_arcsec: 0.5,
            image_width: 200,
            image_height: 200,
            wcs: WcsResponse {
                crval: [10.0, 20.0],
                crpix: [100.0, 100.0],
                cd: [[-0.001, 0.0], [0.0, -0.001]],
                ctype: ["RA---TAN".into(), "DEC--TAN".into()],
                cunit: ["deg".into(), "deg".into()],
                radesys: "ICRS".into(),
                equinox: 2000.0,
            },
            footprint: [[0.0; 2]; 4],
            objects: Vec::new(),
            catalog_version: None,
            capture_time: None,
        };
        let first = engine.annotate(1, &solution, None, &AnnotationOptions::default());
        assert_eq!(first.objects.len(), 1);

        ObjectCatalog::new(vec![object("M 1", 10.0), object("M 2", 10.02)])
            .write_to(&path)
            .unwrap();
        let second = engine.annotate(1, &solution, None, &AnnotationOptions::default());
        assert_eq!(second.objects.len(), 2);
        assert_ne!(first.catalog_version, second.catalog_version);
        std::fs::remove_file(path).unwrap();
    }
}
