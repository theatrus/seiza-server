use crate::{
    models::{
        AnnotationResponse, ConstellationResponse, OverlayContour, OverlayObject, OverlayOutline,
        SolutionResponse,
    },
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
    pub constellations: bool,
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
    /// Also return deep-sky objects with no catalog size, which the
    /// minimum size above would leave out, marked `unsized` and capped
    /// apart so they never displace an object of known size.
    pub unsized_deep_sky: bool,
    /// Most unsized deep-sky objects to return when they are asked for.
    pub max_unsized_deep_sky: usize,
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
            // Projected only when a client asks (the annotations route does
            // by default), not for solution metadata that never draws them.
            constellations: false,
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
            unsized_deep_sky: false,
            max_unsized_deep_sky: 100,
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
        // Catalogs and figures are placed in the picture's own frame, so
        // nothing lands on the flat bands around it, then moved back.
        let mut wcs = solution.wcs.to_seiza();
        let (dimensions, offset) = match solution.content_region {
            Some(region) => {
                wcs.crpix.0 -= region.x as f64;
                wcs.crpix.1 -= region.y as f64;
                (
                    (region.width, region.height),
                    (region.x as f64, region.y as f64),
                )
            }
            None => ((solution.image_width, solution.image_height), (0.0, 0.0)),
        };
        let configured = self.is_configured();
        let mut objects = Vec::new();
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
            ("constellations".into(), true),
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

        let mut constellations = if options.constellations {
            project_constellations(&wcs, dimensions)
        } else {
            Vec::new()
        };
        shift_to_image(&mut objects, &mut constellations, offset);
        if !configured {
            // Without catalogs, the solution's own objects stand in; they
            // are already in the image's frame.
            objects.splice(0..0, solution.objects.iter().cloned());
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
        if options.unsized_deep_sky {
            available.insert("unsized_deep_sky".into(), available["deep_sky"]);
            counts.insert("unsized_deep_sky".into(), 0);
        }
        for object in &objects {
            // Objects of unknown size have their own count, so the deep-sky
            // count stays what a client shows by default.
            if object.size_unknown {
                *counts.entry("unsized_deep_sky".into()).or_insert(0) += 1;
                continue;
            }
            available.insert(layer_name(&object.kind).to_owned(), true);
            *counts
                .entry(layer_name(&object.kind).to_owned())
                .or_insert(0) += 1;
            if object.kind == "transient" && object.near_capture == Some(false) {
                *counts.entry("historical_transients".into()).or_insert(0) += 1;
            }
        }
        counts.insert("constellations".into(), constellations.len());
        let constellation_attribution =
            (!constellations.is_empty()).then(|| seiza::constellations::ATTRIBUTION.to_owned());
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
            constellations,
            constellation_attribution,
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

/// Move objects and figures placed in a content region's frame by its
/// offset, back into the image's.
fn shift_to_image(
    objects: &mut [OverlayObject],
    constellations: &mut [ConstellationResponse],
    (dx, dy): (f64, f64),
) {
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    for object in objects {
        object.x += dx;
        object.y += dy;
        for contour in object
            .outlines
            .iter_mut()
            .flat_map(|outline| outline.contours.iter_mut())
        {
            for point in &mut contour.points {
                point[0] += dx;
                point[1] += dy;
            }
        }
    }
    for constellation in constellations {
        for point in constellation.lines.iter_mut().flatten() {
            point[0] += dx;
            point[1] += dy;
        }
        if let Some(label) = &mut constellation.label {
            label[0] += dx;
            label[1] += dy;
        }
    }
}

/// The constellation figures crossing the image, as pixel polylines rounded
/// to a tenth of a pixel to keep the response small.
fn project_constellations(wcs: &Wcs, dimensions: (u32, u32)) -> Vec<ConstellationResponse> {
    let round = |(x, y): (f64, f64)| [(x * 10.0).round() / 10.0, (y * 10.0).round() / 10.0];
    seiza::constellations::project_figures(wcs, dimensions)
        .into_iter()
        .map(|figure| ConstellationResponse {
            abbreviation: figure.abbr.to_owned(),
            name: figure.name.to_owned(),
            lines: figure
                .polylines
                .into_iter()
                .map(|line| line.into_iter().map(round).collect())
                .collect(),
            label: figure.label.map(round),
        })
        .collect()
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
        if group == DisplayGroup::DeepSky && is_unsized(&placed.object) {
            *totals.entry("unsized_deep_sky".into()).or_insert(0) += 1;
        }
        candidates.push(Candidate {
            requested: requested.matches(&placed.object),
            size_unknown: false,
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
            size_unknown,
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
            size_unknown,
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
    /// Kept only because unsized deep-sky objects were asked for.
    size_unknown: bool,
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
    // Deep sky, named stars, transients, and deep sky of unknown size.
    let mut groups: [Vec<(f64, Candidate)>; 4] = Default::default();
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
                if !within(options.deep_sky_max_mag) {
                    continue;
                }
                if candidate.placed.semi_major_px < options.deep_sky_min_size_px && !named {
                    // An unknown size is not a small one: such objects come
                    // back on request, in a group of their own.
                    if !(options.unsized_deep_sky && is_unsized(object)) {
                        continue;
                    }
                    let mut candidate = candidate;
                    candidate.size_unknown = true;
                    let score = deep_sky_score(&candidate.placed);
                    groups[3].push((score, candidate));
                    continue;
                } else {
                    (0, deep_sky_score(&candidate.placed))
                }
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
        options.max_unsized_deep_sky,
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

/// No catalog gives the object's extent, so its placed size is zero.
fn is_unsized(object: &SkyObject) -> bool {
    object.major_arcmin.is_none_or(|major| major <= 0.0)
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
            size_unknown: false,
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
            size_unknown: false,
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
            size_unknown: false,
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
    use crate::models::{ContentRegion, WcsResponse};
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
            size_unknown: false,
            placed: seiza::objects::PlacedObject {
                object: SkyObject {
                    kind: ObjectKind::Galaxy,
                    ra: 10.0,
                    dec: 20.0,
                    mag,
                    // A placed size of zero stands for no catalog size.
                    major_arcmin: (semi_major_px > 0.0).then_some(1.0),
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
    fn objects_of_unknown_size_come_back_only_on_request_and_apart() {
        let deep = |name: &str, size: f64| candidate(DisplayGroup::DeepSky, name, "", None, size);
        let candidates = || {
            vec![
                deep("Big", 40.0),
                deep("Medium", 10.0),
                // Known to be smaller than a marker: always left out.
                deep("Tiny", 0.5),
                // No catalog size at all.
                deep("Abell 56", 0.0),
                deep("PN G036.0-01.4", 0.0),
            ]
        };
        let defaults = AnnotationOptions {
            max_deep_sky: 2,
            ..AnnotationOptions::default()
        };
        let selected = select_for_display(candidates(), &defaults, false);
        assert_eq!(names(&selected), ["Big", "Medium"]);
        assert!(selected.iter().all(|candidate| !candidate.size_unknown));

        let asked = AnnotationOptions {
            unsized_deep_sky: true,
            ..defaults.clone()
        };
        // Both full-size objects stay: the unsized have a cap of their own.
        let selected = select_for_display(candidates(), &asked, false);
        assert_eq!(
            names(&selected),
            ["Big", "Medium", "Abell 56", "PN G036.0-01.4"]
        );
        assert_eq!(
            selected
                .iter()
                .filter(|candidate| candidate.size_unknown)
                .count(),
            2
        );
        let capped = AnnotationOptions {
            max_unsized_deep_sky: 1,
            ..asked
        };
        assert_eq!(
            names(&select_for_display(candidates(), &capped, false)),
            ["Big", "Medium", "Abell 56"]
        );
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
            content_region: None,
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
    fn wide_fields_carry_constellation_figures_with_their_credit() {
        // 2000×1500 at 0.03°/px, centred between Cassiopeia and Perseus.
        let solution = SolutionResponse {
            center_ra_deg: 20.0,
            center_dec_deg: 60.0,
            pixel_scale_arcsec_per_pixel: 108.0,
            matched_stars: 50,
            rms_arcsec: 60.0,
            image_width: 2000,
            image_height: 1500,
            wcs: WcsResponse {
                crval: [20.0, 60.0],
                crpix: [1000.0, 750.0],
                cd: [[-0.03, 0.0], [0.0, 0.03]],
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
            content_region: None,
        };
        let engine = AnnotationEngine::new(None, None, None, None, None, None);
        let wanted = AnnotationOptions {
            constellations: true,
            ..AnnotationOptions::default()
        };
        // Solution metadata does not pay for figures it never draws.
        assert!(
            engine
                .annotate(1, &solution, None, &AnnotationOptions::default())
                .constellations
                .is_empty()
        );
        let annotations = engine.annotate(1, &solution, None, &wanted);
        let cassiopeia = annotations
            .constellations
            .iter()
            .find(|figure| figure.abbreviation == "Cas")
            .expect("Cassiopeia is in the field");
        assert_eq!(cassiopeia.name, "Cassiopeia");
        assert!(cassiopeia.label.is_some());
        for point in cassiopeia.lines.iter().flatten() {
            assert!((0.0..=2000.0).contains(&point[0]) && (0.0..=1500.0).contains(&point[1]));
            // Coordinates are rounded to a tenth of a pixel.
            assert_eq!((point[0] * 10.0).fract(), 0.0);
        }
        assert_eq!(
            annotations.counts["constellations"],
            annotations.constellations.len()
        );
        assert!(
            annotations
                .constellation_attribution
                .as_deref()
                .is_some_and(|credit| credit.contains("CC BY 4.0"))
        );
        let json = serde_json::to_value(&annotations).unwrap();
        assert!(json["constellations"][0]["lines"][0][0].is_array());

        // Straight lines come back as their two endpoints.
        assert!(cassiopeia.lines.iter().all(|line| line.len() == 2));

        // A 0.3 arcsec/px telescope frame on M31 crosses no figure: nothing
        // is sent, credit included.
        let narrow = SolutionResponse {
            center_ra_deg: 10.68,
            center_dec_deg: 41.27,
            pixel_scale_arcsec_per_pixel: 0.3,
            image_width: 4000,
            image_height: 3000,
            wcs: WcsResponse {
                crval: [10.68, 41.27],
                crpix: [2000.0, 1500.0],
                cd: [[-0.3 / 3600.0, 0.0], [0.0, 0.3 / 3600.0]],
                ..solution.wcs.clone()
            },
            ..solution.clone()
        };
        let narrow = engine.annotate(1, &narrow, None, &wanted);
        assert!(narrow.constellations.is_empty());
        let json = serde_json::to_value(&narrow).unwrap();
        assert!(json.get("constellations").is_none());
        assert!(json.get("constellation_attribution").is_none());
    }

    #[test]
    fn figures_stay_on_the_picture_inside_flat_bands() {
        // The wide field above, as a screenshot that keeps the picture in
        // rows 300 to 1199 and has bars above and below.
        let full = SolutionResponse {
            center_ra_deg: 20.0,
            center_dec_deg: 60.0,
            pixel_scale_arcsec_per_pixel: 108.0,
            matched_stars: 50,
            rms_arcsec: 60.0,
            image_width: 2000,
            image_height: 1500,
            wcs: WcsResponse {
                crval: [20.0, 60.0],
                crpix: [1000.0, 750.0],
                cd: [[-0.03, 0.0], [0.0, 0.03]],
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
            content_region: None,
        };
        let banded = SolutionResponse {
            content_region: Some(ContentRegion {
                x: 0,
                y: 300,
                width: 2000,
                height: 900,
            }),
            ..full.clone()
        };
        let engine = AnnotationEngine::new(None, None, None, None, None, None);
        let wanted = AnnotationOptions {
            constellations: true,
            ..AnnotationOptions::default()
        };
        let whole = engine.annotate(1, &full, None, &wanted);
        let picture = engine.annotate(1, &banded, None, &wanted);
        assert!(!picture.constellations.is_empty());
        for figure in &picture.constellations {
            for point in figure.lines.iter().flatten().chain(&figure.label) {
                assert!(
                    (-0.5..=1999.5).contains(&point[0]) && (299.5..=1199.5).contains(&point[1]),
                    "{} at {point:?} lies on a band",
                    figure.abbreviation
                );
            }
        }
        // Each figure the picture shows runs along the same sky as in the
        // whole image: its points lie on the whole image's lines.
        let on_line = |point: [f64; 2], lines: &[Vec<[f64; 2]>]| {
            lines.iter().any(|line| {
                line.windows(2).any(|pair| {
                    let (a, b) = (pair[0], pair[1]);
                    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                    let t = (((point[0] - a[0]) * dx + (point[1] - a[1]) * dy)
                        / (dx * dx + dy * dy))
                        .clamp(0.0, 1.0);
                    (a[0] + t * dx - point[0]).hypot(a[1] + t * dy - point[1]) < 0.5
                })
            })
        };
        for figure in &picture.constellations {
            let same = whole
                .constellations
                .iter()
                .find(|other| other.abbreviation == figure.abbreviation)
                .expect("a figure on the picture is in the whole image");
            for point in figure.lines.iter().flatten() {
                assert!(
                    on_line(*point, &same.lines),
                    "{} moved",
                    figure.abbreviation
                );
            }
        }
        // A figure only on the bands is left out.
        assert!(picture.constellations.len() <= whole.constellations.len());
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
            content_region: None,
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
            content_region: None,
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
