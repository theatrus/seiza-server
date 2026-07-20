use crate::models::{
    OverlayContour, OverlayObject, OverlayOutline, SatelliteMetadataSource,
    SatelliteSearchSummaryResponse, SatelliteTrackResponse, SatelliteTrackSegment,
    SatelliteTrailRiskResponse, SolutionResponse, SolveOptions,
};
use seiza_satellites::{
    BrightTrailRiskLevel, BrightTrailRiskOptions, CelesTrakLoad, CelesTrakSource,
    ExposureProvenance, ObserverLocation, SatelliteCatalog, SatelliteTrackAnalysis, SingleExposure,
    TrackOptions, UtcTimestamp,
};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex as AsyncMutex;

const HISTORY_SELECTION_THRESHOLD_SECONDS: f64 = 2.0 * 60.0 * 60.0;
const MAX_EXPOSURE_SECONDS: f64 = 60.0 * 60.0;
const MAX_CACHED_PREDICTIONS: usize = 256;

#[derive(Clone)]
pub struct SatelliteEngine {
    source: Option<CelesTrakSource>,
    active: Arc<RwLock<Option<LoadedCatalog>>>,
    refresh_lock: Arc<AsyncMutex<()>>,
    prediction_lock: Arc<AsyncMutex<()>>,
    prediction_cache: Arc<StdMutex<PredictionCache>>,
}

#[derive(Clone)]
struct LoadedCatalog {
    catalog: Arc<SatelliteCatalog>,
    retrieved_at: Option<UtcTimestamp>,
}

pub enum SatellitePrediction {
    Unavailable(String),
    Complete(SatellitePredictionResult),
}

#[derive(Clone)]
pub struct SatellitePredictionResult {
    pub catalog_version: String,
    pub tracks: Vec<SatelliteTrackResponse>,
    pub summary: SatelliteSearchSummaryResponse,
}

#[derive(Default)]
struct PredictionCache {
    entries: HashMap<(String, String), SatellitePredictionResult>,
    order: VecDeque<(String, String)>,
}

impl PredictionCache {
    fn get(&mut self, key: &(String, String)) -> Option<SatellitePredictionResult> {
        let result = self.entries.get(key)?.clone();
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.clone());
        Some(result)
    }

    fn insert(&mut self, key: (String, String), result: SatellitePredictionResult) {
        self.entries.insert(key.clone(), result);
        self.order.retain(|existing| existing != &key);
        self.order.push_back(key);
        while self.order.len() > MAX_CACHED_PREDICTIONS {
            if let Some(expired) = self.order.pop_front() {
                self.entries.remove(&expired);
            }
        }
    }
}

impl SatellitePredictionResult {
    pub fn all_elements_stale(&self) -> bool {
        self.summary.elements_considered > 0
            && self.summary.stale_elements == self.summary.elements_considered
            && self.tracks.is_empty()
    }
}

impl SatelliteEngine {
    pub fn disabled() -> Self {
        Self {
            source: None,
            active: Arc::new(RwLock::new(None)),
            refresh_lock: Arc::new(AsyncMutex::new(())),
            prediction_lock: Arc::new(AsyncMutex::new(())),
            prediction_cache: Arc::new(StdMutex::new(PredictionCache::default())),
        }
    }

    pub fn celestrak(
        cache_dir: PathBuf,
        cache_size_limit_bytes: u64,
    ) -> seiza_satellites::Result<Self> {
        Ok(Self {
            source: Some(
                CelesTrakSource::new(cache_dir)?
                    .with_cache_size_limit_bytes(cache_size_limit_bytes),
            ),
            active: Arc::new(RwLock::new(None)),
            refresh_lock: Arc::new(AsyncMutex::new(())),
            prediction_lock: Arc::new(AsyncMutex::new(())),
            prediction_cache: Arc::new(StdMutex::new(PredictionCache::default())),
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.source.is_some()
            || self
                .active
                .read()
                .expect("satellite catalog lock poisoned")
                .is_some()
    }

    pub async fn refresh(&self) {
        if let Err(error) = self.load_active(true).await {
            tracing::warn!(%error, "could not refresh CelesTrak satellite elements");
        }
    }

    pub async fn predict(
        &self,
        public_id: &str,
        solution: &SolutionResponse,
        options: &SolveOptions,
    ) -> SatellitePrediction {
        if !self.is_enabled() {
            return SatellitePrediction::Unavailable(
                "Satellite track prediction is disabled on this server.".into(),
            );
        }
        let exposure = match single_exposure(options) {
            Ok(exposure) => exposure,
            Err(reason) => return SatellitePrediction::Unavailable(reason),
        };
        let loaded = match self.catalog_for(exposure.midpoint()).await {
            Ok(loaded) => loaded,
            Err(error) => {
                tracing::warn!(%error, "could not load satellite elements for annotation");
                return SatellitePrediction::Unavailable(
                    "Satellite orbital elements are temporarily unavailable.".into(),
                );
            }
        };
        let wcs = solution.wcs.to_seiza();
        let dimensions = (solution.image_width, solution.image_height);
        let fingerprint = loaded.catalog.fingerprint().content_sha256;
        let cache_key = (public_id.to_owned(), fingerprint.clone());
        if let Some(cached) = self
            .prediction_cache
            .lock()
            .expect("satellite prediction cache lock poisoned")
            .get(&cache_key)
        {
            return SatellitePrediction::Complete(cached);
        }
        let _prediction = self.prediction_lock.lock().await;
        if let Some(cached) = self
            .prediction_cache
            .lock()
            .expect("satellite prediction cache lock poisoned")
            .get(&cache_key)
        {
            return SatellitePrediction::Complete(cached);
        }
        let catalog = loaded.catalog.clone();
        let search = match tokio::task::spawn_blocking(move || {
            catalog.tracks_in_footprint(&wcs, dimensions, &exposure, &TrackOptions::default())
        })
        .await
        {
            Ok(Ok(search)) => search,
            Ok(Err(error)) => {
                tracing::warn!(%error, "satellite propagation failed for solved footprint");
                return SatellitePrediction::Unavailable(
                    "Satellite tracks could not be propagated for this exposure.".into(),
                );
            }
            Err(error) => {
                tracing::warn!(%error, "satellite propagation task failed");
                return SatellitePrediction::Unavailable(
                    "Satellite track prediction was interrupted.".into(),
                );
            }
        };
        let analysis = search.into_analysis(&BrightTrailRiskOptions::default(), None);
        let tracks = analysis
            .tracks
            .into_iter()
            .enumerate()
            .map(|(index, track)| track_response(index, track))
            .collect();
        let result = SatellitePredictionResult {
            catalog_version: format!("satellites:{fingerprint}"),
            tracks,
            summary: SatelliteSearchSummaryResponse {
                catalog_source: loaded.catalog.source().to_owned(),
                catalog_retrieved_at: loaded
                    .retrieved_at
                    .map(seiza_satellites::UtcTimestamp::to_rfc3339),
                elements_considered: analysis.elements_considered,
                propagation_failures: analysis.propagation_failures,
                stale_elements: analysis.stale_elements,
            },
        };
        self.prediction_cache
            .lock()
            .expect("satellite prediction cache lock poisoned")
            .insert(cache_key, result.clone());
        SatellitePrediction::Complete(result)
    }

    async fn catalog_for(&self, time: UtcTimestamp) -> seiza_satellites::Result<LoadedCatalog> {
        let active = self.load_active(false).await?;
        let Some(source) = self.source.clone() else {
            return Ok(active);
        };
        let should_search_history = active.retrieved_at.is_some_and(|retrieved_at| {
            retrieved_at.seconds_since(time).abs() > HISTORY_SELECTION_THRESHOLD_SECONDS
        });
        if !should_search_history {
            return Ok(active);
        }
        let historical = tokio::task::spawn_blocking(move || source.load_cached_for(time))
            .await
            .map_err(|error| seiza_satellites::Error::CacheLock(error.to_string()))??;
        Ok(historical.map(LoadedCatalog::from).unwrap_or(active))
    }

    async fn load_active(&self, force: bool) -> seiza_satellites::Result<LoadedCatalog> {
        if !force
            && let Some(loaded) = self.current_active()
            && loaded.retrieved_at.is_none_or(|retrieved_at| {
                now_unix_seconds() - retrieved_at.unix_seconds()
                    < seiza_satellites::CELESTRAK_MIN_REFRESH.as_secs_f64()
            })
        {
            return Ok(loaded);
        }
        let Some(source) = self.source.clone() else {
            return self.current_active().ok_or_else(|| {
                seiza_satellites::Error::EmptyElements("satellite engine is disabled".into())
            });
        };
        let _refresh = self.refresh_lock.lock().await;
        if !force
            && let Some(loaded) = self.current_active()
            && loaded.retrieved_at.is_none_or(|retrieved_at| {
                now_unix_seconds() - retrieved_at.unix_seconds()
                    < seiza_satellites::CELESTRAK_MIN_REFRESH.as_secs_f64()
            })
        {
            return Ok(loaded);
        }
        let loaded = LoadedCatalog::from(source.load_active().await?);
        *self
            .active
            .write()
            .expect("satellite catalog lock poisoned") = Some(loaded.clone());
        Ok(loaded)
    }

    fn current_active(&self) -> Option<LoadedCatalog> {
        self.active
            .read()
            .expect("satellite catalog lock poisoned")
            .clone()
    }
}

impl From<CelesTrakLoad> for LoadedCatalog {
    fn from(load: CelesTrakLoad) -> Self {
        Self {
            retrieved_at: load.catalog.retrieved_at(),
            catalog: Arc::new(load.catalog),
        }
    }
}

pub fn track_overlay_object(track: &SatelliteTrackResponse) -> OverlayObject {
    let representative = track
        .segments
        .iter()
        .max_by(|left, right| segment_length(left).total_cmp(&segment_length(right)))
        .map(|segment| {
            [
                (segment.start[0] + segment.end[0]) / 2.0,
                (segment.start[1] + segment.end[1]) / 2.0,
            ]
        })
        .unwrap_or([0.0, 0.0]);
    OverlayObject {
        stable_id: Some(track.stable_id.clone()),
        name: track.label.clone(),
        common_name: String::new(),
        kind: "satellite".into(),
        mag: None,
        x: representative[0],
        y: representative[1],
        semi_major_px: 0.0,
        semi_minor_px: 0.0,
        angle_deg: None,
        source: Some("satellite_prediction".into()),
        catalog_source: Some(track.source.clone()),
        aliases: track.cospar_id.iter().cloned().collect(),
        parent_ids: Vec::new(),
        alternate_ids: track
            .norad_id
            .map(|id| vec![format!("NORAD {id}")])
            .unwrap_or_default(),
        alternate_sources: Vec::new(),
        ra_deg: None,
        dec_deg: None,
        discovered: None,
        near_capture: None,
        distance_au: None,
        motion_arcsec_per_hour: track
            .maximum_apparent_rate_arcsec_per_second
            .map(|rate| rate * 3_600.0),
        direction_pa_deg: None,
        direction_angle_deg: None,
        outlines: vec![OverlayOutline {
            geometry_id: format!("{}:predicted-track", track.stable_id),
            source_record_id: track.stable_id.clone(),
            role: "predicted-track".into(),
            quality: "propagated".into(),
            level: Some(track.risk.level.clone()),
            contours: track
                .segments
                .iter()
                .map(|segment| OverlayContour {
                    closed: false,
                    points: vec![segment.start, segment.end],
                })
                .collect(),
        }],
    }
}

fn segment_length(segment: &SatelliteTrackSegment) -> f64 {
    (segment.end[0] - segment.start[0]).hypot(segment.end[1] - segment.start[1])
}

fn single_exposure(options: &SolveOptions) -> Result<SingleExposure, String> {
    let start = options.capture_time.ok_or_else(|| {
        "Satellite tracks require the shutter-open date and time for this image.".to_owned()
    })?;
    let duration = options.exposure_seconds.ok_or_else(|| {
        "Satellite tracks require the duration of one shutter-open exposure.".to_owned()
    })?;
    if duration > MAX_EXPOSURE_SECONDS {
        return Err(
            "Satellite tracks support a single shutter-open exposure of up to one hour.".into(),
        );
    }
    let observer = match (
        options.observer_latitude_deg,
        options.observer_longitude_deg,
        options.observer_itrf_m,
    ) {
        (Some(latitude), Some(longitude), None) => ObserverLocation::geodetic(
            latitude,
            longitude,
            options.observer_altitude_m.unwrap_or(0.0),
        ),
        (None, None, Some([x, y, z])) => ObserverLocation::itrf_meters(x, y, z),
        _ => {
            return Err(
                "Satellite tracks require the observer latitude and longitude (or FITS OBSGEO coordinates)."
                    .into(),
            );
        }
    }
    .map_err(|error| error.to_string())?;
    let timestamp = UtcTimestamp::from_unix_seconds(
        start.timestamp() as f64 + start.timestamp_subsec_nanos() as f64 / 1e9,
    )
    .map_err(|error| error.to_string())?;
    let provenance = match options.satellite_metadata_source {
        Some(SatelliteMetadataSource::FitsHeader)
            if options
                .satellite_metadata_keywords
                .iter()
                .any(|keyword| keyword == "DATE-BEG")
                && options
                    .satellite_metadata_keywords
                    .iter()
                    .any(|keyword| keyword == "DATE-END") =>
        {
            ExposureProvenance::FitsBounds
        }
        Some(SatelliteMetadataSource::FitsHeader)
            if options
                .satellite_metadata_keywords
                .iter()
                .any(|keyword| keyword == "DATE-AVG") =>
        {
            ExposureProvenance::FitsDateAvgAndExposure
        }
        Some(SatelliteMetadataSource::FitsHeader)
            if options
                .satellite_metadata_keywords
                .iter()
                .any(|keyword| keyword == "DATE-END") =>
        {
            ExposureProvenance::FitsEndAndExposure
        }
        Some(SatelliteMetadataSource::FitsHeader) => ExposureProvenance::FitsDateObsAndExposure,
        _ => ExposureProvenance::Explicit,
    };
    SingleExposure::from_start_and_duration(timestamp, duration, observer, provenance)
        .map_err(|error| error.to_string())
}

fn track_response(index: usize, track: SatelliteTrackAnalysis) -> SatelliteTrackResponse {
    let risk = track.bright_trail_risk;
    let maximum_apparent_rate_arcsec_per_second = track.maximum_apparent_rate_arcsec_per_second;
    let label = track.identity.display_label();
    let stable_id = if let Some(norad_id) = track.identity.norad_id {
        format!("satellite:norad:{norad_id}")
    } else if let Some(cospar_id) = &track.identity.cospar_id {
        format!("satellite:cospar:{cospar_id}")
    } else {
        format!("satellite:anonymous:{index}")
    };
    SatelliteTrackResponse {
        stable_id,
        label,
        name: track.identity.name,
        norad_id: track.identity.norad_id,
        cospar_id: track.identity.cospar_id,
        source: track.source,
        element_epoch_utc: track.element_epoch_utc.to_rfc3339(),
        element_age_seconds: track.element_age_seconds,
        sample_interval_seconds: track.sample_interval_seconds,
        maximum_apparent_rate_arcsec_per_second,
        segments: track
            .clipped_segments
            .into_iter()
            .map(|segment| SatelliteTrackSegment {
                start: [segment.start.x, segment.start.y],
                end: [segment.end.x, segment.end.y],
            })
            .collect(),
        risk: SatelliteTrailRiskResponse {
            level: match risk.level {
                BrightTrailRiskLevel::Low => "low",
                BrightTrailRiskLevel::Possible => "possible",
                BrightTrailRiskLevel::High => "high",
            }
            .into(),
            score: risk.score,
            maximum_sunlight_fraction: risk.maximum_sunlight_fraction,
            minimum_range_km: risk.minimum_range_km,
            maximum_elevation_deg: risk.maximum_elevation_deg,
            clipped_length_px: risk.clipped_length_px,
        },
    }
}

fn now_unix_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn manual_image_metadata_builds_a_single_exposure() {
        let options = SolveOptions {
            capture_time: Some(Utc.with_ymd_and_hms(2026, 7, 19, 4, 5, 6).unwrap()),
            exposure_seconds: Some(30.0),
            observer_latitude_deg: Some(37.3),
            observer_longitude_deg: Some(-122.0),
            observer_altitude_m: Some(50.0),
            satellite_metadata_source: Some(SatelliteMetadataSource::Explicit),
            ..SolveOptions::default()
        };
        let exposure = single_exposure(&options).unwrap();
        assert_eq!(exposure.duration_seconds(), 30.0);
        assert_eq!(exposure.provenance, ExposureProvenance::Explicit);
    }

    #[test]
    fn missing_duration_has_a_specific_unavailable_reason() {
        let options = SolveOptions {
            capture_time: Some(Utc.with_ymd_and_hms(2026, 7, 19, 4, 5, 6).unwrap()),
            observer_latitude_deg: Some(37.3),
            observer_longitude_deg: Some(-122.0),
            ..SolveOptions::default()
        };
        assert_eq!(
            single_exposure(&options).unwrap_err(),
            "Satellite tracks require the duration of one shutter-open exposure."
        );
    }

    #[test]
    fn long_or_stacked_exposure_is_not_propagated() {
        let options = SolveOptions {
            capture_time: Some(Utc.with_ymd_and_hms(2026, 7, 19, 4, 5, 6).unwrap()),
            exposure_seconds: Some(MAX_EXPOSURE_SECONDS + 1.0),
            observer_latitude_deg: Some(37.3),
            observer_longitude_deg: Some(-122.0),
            ..SolveOptions::default()
        };
        assert_eq!(
            single_exposure(&options).unwrap_err(),
            "Satellite tracks support a single shutter-open exposure of up to one hour."
        );
    }
}
