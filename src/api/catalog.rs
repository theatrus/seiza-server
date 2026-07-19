//! Sky-object catalog and star-identifier query endpoints. Split from the
//! parent module, which keeps the solve, upload, and worker surface.

use super::*;

pub(super) const DEFAULT_CATALOG_QUERY_LIMIT: usize = 100;
pub(super) const DEFAULT_CATALOG_SEARCH_LIMIT: usize = 20;
pub(super) const MAX_CATALOG_QUERY_LIMIT: usize = 1_000;
pub(super) const MAX_CATALOG_SEARCH_LIMIT: usize = 100;

#[derive(Debug, Deserialize)]
pub(super) struct CatalogObjectsQuery {
    pub(super) ra: f64,
    pub(super) dec: f64,
    pub(super) radius: f64,
    /// Comma-separated ObjectKind names, such as `galaxy,nebula`.
    pub(super) kinds: Option<String>,
    pub(super) max_mag: Option<f32>,
    pub(super) min_major_arcmin: Option<f32>,
    #[serde(default)]
    pub(super) common_name_only: bool,
    #[serde(default = "default_true")]
    pub(super) include_extent_overlaps: bool,
    #[serde(default = "default_catalog_query_limit")]
    pub(super) limit: usize,
    #[serde(default = "default_catalog_sort")]
    pub(super) sort: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct CatalogObjectSearchQuery {
    pub(super) q: String,
    #[serde(default)]
    pub(super) prefix: bool,
    #[serde(default = "default_catalog_search_limit")]
    pub(super) limit: usize,
}

#[derive(Debug, Deserialize)]
pub(super) struct StarIdentifierSearchQuery {
    pub(super) q: String,
    #[serde(default)]
    pub(super) prefix: bool,
    #[serde(default = "default_catalog_search_limit")]
    pub(super) limit: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct CatalogObjectsResponse {
    pub(super) catalog_version: String,
    pub(super) catalog_objects: usize,
    pub(super) returned: usize,
    pub(super) objects: Vec<CatalogObjectHitResponse>,
}

#[derive(Debug, Serialize)]
pub(super) struct CatalogObjectSearchResponse {
    pub(super) catalog_version: String,
    pub(super) catalog_objects: usize,
    pub(super) returned: usize,
    pub(super) matches: Vec<CatalogObjectNameResponse>,
}

#[derive(Debug, Serialize)]
pub(super) struct CatalogObjectDetailsResponse {
    pub(super) catalog_version: String,
    pub(super) format_version: u8,
    pub(super) capabilities: CatalogCapabilitiesResponse,
    pub(super) object: CatalogObjectResponse,
    pub(super) details: ObjectDetails,
    pub(super) provenance: Option<ObjectCatalogProvenance>,
}

#[derive(Debug, Serialize)]
pub(super) struct CatalogCapabilitiesResponse {
    pub(super) source_records: bool,
    pub(super) relations: bool,
    pub(super) selections: bool,
    pub(super) ellipses: bool,
    pub(super) outlines: bool,
    pub(super) provenance: bool,
    pub(super) unknown_optional_sections: usize,
}

impl From<ObjectCatalogCapabilities> for CatalogCapabilitiesResponse {
    fn from(capabilities: ObjectCatalogCapabilities) -> Self {
        Self {
            source_records: capabilities.source_records,
            relations: capabilities.relations,
            selections: capabilities.selections,
            ellipses: capabilities.ellipses,
            outlines: capabilities.outlines,
            provenance: capabilities.provenance,
            unknown_optional_sections: capabilities.unknown_optional_sections,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct StarIdentifierSearchResponse {
    pub(super) catalog_version: String,
    pub(super) catalog_entries: usize,
    pub(super) spatial_labels: usize,
    pub(super) attribution: String,
    pub(super) epoch: f64,
    pub(super) returned: usize,
    pub(super) matches: Vec<StarIdentifierMatch>,
}

#[derive(Debug, Serialize)]
pub(super) struct CatalogObjectHitResponse {
    #[serde(flatten)]
    pub(super) object: CatalogObjectResponse,
    pub(super) center_inside: bool,
    pub(super) extent_only: bool,
    pub(super) distance_from_center_deg: f64,
    pub(super) predicted_prominence: f64,
}

#[derive(Debug, Serialize)]
pub(super) struct CatalogObjectNameResponse {
    pub(super) matched_name: String,
    #[serde(flatten)]
    pub(super) object: CatalogObjectResponse,
}

#[derive(Debug, Serialize)]
pub(super) struct CatalogObjectResponse {
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) common_name: String,
    pub(super) id: String,
    pub(super) source: String,
    pub(super) aliases: Vec<String>,
    pub(super) parent_ids: Vec<String>,
    pub(super) alternate_ids: Vec<String>,
    pub(super) alternate_sources: Vec<String>,
    pub(super) ra_deg: f64,
    pub(super) dec_deg: f64,
    pub(super) mag: Option<f32>,
    pub(super) major_arcmin: Option<f32>,
    pub(super) minor_arcmin: Option<f32>,
    pub(super) position_angle_deg: Option<f32>,
}

impl From<SkyObject> for CatalogObjectResponse {
    fn from(object: SkyObject) -> Self {
        Self {
            kind: object.kind.as_str().into(),
            name: object.name,
            common_name: object.common_name,
            id: object.metadata.id,
            source: object.metadata.source,
            aliases: object.metadata.aliases,
            parent_ids: object.metadata.parent_ids,
            alternate_ids: object.metadata.alternate_ids,
            alternate_sources: object.metadata.alternate_sources,
            ra_deg: object.ra,
            dec_deg: object.dec,
            mag: object.mag,
            major_arcmin: object.major_arcmin,
            minor_arcmin: object.minor_arcmin,
            position_angle_deg: object.position_angle_deg,
        }
    }
}

impl From<ObjectHit> for CatalogObjectHitResponse {
    fn from(hit: ObjectHit) -> Self {
        Self {
            object: hit.object.into(),
            center_inside: hit.center_inside,
            extent_only: hit.extent_only,
            distance_from_center_deg: hit.distance_from_center_deg,
            predicted_prominence: hit.predicted_prominence,
        }
    }
}

impl From<ObjectNameMatch> for CatalogObjectNameResponse {
    fn from(item: ObjectNameMatch) -> Self {
        Self {
            matched_name: item.matched_name,
            object: item.object.into(),
        }
    }
}

pub(super) async fn get_catalog_objects(
    State(state): State<AppState>,
    Query(params): Query<CatalogObjectsQuery>,
) -> Result<Json<CatalogObjectsResponse>, ApiError> {
    validate_catalog_limit(params.limit, MAX_CATALOG_QUERY_LIMIT)?;
    if params.max_mag.is_some_and(|value| !value.is_finite()) {
        return Err(ApiError::bad_request("max_mag must be finite"));
    }
    if params
        .min_major_arcmin
        .is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        return Err(ApiError::bad_request(
            "min_major_arcmin must be finite and non-negative",
        ));
    }
    let query = ObjectQuery {
        kinds: parse_object_kinds(params.kinds.as_deref())?,
        max_mag: params.max_mag,
        min_major_arcmin: params.min_major_arcmin,
        common_name_only: params.common_name_only,
        include_extent_overlaps: params.include_extent_overlaps,
        limit: Some(params.limit),
        sort: parse_object_sort(&params.sort)?,
    };
    let region = SkyRegion::Cone {
        center: (params.ra, params.dec),
        radius_deg: params.radius,
    };
    let (objects, catalog_version, catalog_objects) = state
        .annotations
        .query_objects(&region, &query)
        .map_err(catalog_query_error)?
        .ok_or_else(catalog_unavailable)?;
    let objects: Vec<_> = objects.into_iter().map(Into::into).collect();
    Ok(Json(CatalogObjectsResponse {
        catalog_version,
        catalog_objects,
        returned: objects.len(),
        objects,
    }))
}

pub(super) async fn search_catalog_objects(
    State(state): State<AppState>,
    Query(params): Query<CatalogObjectSearchQuery>,
) -> Result<Json<CatalogObjectSearchResponse>, ApiError> {
    validate_catalog_limit(params.limit, MAX_CATALOG_SEARCH_LIMIT)?;
    let designation = params.q.trim();
    if designation.is_empty() {
        return Err(ApiError::bad_request("q must not be empty"));
    }
    if designation.len() > 256 {
        return Err(ApiError::bad_request("q must be at most 256 bytes"));
    }
    let (matches, catalog_version, catalog_objects) = state
        .annotations
        .search_objects(designation, params.prefix, params.limit)
        .map_err(ApiError::internal)?
        .ok_or_else(catalog_unavailable)?;
    let matches: Vec<_> = matches.into_iter().map(Into::into).collect();
    Ok(Json(CatalogObjectSearchResponse {
        catalog_version,
        catalog_objects,
        returned: matches.len(),
        matches,
    }))
}

pub(super) async fn get_catalog_object_details(
    State(state): State<AppState>,
    Path(canonical_id): Path<String>,
) -> Result<Json<CatalogObjectDetailsResponse>, ApiError> {
    if canonical_id.trim().is_empty() {
        return Err(ApiError::bad_request("canonical object ID is required"));
    }
    let lookup = state
        .annotations
        .object_details(&canonical_id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found_message("catalog object not found"))?;
    Ok(Json(CatalogObjectDetailsResponse {
        catalog_version: lookup.catalog_version,
        format_version: lookup.format_version,
        capabilities: lookup.capabilities.into(),
        object: lookup.object.into(),
        details: lookup.details,
        provenance: lookup.provenance,
    }))
}

pub(super) async fn search_star_identifiers(
    State(state): State<AppState>,
    Query(params): Query<StarIdentifierSearchQuery>,
) -> Result<Json<StarIdentifierSearchResponse>, ApiError> {
    validate_catalog_limit(params.limit, MAX_CATALOG_SEARCH_LIMIT)?;
    let query = params.q.trim();
    if query.is_empty() {
        return Err(ApiError::bad_request("q must not be empty"));
    }
    if query.len() > 256 {
        return Err(ApiError::bad_request("q must be at most 256 bytes"));
    }
    let result = state
        .annotations
        .search_star_identifiers(query, params.prefix, params.limit)
        .map_err(ApiError::internal)?
        .ok_or_else(star_identifier_catalog_unavailable)?;
    Ok(Json(StarIdentifierSearchResponse {
        catalog_version: result.catalog_version,
        catalog_entries: result.catalog_entries,
        spatial_labels: result.spatial_labels,
        attribution: result.attribution,
        epoch: result.epoch,
        returned: result.matches.len(),
        matches: result.matches,
    }))
}

pub(super) fn catalog_unavailable() -> ApiError {
    ApiError::service_unavailable(
        "object catalog is not configured or could not be opened; set SEIZA_OBJECT_DATA",
    )
}

pub(super) fn star_identifier_catalog_unavailable() -> ApiError {
    ApiError::service_unavailable(
        "stellar identifier catalog is not configured or could not be opened; set SEIZA_STAR_IDENTIFIER_DATA",
    )
}

pub(super) fn catalog_query_error(error: ObjectQueryError) -> ApiError {
    match error {
        ObjectQueryError::Catalog(_) => ApiError::internal(error),
        _ => ApiError::bad_request(error),
    }
}

pub(super) fn validate_catalog_limit(limit: usize, maximum: usize) -> Result<(), ApiError> {
    if !(1..=maximum).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {maximum}"
        )));
    }
    Ok(())
}

pub(super) fn parse_object_kinds(value: Option<&str>) -> Result<Vec<ObjectKind>, ApiError> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(parse_object_kind)
        .collect()
}

pub(super) fn parse_object_kind(value: &str) -> Result<ObjectKind, ApiError> {
    let normalized = value.to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "galaxy" => Ok(ObjectKind::Galaxy),
        "open-cluster" => Ok(ObjectKind::OpenCluster),
        "globular-cluster" => Ok(ObjectKind::GlobularCluster),
        "nebula" => Ok(ObjectKind::Nebula),
        "planetary-nebula" => Ok(ObjectKind::PlanetaryNebula),
        "hii" | "hii-region" => Ok(ObjectKind::HiiRegion),
        "supernova-remnant" => Ok(ObjectKind::SupernovaRemnant),
        "dark-nebula" => Ok(ObjectKind::DarkNebula),
        "cluster-nebula" | "cluster-with-nebula" => Ok(ObjectKind::ClusterWithNebula),
        "star" => Ok(ObjectKind::Star),
        "double-star" => Ok(ObjectKind::DoubleStar),
        "association" => Ok(ObjectKind::Association),
        "other" => Ok(ObjectKind::Other),
        "transient" => Ok(ObjectKind::Transient),
        _ => Err(ApiError::bad_request(format!(
            "unsupported object kind `{value}`"
        ))),
    }
}

pub(super) fn parse_object_sort(value: &str) -> Result<ObjectSort, ApiError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "prominence" => Ok(ObjectSort::Prominence),
        "size" => Ok(ObjectSort::Size),
        "magnitude" | "mag" => Ok(ObjectSort::Magnitude),
        "distance" => Ok(ObjectSort::Distance),
        "name" => Ok(ObjectSort::Name),
        _ => Err(ApiError::bad_request(format!(
            "unsupported catalog sort `{value}`"
        ))),
    }
}

pub(super) fn default_catalog_query_limit() -> usize {
    DEFAULT_CATALOG_QUERY_LIMIT
}

pub(super) fn default_catalog_search_limit() -> usize {
    DEFAULT_CATALOG_SEARCH_LIMIT
}

pub(super) fn default_catalog_sort() -> String {
    "prominence".into()
}
