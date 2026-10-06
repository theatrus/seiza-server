use crate::{
    models::{AnnotationResponse, OverlayContour, OverlayObject, OverlayOutline, SolutionResponse},
    star_identifiers::{StarIdentifierLayer, StarIdentifierMatch},
};
use chrono::{DateTime, NaiveDate, Utc};
use seiza::{
    catalog::{StarCatalog, TileCatalog, angular_separation_deg},
    minor_bodies::{MinorBodyCatalog, MinorBodyKind},
    objects::{
        GeometryData, GeometryQuality, GeometryRole, ObjectCatalog, ObjectCatalogCapabilities,
        ObjectCatalogProvenance, ObjectDetails, ObjectHit, ObjectKind, ObjectNameMatch,
        ObjectQuery, ObjectQueryError, SkyObject, SkyRegion,
    },
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
    pub star_identifiers: bool,
    pub field_stars: bool,
    pub transients: bool,
    pub minor_bodies: bool,
    pub historical_transients: bool,
    pub field_star_mag_limit: f32,
    pub max_field_stars: usize,
    pub star_identifier_mag_limit: f32,
    pub max_star_identifiers: usize,
    /// Most deep-sky objects to return, ranked by how visible they are likely
    /// to be at this image scale. Wide fields hold tens of thousands of
    /// catalog galaxies far smaller than a pixel.
    pub max_deep_sky: usize,
    /// Deep-sky objects whose catalog semi-major axis spans fewer image
    /// pixels than this are left out unless they have a common name.
    pub deep_sky_min_size_px: f64,
    pub deep_sky_max_mag: Option<f32>,
    pub max_named_stars: usize,
    pub named_star_mag_limit: Option<f32>,
    pub max_transients: usize,
    /// Fields wider than [`WIDE_FIELD_DIAGONAL_DEG`] default to
    /// [`WIDE_FIELD_TRANSIENT_MAG_LIMIT`] when this is unset.
    pub transient_mag_limit: Option<f32>,
    pub max_minor_bodies: usize,
    /// Names, designations or stable IDs the user asked for; matching objects
    /// in the field are always returned, whatever the limits above say.
    pub requested_objects: Vec<String>,
}

/// Fields wider than this many degrees across the diagonal (phone and
/// camera-lens frames) get a transient magnitude limit by default.
pub const WIDE_FIELD_DIAGONAL_DEG: f64 = 10.0;
/// Supernovae fainter than this vanish in a wide-field frame.
pub const WIDE_FIELD_TRANSIENT_MAG_LIMIT: f32 = 13.0;

pub struct StarIdentifierCatalogSearch {
    pub matches: Vec<StarIdentifierMatch>,
    pub catalog_version: String,
    pub catalog_entries: usize,
    pub spatial_labels: usize,
    pub attribution: String,
    pub epoch: f64,
}

pub struct ObjectCatalogDetailsLookup {
    pub object: SkyObject,
    pub details: ObjectDetails,
    pub capabilities: ObjectCatalogCapabilities,
    pub provenance: Option<ObjectCatalogProvenance>,
    pub format_version: u8,
    pub catalog_version: String,
}

impl Default for AnnotationOptions {
    fn default() -> Self {
        Self {
            deep_sky: true,
            named_stars: true,
            star_identifiers: false,
            field_stars: false,
            transients: true,
            minor_bodies: true,
            historical_transients: false,
            field_star_mag_limit: 10.0,
            max_field_stars: 300,
            star_identifier_mag_limit: 10.0,
            max_star_identifiers: 150,
            max_deep_sky: 200,
            deep_sky_min_size_px: 1.5,
            deep_sky_max_mag: None,
            max_named_stars: 60,
            named_star_mag_limit: None,
            max_transients: 60,
            transient_mag_limit: None,
            max_minor_bodies: 100,
            requested_objects: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct AnnotationEngine {
    stars: Option<Arc<TileCatalog>>,
    star_version: Option<String>,
    objects: Option<ReloadingCatalog<ObjectCatalog>>,
    star_identifiers: Option<ReloadingCatalog<StarIdentifierLayer>>,
    transients: Option<ReloadingCatalog<ObjectCatalog>>,
    minor_bodies: Option<ReloadingCatalog<MinorBodyCatalog>>,
}

impl AnnotationEngine {
    pub fn new(
        stars: Option<Arc<TileCatalog>>,
        star_path: Option<&Path>,
        object_path: Option<&Path>,
        star_identifier_path: Option<&Path>,
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
            star_identifiers: star_identifier_path.map(|path| {
                ReloadingCatalog::new(
                    path.to_owned(),
                    "star-identifier",
                    StarIdentifierLayer::open,
                )
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
        self.objects.is_some()
            || self.star_identifiers.is_some()
            || self.transients.is_some()
            || self.minor_bodies.is_some()
    }

    pub fn annotate(
        &self,
        job_id: impl ToString,
        solution: &SolutionResponse,
        capture_time: Option<DateTime<Utc>>,
        options: &AnnotationOptions,
    ) -> AnnotationResponse {
        let wcs = solution.wcs.to_seiza();
        let dimensions = (solution.image_width, solution.image_height);
        let mut objects = if self.is_configured() {
            Vec::new()
        } else {
            solution.objects.clone()
        };
        let mut versions = Vec::new();
        let mut totals = BTreeMap::new();
        let mut available = BTreeMap::from([
            ("deep_sky".into(), false),
            ("named_stars".into(), false),
            ("star_identifiers".into(), false),
            ("field_stars".into(), self.stars.is_some()),
            ("transients".into(), false),
            ("historical_transients".into(), false),
            ("minor_bodies".into(), false),
            ("satellite_tracks".into(), false),
            ("grid".into(), true),
        ]);

        if let Some(version) = &self.star_version {
            versions.push(format!("stars:{version}"));
        }
        if let Some(catalog) = &self.objects
            && let Some((catalog, version)) = catalog.current()
        {
            available.insert("deep_sky".into(), true);
            available.insert("named_stars".into(), true);
            versions.push(format!("objects:{version}"));
            merge_totals(
                &mut totals,
                append_object_catalog(
                    &mut objects,
                    &catalog,
                    &wcs,
                    dimensions,
                    capture_time,
                    options,
                    false,
                ),
            );
        }
        if let Some(catalog) = &self.star_identifiers
            && let Some((catalog, version)) = catalog.current()
        {
            available.insert("star_identifiers".into(), true);
            versions.push(format!("star-identifiers:{version}"));
            if options.star_identifiers {
                append_star_identifier_catalog(&mut objects, &catalog, &wcs, dimensions, options);
            }
        }
        if let Some(catalog) = &self.transients
            && let Some((catalog, version)) = catalog.current()
        {
            available.insert("transients".into(), true);
            available.insert("historical_transients".into(), true);
            versions.push(format!("transients:{version}"));
            if options.transients {
                merge_totals(
                    &mut totals,
                    append_object_catalog(
                        &mut objects,
                        &catalog,
                        &wcs,
                        dimensions,
                        capture_time,
                        options,
                        true,
                    ),
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
                available.insert("minor_bodies".into(), true);
                append_minor_bodies(
                    &mut objects,
                    &mut totals,
                    &catalog,
                    &wcs,
                    dimensions,
                    capture_time,
                    options.max_minor_bodies,
                );
            }
        }

        let mut counts = BTreeMap::from([
            ("deep_sky".into(), 0),
            ("named_stars".into(), 0),
            ("star_identifiers".into(), 0),
            ("field_stars".into(), 0),
            ("transients".into(), 0),
            ("historical_transients".into(), 0),
            ("minor_bodies".into(), 0),
            ("satellite_tracks".into(), 0),
        ]);
        for object in &objects {
            available.insert(layer_name(&object.kind).to_owned(), true);
            *counts
                .entry(layer_name(&object.kind).to_owned())
                .or_insert(0) += 1;
            if object.kind == "transient" && object.near_capture == Some(false) {
                *counts.entry("historical_transients".into()).or_insert(0) += 1;
            }
        }
        AnnotationResponse {
            job_id: job_id.to_string(),
            catalog_version: if versions.is_empty() {
                "unconfigured".into()
            } else {
                versions.join(";")
            },
            capture_time,
            available,
            unavailable_reasons: BTreeMap::new(),
            counts,
            totals,
            objects,
            satellite_tracks: Vec::new(),
            satellite_search: None,
        }
    }

    pub fn query_objects(
        &self,
        region: &SkyRegion,
        query: &ObjectQuery,
    ) -> Result<Option<(Vec<ObjectHit>, String, usize)>, ObjectQueryError> {
        let Some(catalog) = &self.objects else {
            return Ok(None);
        };
        let Some((catalog, version)) = catalog.current() else {
            return Ok(None);
        };
        let catalog_objects = catalog.len();
        Ok(Some((
            catalog.query_region(region, query)?,
            version,
            catalog_objects,
        )))
    }

    pub fn search_objects(
        &self,
        designation: &str,
        prefix: bool,
        limit: usize,
    ) -> io::Result<Option<(Vec<ObjectNameMatch>, String, usize)>> {
        let Some(catalog) = &self.objects else {
            return Ok(None);
        };
        let Some((catalog, version)) = catalog.current() else {
            return Ok(None);
        };
        let catalog_objects = catalog.len();
        let mut matches = if prefix {
            catalog.search_names(designation, limit)?
        } else {
            catalog.lookup_name(designation)?
        };
        matches.truncate(limit);
        Ok(Some((matches, version, catalog_objects)))
    }

    pub fn object_details(
        &self,
        canonical_id: &str,
    ) -> io::Result<Option<ObjectCatalogDetailsLookup>> {
        let Some(catalog) = &self.objects else {
            return Ok(None);
        };
        let Some((catalog, catalog_version)) = catalog.current() else {
            return Ok(None);
        };
        let Some(details) = catalog.object_details(canonical_id)? else {
            return Ok(None);
        };
        let object = catalog
            .lookup_name(canonical_id)?
            .into_iter()
            .find(|item| item.object.metadata.id == canonical_id)
            .map(|item| item.object)
            .or_else(|| {
                details
                    .source_records
                    .iter()
                    .find(|record| record.object.metadata.id == canonical_id)
                    .map(|record| record.object.clone())
            });
        let Some(object) = object else {
            return Ok(None);
        };
        Ok(Some(ObjectCatalogDetailsLookup {
            object,
            details,
            capabilities: catalog.capabilities(),
            provenance: catalog.provenance()?,
            format_version: catalog.format_version(),
            catalog_version,
        }))
    }

    pub fn search_star_identifiers(
        &self,
        query: &str,
        prefix: bool,
        limit: usize,
    ) -> io::Result<Option<StarIdentifierCatalogSearch>> {
        let Some(catalog) = &self.star_identifiers else {
            return Ok(None);
        };
        let Some((catalog, version)) = catalog.current() else {
            return Ok(None);
        };
        let matches = catalog.search(query, prefix, limit)?;
        Ok(Some(StarIdentifierCatalogSearch {
            matches,
            catalog_version: version,
            catalog_entries: catalog.len(),
            spatial_labels: catalog.label_count(),
            attribution: catalog.attribution().to_owned(),
            epoch: catalog.epoch(),
        }))
    }

    fn warm_catalogs(&self) {
        if let Some(catalog) = &self.objects {
            let _ = catalog.current();
        }
        if let Some(catalog) = &self.star_identifiers {
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

/// Adds the catalog's objects worth drawing to `output` and returns how
/// many of each layer were in the field before selection.
fn append_object_catalog(
    output: &mut Vec<OverlayObject>,
    catalog: &ObjectCatalog,
    wcs: &Wcs,
    dimensions: (u32, u32),
    capture_time: Option<DateTime<Utc>>,
    options: &AnnotationOptions,
    force_transient: bool,
) -> BTreeMap<String, usize> {
    let mut totals = BTreeMap::new();
    let placed_objects = match catalog.objects_in_footprint(wcs, dimensions) {
        Ok(placed_objects) => placed_objects,
        Err(error) => {
            tracing::warn!(%error, "could not query object catalog for solved footprint");
            return totals;
        }
    };
    let wide_field = field_diagonal_deg(wcs, dimensions) > WIDE_FIELD_DIAGONAL_DEG;
    let requested = RequestedObjects::new(&options.requested_objects);
    let mut candidates = Vec::new();
    for placed in placed_objects {
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
        let group = if transient {
            DisplayGroup::Transient
        } else if named_star {
            DisplayGroup::NamedStar
        } else {
            DisplayGroup::DeepSky
        };
        *totals.entry(group.layer().into()).or_insert(0) += 1;
        candidates.push(Candidate {
            requested: requested.matches(&placed.object),
            group,
            placed,
            discovered,
            near_capture,
        });
    }

    for candidate in select_for_display(candidates, options, wide_field) {
        let Candidate {
            placed,
            discovered,
            near_capture,
            group,
            ..
        } = candidate;
        let transient = group == DisplayGroup::Transient;
        let stable_id =
            (!placed.object.metadata.id.is_empty()).then(|| placed.object.metadata.id.clone());
        let outlines = stable_id
            .as_deref()
            .map(|id| projected_outlines(catalog, id, wcs))
            .unwrap_or_default();
        output.push(OverlayObject {
            stable_id,
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
            catalog_source: (!placed.object.metadata.source.is_empty())
                .then_some(placed.object.metadata.source),
            aliases: placed.object.metadata.aliases,
            parent_ids: placed.object.metadata.parent_ids,
            alternate_ids: placed.object.metadata.alternate_ids,
            alternate_sources: placed.object.metadata.alternate_sources,
            ra_deg: Some(placed.object.ra),
            dec_deg: Some(placed.object.dec),
            discovered,
            near_capture,
            distance_au: None,
            motion_arcsec_per_hour: None,
            direction_pa_deg: None,
            direction_angle_deg: None,
            outlines,
        });
    }
    totals
}

fn merge_totals(totals: &mut BTreeMap<String, usize>, more: BTreeMap<String, usize>) {
    for (layer, count) in more {
        *totals.entry(layer).or_insert(0) += count;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DisplayGroup {
    DeepSky,
    NamedStar,
    Transient,
}

impl DisplayGroup {
    fn layer(self) -> &'static str {
        match self {
            Self::DeepSky => "deep_sky",
            Self::NamedStar => "named_stars",
            Self::Transient => "transients",
        }
    }
}

struct Candidate {
    group: DisplayGroup,
    requested: bool,
    placed: seiza::objects::PlacedObject,
    discovered: Option<String>,
    near_capture: Option<bool>,
}

/// Choose which catalog objects in a solved field are worth drawing.
///
/// A wide field can hold tens of thousands of catalog galaxies far smaller
/// than a pixel; drawing them buries the few that matter. Requested objects
/// always pass. Deep-sky objects must span `deep_sky_min_size_px` or carry a
/// common name, and are ranked by apparent size, brightness and naming;
/// named stars and transients are ranked by brightness. Each group is then
/// capped.
fn select_for_display(
    candidates: Vec<Candidate>,
    options: &AnnotationOptions,
    wide_field: bool,
) -> Vec<Candidate> {
    let transient_limit = options
        .transient_mag_limit
        .or(wide_field.then_some(WIDE_FIELD_TRANSIENT_MAG_LIMIT));
    let mut groups: [Vec<(f64, Candidate)>; 3] = Default::default();
    let mut requested = Vec::new();
    for candidate in candidates {
        if candidate.requested {
            requested.push(candidate);
            continue;
        }
        let object = &candidate.placed.object;
        let within = |limit: Option<f32>| match (limit, object.mag) {
            (None, _) => true,
            (Some(limit), Some(mag)) => mag <= limit,
            (Some(_), None) => false,
        };
        let (slot, score) = match candidate.group {
            DisplayGroup::DeepSky => {
                let named = !object.common_name.is_empty() || is_messier(object);
                if !within(options.deep_sky_max_mag)
                    || (candidate.placed.semi_major_px < options.deep_sky_min_size_px && !named)
                {
                    continue;
                }
                (0, deep_sky_score(&candidate.placed))
            }
            DisplayGroup::NamedStar => {
                if !within(options.named_star_mag_limit) {
                    continue;
                }
                // IAU proper names first, then brighter stars.
                let proper = object.metadata.source.contains("IAU");
                (
                    1,
                    f64::from(proper) * 100.0 - f64::from(object.mag.unwrap_or(20.0)),
                )
            }
            DisplayGroup::Transient => {
                if !within(transient_limit) {
                    continue;
                }
                (2, -f64::from(object.mag.unwrap_or(30.0)))
            }
        };
        groups[slot].push((score, candidate));
    }
    let caps = [
        options.max_deep_sky,
        options.max_named_stars,
        options.max_transients,
    ];
    for (group, cap) in groups.iter_mut().zip(caps) {
        group.sort_by(|a, b| b.0.total_cmp(&a.0));
        group.truncate(cap);
    }
    requested
        .into_iter()
        .chain(groups.into_iter().flatten().map(|(_, candidate)| candidate))
        .collect()
}

/// Larger, brighter and named objects rank first. Size dominates: a
/// galaxy a tenth of a pixel across is invisible however bright.
fn deep_sky_score(placed: &seiza::objects::PlacedObject) -> f64 {
    let object = &placed.object;
    let size = placed.semi_major_px.max(0.05).ln();
    let brightness = object
        .mag
        .map_or(0.0, |mag| (14.0 - f64::from(mag)).clamp(0.0, 14.0) * 0.4);
    let named = if object.common_name.is_empty() {
        0.0
    } else {
        2.0
    };
    let messier = if is_messier(object) { 4.0 } else { 0.0 };
    size + brightness + named + messier
}

fn is_messier(object: &SkyObject) -> bool {
    let messier = |name: &str| {
        name.strip_prefix('M')
            .map(str::trim_start)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    };
    messier(&object.name) || object.metadata.aliases.iter().any(|alias| messier(alias))
}

/// Matches catalog objects against the names, designations and stable IDs a
/// user asked for, ignoring case and spacing ("m31", "M 31", "NGC224").
struct RequestedObjects(Vec<String>);

impl RequestedObjects {
    fn new(requests: &[String]) -> Self {
        Self(
            requests
                .iter()
                .map(|request| normalize_designation(request))
                .filter(|request| !request.is_empty())
                .collect(),
        )
    }

    fn matches(&self, object: &SkyObject) -> bool {
        !self.0.is_empty()
            && std::iter::once(&object.name)
                .chain(std::iter::once(&object.common_name))
                .chain(std::iter::once(&object.metadata.id))
                .chain(&object.metadata.aliases)
                .chain(&object.metadata.alternate_ids)
                .map(|name| normalize_designation(name))
                .any(|name| !name.is_empty() && self.0.contains(&name))
    }
}

fn normalize_designation(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn field_diagonal_deg(wcs: &Wcs, dimensions: (u32, u32)) -> f64 {
    (dimensions.0 as f64).hypot(dimensions.1 as f64) * wcs.scale_arcsec_per_px() / 3600.0
}

fn projected_outlines(
    catalog: &ObjectCatalog,
    canonical_id: &str,
    wcs: &Wcs,
) -> Vec<OverlayOutline> {
    let Ok(geometries) = catalog.geometries(canonical_id) else {
        return Vec::new();
    };
    geometries
        .into_iter()
        .filter_map(|geometry| {
            let GeometryData::OutlineSet { level, contours } = geometry.data else {
                return None;
            };
            let contours = contours
                .into_iter()
                .filter_map(|contour| {
                    let points = contour
                        .vertices
                        .into_iter()
                        .map(|(ra, dec)| wcs.world_to_pixel(ra, dec).map(|(x, y)| [x, y]))
                        .collect::<Option<Vec<_>>>()?;
                    let minimum_points = if contour.closed { 3 } else { 2 };
                    (points.len() >= minimum_points).then_some(OverlayContour {
                        closed: contour.closed,
                        points,
                    })
                })
                .collect::<Vec<_>>();
            (!contours.is_empty()).then_some(OverlayOutline {
                geometry_id: geometry.id,
                source_record_id: geometry.source_record_id,
                role: geometry_role_name(geometry.role).into(),
                quality: geometry_quality_name(geometry.quality).into(),
                level,
                contours,
            })
        })
        .collect()
}

fn geometry_role_name(role: GeometryRole) -> &'static str {
    match role {
        GeometryRole::CatalogExtent => "catalog-extent",
        GeometryRole::PreferredRender => "preferred-render",
        GeometryRole::FallbackExtent => "fallback-extent",
        GeometryRole::BrightnessLevel => "brightness-level",
        GeometryRole::Component => "component",
    }
}

fn geometry_quality_name(quality: GeometryQuality) -> &'static str {
    match quality {
        GeometryQuality::Catalog => "catalog",
        GeometryQuality::Curated => "curated",
        GeometryQuality::Estimated => "estimated",
        GeometryQuality::Derived => "derived",
    }
}

fn append_star_identifier_catalog(
    output: &mut Vec<OverlayObject>,
    catalog: &StarIdentifierLayer,
    wcs: &Wcs,
    dimensions: (u32, u32),
    options: &AnnotationOptions,
) {
    let labels = catalog.labels_in_footprint(
        wcs,
        dimensions,
        options.star_identifier_mag_limit,
        options.max_star_identifiers.saturating_mul(8),
    );
    let mut added = 0usize;
    for label in labels {
        // `objects.bin` already carries IAU and bright-star labels. Avoid
        // drawing a second label at the same position when both layers are
        // enabled, while still exposing variables and double stars that only
        // exist in the identifier sidecar.
        let already_labeled = output.iter().any(|object| {
            matches!(object.kind.as_str(), "star" | "double-star")
                && object.ra_deg.zip(object.dec_deg).is_some_and(|(ra, dec)| {
                    angular_separation_deg(ra, dec, label.ra, label.dec) <= 3.0 / 3_600.0
                })
        });
        if already_labeled {
            continue;
        }
        let Some((x, y)) = wcs.world_to_pixel(label.ra, label.dec) else {
            continue;
        };
        output.push(OverlayObject {
            stable_id: None,
            name: label.designation,
            common_name: label.detail,
            kind: "identified-star".into(),
            mag: label.mag,
            x,
            y,
            semi_major_px: 0.0,
            semi_minor_px: 0.0,
            angle_deg: Some(0.0),
            source: Some(format!("star_identifiers:{}", label.catalog.as_str())),
            catalog_source: None,
            aliases: Vec::new(),
            parent_ids: Vec::new(),
            alternate_ids: Vec::new(),
            alternate_sources: Vec::new(),
            ra_deg: Some(label.ra),
            dec_deg: Some(label.dec),
            discovered: None,
            near_capture: None,
            distance_au: None,
            motion_arcsec_per_hour: None,
            direction_pa_deg: None,
            direction_angle_deg: None,
            outlines: Vec::new(),
        });
        added += 1;
        if added >= options.max_star_identifiers {
            break;
        }
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
            stable_id: None,
            name: String::new(),
            common_name: String::new(),
            kind: "field-star".into(),
            mag: Some(star.mag),
            x,
            y,
            semi_major_px: 0.0,
            semi_minor_px: 0.0,
            angle_deg: Some(0.0),
            source: Some("star_catalog".into()),
            catalog_source: None,
            aliases: Vec::new(),
            parent_ids: Vec::new(),
            alternate_ids: Vec::new(),
            alternate_sources: Vec::new(),
            ra_deg: Some(star.ra),
            dec_deg: Some(star.dec),
            discovered: None,
            near_capture: None,
            distance_au: None,
            motion_arcsec_per_hour: None,
            direction_pa_deg: None,
            direction_angle_deg: None,
            outlines: Vec::new(),
        });
        field_count += 1;
        if field_count >= limit {
            break;
        }
    }
}

fn append_minor_bodies(
    output: &mut Vec<OverlayObject>,
    totals: &mut BTreeMap<String, usize>,
    catalog: &MinorBodyCatalog,
    wcs: &Wcs,
    dimensions: (u32, u32),
    capture_time: DateTime<Utc>,
    limit: usize,
) {
    let jd = 2_440_587.5 + capture_time.timestamp_millis() as f64 / 86_400_000.0;
    let mut placed_bodies = catalog.objects_in_footprint(wcs, dimensions, jd, 18.0);
    totals.insert("minor_bodies".into(), placed_bodies.len());
    // Brightest first, so a wide field keeps the bodies that could show.
    placed_bodies.sort_by(|a, b| a.mag.total_cmp(&b.mag));
    placed_bodies.truncate(limit);
    for placed in placed_bodies {
        let kind = match placed.body.kind {
            MinorBodyKind::Comet => "comet",
            MinorBodyKind::Asteroid => "asteroid",
        };
        output.push(OverlayObject {
            stable_id: None,
            name: placed.body.name,
            common_name: format!("V~{:.1}, {:.2} AU", placed.mag, placed.delta_au),
            kind: kind.into(),
            mag: Some(placed.mag as f32),
            x: placed.x,
            y: placed.y,
            semi_major_px: 0.0,
            semi_minor_px: 0.0,
            angle_deg: Some(0.0),
            source: Some("minor_body".into()),
            catalog_source: None,
            aliases: Vec::new(),
            parent_ids: Vec::new(),
            alternate_ids: Vec::new(),
            alternate_sources: Vec::new(),
            ra_deg: Some(placed.ra),
            dec_deg: Some(placed.dec),
            discovered: None,
            near_capture: Some(true),
            distance_au: Some(placed.delta_au),
            motion_arcsec_per_hour: placed.motion_arcsec_per_hour,
            direction_pa_deg: placed.direction_pa_deg,
            direction_angle_deg: placed
                .direction_pa_deg
                .and_then(|angle| direction_image_angle(wcs, placed.ra, placed.dec, angle)),
            outlines: Vec::new(),
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
        "identified-star" => "star_identifiers",
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
    use seiza::{
        minor_bodies::{MinorBody, MinorBodyCatalog, MinorBodyKind},
        objects::{
            ObjectCatalogData, ObjectContour, ObjectDetails, ObjectGeometry, ObjectMetadata,
            SkyObject,
        },
        star_ids::{StarIdentifierCatalogBuilder, StarNameCatalog, StarNameKind},
    };

    #[test]
    fn minor_body_annotations_preserve_apparent_motion_rate() {
        let body = MinorBody {
            kind: MinorBodyKind::Asteroid,
            name: "(12345) Test".into(),
            epoch_jd: 2_460_000.5,
            q_or_a: 2.5,
            eccentricity: 0.2,
            inclination_deg: 12.0,
            node_deg: 45.0,
            arg_perihelion_deg: 110.0,
            mean_anomaly_deg: 30.0,
            h_mag: 10.0,
            slope: 0.15,
        };
        let jd = body.epoch_jd + 100.0;
        let (ra, dec, _, _) = MinorBodyCatalog::position_at(&body, jd).unwrap();
        let wcs = Wcs::from_center_scale_rotation((ra, dec), (500.0, 500.0), 2.0, 0.0, false);
        let capture_time =
            DateTime::from_timestamp_millis(((jd - 2_440_587.5) * 86_400_000.0).round() as i64)
                .unwrap();
        let catalog = MinorBodyCatalog::new(vec![body]);
        let mut objects = Vec::new();

        let mut totals = BTreeMap::new();
        append_minor_bodies(
            &mut objects,
            &mut totals,
            &catalog,
            &wcs,
            (1000, 1000),
            capture_time,
            100,
        );
        assert_eq!(totals["minor_bodies"], 1);

        assert_eq!(objects.len(), 1);
        let object = &objects[0];
        assert_eq!(object.kind, "asteroid");
        assert!(
            object
                .motion_arcsec_per_hour
                .is_some_and(|speed| speed > 1.0)
        );
        assert!(object.direction_pa_deg.is_some());
        assert!(object.direction_angle_deg.is_some());
        assert!(serde_json::to_value(object).unwrap()["motion_arcsec_per_hour"].is_number());
    }

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

    fn candidate(
        group: DisplayGroup,
        name: &str,
        common_name: &str,
        mag: Option<f32>,
        semi_major_px: f64,
    ) -> Candidate {
        Candidate {
            group,
            requested: false,
            placed: seiza::objects::PlacedObject {
                object: SkyObject {
                    kind: ObjectKind::Galaxy,
                    ra: 10.0,
                    dec: 20.0,
                    mag,
                    major_arcmin: None,
                    minor_arcmin: None,
                    position_angle_deg: None,
                    name: name.into(),
                    common_name: common_name.into(),
                    metadata: ObjectMetadata::default(),
                },
                x: 10.0,
                y: 10.0,
                semi_major_px,
                semi_minor_px: semi_major_px,
                angle_deg: Some(0.0),
            },
            discovered: None,
            near_capture: None,
        }
    }

    fn names(selected: &[Candidate]) -> Vec<&str> {
        selected
            .iter()
            .map(|candidate| candidate.placed.object.name.as_str())
            .collect()
    }

    #[test]
    fn wide_fields_keep_visible_and_named_objects_only() {
        let mut candidates = vec![
            candidate(DisplayGroup::DeepSky, "PGC 1", "", Some(15.0), 0.2),
            candidate(DisplayGroup::DeepSky, "UGC 2", "", None, 0.4),
            candidate(
                DisplayGroup::DeepSky,
                "NGC 457",
                "Owl Cluster",
                Some(6.4),
                0.9,
            ),
            candidate(DisplayGroup::DeepSky, "M 76", "", Some(10.1), 0.5),
            candidate(DisplayGroup::DeepSky, "IC 1805", "", Some(6.5), 25.0),
            candidate(
                DisplayGroup::DeepSky,
                "M 31",
                "Andromeda Galaxy",
                Some(3.4),
                75.0,
            ),
        ];
        // Thousands of tiny galaxies must not crowd out the visible ones.
        candidates.extend((0..5_000).map(|i| {
            candidate(
                DisplayGroup::DeepSky,
                &format!("PGC {i}"),
                "",
                Some(15.0),
                0.3,
            )
        }));
        let options = AnnotationOptions {
            max_deep_sky: 3,
            ..AnnotationOptions::default()
        };
        let selected = select_for_display(candidates, &options, true);
        assert_eq!(names(&selected), ["M 31", "IC 1805", "NGC 457"]);

        // Uncapped, sub-pixel objects without a name still stay out.
        let selected = select_for_display(
            vec![
                candidate(DisplayGroup::DeepSky, "PGC 1", "", Some(12.0), 0.2),
                candidate(
                    DisplayGroup::DeepSky,
                    "NGC 457",
                    "Owl Cluster",
                    Some(6.4),
                    0.9,
                ),
                candidate(DisplayGroup::DeepSky, "NGC 7380", "", Some(7.2), 10.6),
            ],
            &AnnotationOptions::default(),
            true,
        );
        assert_eq!(names(&selected), ["NGC 7380", "NGC 457"]);
    }

    #[test]
    fn requested_objects_always_show_and_come_first() {
        let requested = RequestedObjects::new(&["ngc7635".into(), " m 31 ".into()]);
        let mut bubble = candidate(
            DisplayGroup::DeepSky,
            "NGC 7635",
            "Bubble Nebula",
            Some(10.0),
            0.1,
        );
        let mut tiny = candidate(DisplayGroup::DeepSky, "PGC 9", "", Some(17.0), 0.01);
        tiny.placed.object.metadata.aliases = vec!["M31".into()];
        bubble.requested = requested.matches(&bubble.placed.object);
        tiny.requested = requested.matches(&tiny.placed.object);
        assert!(bubble.requested && tiny.requested);
        let other = candidate(DisplayGroup::DeepSky, "IC 1805", "", Some(6.5), 25.0);
        assert!(!requested.matches(&other.placed.object));

        let options = AnnotationOptions {
            max_deep_sky: 1,
            deep_sky_max_mag: Some(8.0),
            ..AnnotationOptions::default()
        };
        let selected = select_for_display(vec![other, tiny, bubble], &options, true);
        assert_eq!(names(&selected), ["PGC 9", "NGC 7635", "IC 1805"]);
        assert!(!RequestedObjects::new(&[]).matches(&selected[0].placed.object));
    }

    #[test]
    fn wide_fields_drop_faint_transients_and_rank_stars_by_brightness() {
        let transients = || {
            vec![
                candidate(DisplayGroup::Transient, "SN 2026a", "", Some(17.5), 0.0),
                candidate(DisplayGroup::Transient, "Nova Cas", "", Some(9.0), 0.0),
            ]
        };
        let options = AnnotationOptions::default();
        assert_eq!(
            names(&select_for_display(transients(), &options, true)),
            ["Nova Cas"]
        );
        // Narrow fields keep both unless asked otherwise.
        assert_eq!(
            names(&select_for_display(transients(), &options, false)),
            ["Nova Cas", "SN 2026a"]
        );
        assert_eq!(
            names(&select_for_display(
                transients(),
                &AnnotationOptions {
                    transient_mag_limit: Some(20.0),
                    ..AnnotationOptions::default()
                },
                true
            )),
            ["Nova Cas", "SN 2026a"]
        );

        let mut proper = candidate(DisplayGroup::NamedStar, "Segin", "", Some(3.4), 0.0);
        proper.placed.object.metadata.source = "IAU Catalog of Star Names".into();
        let stars = vec![
            candidate(DisplayGroup::NamedStar, "HR 1", "", Some(5.9), 0.0),
            candidate(DisplayGroup::NamedStar, "HR 2", "", Some(2.1), 0.0),
            proper,
        ];
        let selected = select_for_display(
            stars,
            &AnnotationOptions {
                max_named_stars: 2,
                ..AnnotationOptions::default()
            },
            true,
        );
        assert_eq!(names(&selected), ["Segin", "HR 2"]);
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
            metadata: ObjectMetadata::default(),
        };
        ObjectCatalog::new(vec![object("M 1", 10.0)])
            .write_to(&path)
            .unwrap();
        let engine = AnnotationEngine::new(None, None, Some(&path), None, None, None);
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
                sip: None,
            },
            footprint: [[0.0; 2]; 4],
            objects: Vec::new(),
            catalog_version: None,
            capture_time: None,
            statistics: None,
            pixel_coordinates: None,
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

    #[test]
    fn v4_metadata_and_outlines_are_projected_without_inventing_an_angle() {
        let path = std::env::temp_dir().join(format!(
            "seiza-server-annotation-v4-{}.bin",
            uuid::Uuid::now_v7()
        ));
        let object = SkyObject {
            kind: ObjectKind::Nebula,
            ra: 10.0,
            dec: 20.0,
            mag: None,
            major_arcmin: Some(30.0),
            minor_arcmin: Some(10.0),
            position_angle_deg: None,
            name: "NGC 1".into(),
            common_name: "Test Nebula".into(),
            metadata: ObjectMetadata {
                id: "openngc:NGC1".into(),
                source: "OpenNGC".into(),
                aliases: vec!["Test 1".into()],
                ..ObjectMetadata::default()
            },
        };
        let mut details = ObjectDetails::from_canonical(&object);
        details.geometries.push(ObjectGeometry {
            id: "openngc:NGC1#outline-1".into(),
            source_record_id: "openngc:NGC1".into(),
            role: GeometryRole::BrightnessLevel,
            quality: GeometryQuality::Catalog,
            method: "OpenNGC outline".into(),
            evidence: String::new(),
            data: GeometryData::OutlineSet {
                level: Some("1".into()),
                contours: vec![ObjectContour {
                    closed: true,
                    vertices: vec![(9.99, 19.99), (10.01, 19.99), (10.0, 20.01)],
                }],
            },
        });
        ObjectCatalog::from_data(ObjectCatalogData {
            objects: vec![object],
            details: vec![details],
            provenance: Default::default(),
        })
        .unwrap()
        .write_to(&path)
        .unwrap();
        let engine = AnnotationEngine::new(None, None, Some(&path), None, None, None);
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
                sip: None,
            },
            footprint: [[0.0; 2]; 4],
            objects: Vec::new(),
            catalog_version: None,
            capture_time: None,
            statistics: None,
            pixel_coordinates: None,
        };

        let annotation = engine.annotate(1, &solution, None, &AnnotationOptions::default());
        assert_eq!(annotation.objects.len(), 1);
        assert_eq!(
            annotation.objects[0].stable_id.as_deref(),
            Some("openngc:NGC1")
        );
        assert_eq!(
            annotation.objects[0].catalog_source.as_deref(),
            Some("OpenNGC")
        );
        assert_eq!(annotation.objects[0].angle_deg, None);
        assert_eq!(annotation.objects[0].outlines.len(), 1);
        assert_eq!(
            annotation.objects[0].outlines[0].contours[0].points.len(),
            3
        );

        let lookup = engine.object_details("openngc:NGC1").unwrap().unwrap();
        assert_eq!(lookup.format_version, 4);
        assert!(lookup.capabilities.outlines);
        assert_eq!(lookup.details.source_records[0].source, "OpenNGC");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn stellar_identifier_labels_are_an_independent_annotation_layer() {
        let path = std::env::temp_dir().join(format!(
            "seiza-server-annotation-star-ids-{}.bin",
            uuid::Uuid::now_v7()
        ));
        let mut builder = StarIdentifierCatalogBuilder::new(2025.5, "test identifiers");
        builder
            .add_name(
                StarNameCatalog::GeneralCatalogOfVariableStars,
                StarNameKind::VariableStar,
                "RR Lyr",
                "gcvs:RR-Lyr",
                "RRAB",
                10.0,
                20.0,
                Some(7.1),
            )
            .unwrap();
        builder.write_to(&path).unwrap();
        let engine = AnnotationEngine::new(None, None, None, Some(&path), None, None);
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
                sip: None,
            },
            footprint: [[0.0; 2]; 4],
            objects: Vec::new(),
            catalog_version: None,
            capture_time: None,
            statistics: None,
            pixel_coordinates: None,
        };

        let hidden = engine.annotate(1, &solution, None, &AnnotationOptions::default());
        assert!(hidden.available["star_identifiers"]);
        assert_eq!(hidden.counts["star_identifiers"], 0);

        let visible = engine.annotate(
            1,
            &solution,
            None,
            &AnnotationOptions {
                star_identifiers: true,
                ..AnnotationOptions::default()
            },
        );
        assert_eq!(visible.counts["star_identifiers"], 1);
        assert_eq!(visible.objects[0].kind, "identified-star");
        assert_eq!(visible.objects[0].name, "RR Lyr");
        std::fs::remove_file(path).unwrap();
    }
}
