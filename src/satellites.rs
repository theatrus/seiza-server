use crate::{
    models::{
        OverlayContour, OverlayObject, OverlayOutline, SatelliteMetadataSource,
        SatellitePixelAlignmentResponse, SatelliteSearchSummaryResponse, SatelliteTrackResponse,
        SatelliteTrackSegment, SatelliteTrailRiskResponse, SolutionResponse, SolveOptions,
    },
    solver::decode_monochrome_u16,
};
use bytes::Bytes;
use seiza_satellites::{
    BrightTrailRiskLevel, BrightTrailRiskOptions, CacheState, ExposureProvenance, ObserverLocation,
    OrbitalCatalogLoad, OrbitalCatalogProvider, OrbitalCatalogSource, SatelliteCatalog,
    SatelliteTrackAnalysis, SingleExposure, TrackOptions, UtcTimestamp,
    trail_alignment::{
        PIXEL_TRAIL_ALIGNMENT_VERSION, PixelTrailAligner, PixelTrailAlignment,
        PixelTrailAlignmentConfig, PixelTrailAlignmentStatus, PixelTrailNotEvaluatedReason,
    },
};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::sync::Mutex as AsyncMutex;

const MAX_EXPOSURE_SECONDS: f64 = 60.0 * 60.0;
const MAX_CACHED_PREDICTIONS: usize = 256;
const SATELLITE_CATALOG_LOOKUP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct SatelliteEngine {
    source: Option<OrbitalCatalogSource>,
    resolution_lock: Arc<AsyncMutex<()>>,
    prediction_lock: Arc<AsyncMutex<()>>,
    prediction_cache: Arc<StdMutex<PredictionCache>>,
}

#[derive(Clone)]
struct LoadedCatalog {
    catalog: Arc<SatelliteCatalog>,
    retrieved_at: UtcTimestamp,
    query_time: Option<UtcTimestamp>,
    provider: OrbitalCatalogProvider,
    cache_state: CacheState,
    warning: Option<String>,
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

pub struct SatellitePixelSource {
    pub bytes: Bytes,
    pub filename: String,
}

#[derive(Default)]
struct PredictionCache {
    entries: HashMap<(String, String, bool), SatellitePredictionResult>,
    order: VecDeque<(String, String, bool)>,
}

impl PredictionCache {
    fn get(&mut self, key: &(String, String, bool)) -> Option<SatellitePredictionResult> {
        let result = self.entries.get(key)?.clone();
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.clone());
        Some(result)
    }

    fn insert(&mut self, key: (String, String, bool), result: SatellitePredictionResult) {
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
            resolution_lock: Arc::new(AsyncMutex::new(())),
            prediction_lock: Arc::new(AsyncMutex::new(())),
            prediction_cache: Arc::new(StdMutex::new(PredictionCache::default())),
        }
    }

    pub fn orbital(
        cache_dir: PathBuf,
        cache_size_limit_bytes: u64,
    ) -> seiza_satellites::Result<Self> {
        Ok(Self::with_source(
            OrbitalCatalogSource::new(cache_dir)?
                .with_cache_size_limit_bytes(cache_size_limit_bytes),
        ))
    }

    fn with_source(source: OrbitalCatalogSource) -> Self {
        Self {
            source: Some(source),
            resolution_lock: Arc::new(AsyncMutex::new(())),
            prediction_lock: Arc::new(AsyncMutex::new(())),
            prediction_cache: Arc::new(StdMutex::new(PredictionCache::default())),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.source.is_some()
    }

    pub async fn predict(
        &self,
        public_id: &str,
        solution: &SolutionResponse,
        options: &SolveOptions,
        pixel_source: Option<SatellitePixelSource>,
        pixel_alignment_error: Option<String>,
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
        let loaded = match tokio::time::timeout(
            SATELLITE_CATALOG_LOOKUP_TIMEOUT,
            self.catalog_for(&exposure),
        )
        .await
        {
            Ok(Ok(loaded)) => loaded,
            Ok(Err(error)) => {
                tracing::warn!(%error, "could not load satellite elements for annotation");
                return SatellitePrediction::Unavailable(
                    "Satellite orbital elements are temporarily unavailable.".into(),
                );
            }
            Err(_) => {
                tracing::warn!(
                    timeout_seconds = SATELLITE_CATALOG_LOOKUP_TIMEOUT.as_secs(),
                    "satellite element lookup timed out"
                );
                return SatellitePrediction::Unavailable(
                    "Satellite orbital element lookup timed out; try this overlay again shortly."
                        .into(),
                );
            }
        };
        if let Some(warning) = &loaded.warning {
            tracing::warn!(
                %warning,
                provider = ?loaded.provider,
                cache_state = ?loaded.cache_state,
                query_time = loaded.query_time.map(UtcTimestamp::to_rfc3339),
                "satellite catalog resolved with a provider warning"
            );
        }
        let wcs = solution.wcs.to_seiza();
        let dimensions = (solution.image_width, solution.image_height);
        let fingerprint = loaded.catalog.fingerprint().content_sha256;
        let cache_key = (
            public_id.to_owned(),
            fingerprint.clone(),
            pixel_source.is_some(),
        );
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
        let analyzed = match tokio::task::spawn_blocking(move || {
            let search = catalog.tracks_in_footprint(
                &wcs,
                dimensions,
                &exposure,
                &TrackOptions::default(),
            )?;
            let (pixel_aligner, pixel_alignment_error) = match pixel_source {
                Some(source) => match decode_monochrome_u16(&source.bytes, &source.filename) {
                    Ok(frame)
                        if (frame.width, frame.height)
                            == (dimensions.0 as usize, dimensions.1 as usize) =>
                    {
                        match PixelTrailAligner::from_u16(
                            frame.width,
                            frame.height,
                            &frame.pixels,
                            frame.adu_per_stored_unit,
                            PixelTrailAlignmentConfig::default(),
                        ) {
                            Ok(aligner) => (Some(aligner), pixel_alignment_error),
                            Err(error) => (
                                None,
                                Some(format!(
                                    "Pixel trail detection could not initialize: {error}"
                                )),
                            ),
                        }
                    }
                    Ok(frame) => (
                        None,
                        Some(format!(
                            "Pixel trail detection skipped an image-size mismatch: decoded {}×{}, solved {}×{}.",
                            frame.width, frame.height, dimensions.0, dimensions.1
                        )),
                    ),
                    Err(error) => (
                        None,
                        Some(format!("Pixel trail detection could not decode the image: {error}")),
                    ),
                },
                None => (None, pixel_alignment_error),
            };
            let pixel_alignment_attempted = pixel_aligner.is_some();
            Ok::<_, seiza_satellites::Error>((
                search.into_analysis(
                    &BrightTrailRiskOptions::default(),
                    pixel_aligner.as_ref(),
                ),
                pixel_alignment_attempted,
                pixel_alignment_error,
            ))
        })
        .await
        {
            Ok(Ok(analyzed)) => analyzed,
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
        let (analysis, pixel_alignment_attempted, pixel_alignment_error) = analyzed;
        let pixel_aligned = analysis
            .tracks
            .iter()
            .filter(|track| {
                track
                    .pixel_alignment
                    .as_ref()
                    .is_some_and(PixelTrailAlignment::detected)
            })
            .count();
        let tracks = analysis
            .tracks
            .into_iter()
            .enumerate()
            .map(|(index, track)| track_response(index, track))
            .collect();
        let result = SatellitePredictionResult {
            catalog_version: if pixel_alignment_attempted {
                format!("satellites:{fingerprint};pixel-trail:{PIXEL_TRAIL_ALIGNMENT_VERSION}")
            } else {
                format!("satellites:{fingerprint}")
            },
            tracks,
            summary: SatelliteSearchSummaryResponse {
                catalog_source: loaded.catalog.source().to_owned(),
                catalog_retrieved_at: Some(loaded.retrieved_at.to_rfc3339()),
                elements_considered: analysis.elements_considered,
                propagation_failures: analysis.propagation_failures,
                stale_elements: analysis.stale_elements,
                pixel_alignment_attempted,
                pixel_aligned,
                pixel_alignment_error,
            },
        };
        self.prediction_cache
            .lock()
            .expect("satellite prediction cache lock poisoned")
            .insert(cache_key, result.clone());
        SatellitePrediction::Complete(result)
    }

    async fn catalog_for(
        &self,
        exposure: &SingleExposure,
    ) -> seiza_satellites::Result<LoadedCatalog> {
        let Some(source) = self.source.clone() else {
            return Err(seiza_satellites::Error::EmptyElements(
                "satellite engine is disabled".into(),
            ));
        };
        let _resolution = self.resolution_lock.lock().await;
        source.load_for_exposure(exposure).await.map(Into::into)
    }
}

impl From<OrbitalCatalogLoad> for LoadedCatalog {
    fn from(load: OrbitalCatalogLoad) -> Self {
        Self {
            retrieved_at: load.snapshot.retrieved_at,
            query_time: load.snapshot.query_time,
            provider: load.snapshot.provider,
            cache_state: load.state,
            warning: load.warning,
            catalog: Arc::new(load.catalog),
        }
    }
}

pub fn track_overlay_object(track: &SatelliteTrackResponse) -> OverlayObject {
    let aligned_segments = track
        .pixel_alignment
        .as_ref()
        .filter(|alignment| alignment.status == "detected")
        .map(|alignment| alignment.segments.as_slice())
        .unwrap_or_default();
    let representative_segments = if aligned_segments.is_empty() {
        track.segments.as_slice()
    } else {
        aligned_segments
    };
    let representative = representative_segments
        .iter()
        .max_by(|left, right| segment_length(left).total_cmp(&segment_length(right)))
        .map(|segment| {
            [
                (segment.start[0] + segment.end[0]) / 2.0,
                (segment.start[1] + segment.end[1]) / 2.0,
            ]
        })
        .unwrap_or([0.0, 0.0]);
    let mut outlines = vec![OverlayOutline {
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
    }];
    if !aligned_segments.is_empty() {
        outlines.push(OverlayOutline {
            geometry_id: format!("{}:pixel-aligned-track", track.stable_id),
            source_record_id: track.stable_id.clone(),
            role: "pixel-aligned-track".into(),
            quality: "detected".into(),
            level: Some("detected".into()),
            contours: aligned_segments
                .iter()
                .map(|segment| OverlayContour {
                    closed: false,
                    points: vec![segment.start, segment.end],
                })
                .collect(),
        });
    }
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
        outlines,
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
    let pixel_alignment = track.pixel_alignment.map(pixel_alignment_response);
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
        pixel_alignment,
    }
}

fn pixel_alignment_response(alignment: PixelTrailAlignment) -> SatellitePixelAlignmentResponse {
    SatellitePixelAlignmentResponse {
        status: match alignment.status {
            PixelTrailAlignmentStatus::Detected => "detected",
            PixelTrailAlignmentStatus::NotDetected => "not_detected",
            PixelTrailAlignmentStatus::NotEvaluated => "not_evaluated",
        }
        .into(),
        not_evaluated_reason: alignment.not_evaluated_reason.map(|reason| {
            match reason {
                PixelTrailNotEvaluatedReason::EmptyPath => "empty_path",
                PixelTrailNotEvaluatedReason::TooShort => "too_short",
                PixelTrailNotEvaluatedReason::InsufficientCoverage => "insufficient_coverage",
            }
            .into()
        }),
        segments: alignment
            .aligned_segments
            .into_iter()
            .map(|segment| SatelliteTrackSegment {
                start: [segment.start.x, segment.start.y],
                end: [segment.end.x, segment.end.y],
            })
            .collect(),
        mean_normal_offset_px: alignment.mean_normal_offset_px,
        angle_delta_deg: alignment.angle_delta_deg,
        contrast_adu: alignment.contrast_adu,
        contrast_sigma: alignment.contrast_sigma,
        continuity: alignment.continuity,
        coverage: alignment.coverage,
        search_radius_px: alignment.search_radius_px,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    const HISTORICAL_TLE: &str = "ISS (ZARYA)\n\
1 25544U 98067A   24123.50000000  .00016717  00000-0  30126-3 0  9990\n\
2 25544  51.6400 160.0000 0005000  80.0000 280.0000 15.50000000450000\n";

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

    #[test]
    fn overlay_keeps_prediction_and_pixel_evidence_as_separate_geometry() {
        let track = SatelliteTrackResponse {
            stable_id: "satellite:norad:25544".into(),
            label: "ISS (ZARYA)".into(),
            name: "ISS (ZARYA)".into(),
            norad_id: Some(25_544),
            cospar_id: Some("1998-067A".into()),
            source: "celestrak:active".into(),
            element_epoch_utc: "2026-07-20T00:00:00Z".into(),
            element_age_seconds: 120.0,
            sample_interval_seconds: 1.0,
            maximum_apparent_rate_arcsec_per_second: Some(300.0),
            segments: vec![SatelliteTrackSegment {
                start: [10.0, 20.0],
                end: [90.0, 20.0],
            }],
            risk: SatelliteTrailRiskResponse {
                level: "high".into(),
                score: 0.9,
                maximum_sunlight_fraction: 1.0,
                minimum_range_km: 420.0,
                maximum_elevation_deg: 70.0,
                clipped_length_px: 80.0,
            },
            pixel_alignment: Some(SatellitePixelAlignmentResponse {
                status: "detected".into(),
                not_evaluated_reason: None,
                segments: vec![SatelliteTrackSegment {
                    start: [20.0, 24.0],
                    end: [80.0, 24.0],
                }],
                mean_normal_offset_px: 4.0,
                angle_delta_deg: 0.1,
                contrast_adu: 120.0,
                contrast_sigma: 8.0,
                continuity: 0.92,
                coverage: 0.75,
                search_radius_px: 12.0,
            }),
        };

        let overlay = track_overlay_object(&track);

        assert_eq!((overlay.x, overlay.y), (50.0, 24.0));
        assert_eq!(overlay.outlines.len(), 2);
        assert_eq!(overlay.outlines[0].role, "predicted-track");
        assert_eq!(overlay.outlines[0].quality, "propagated");
        assert_eq!(overlay.outlines[1].role, "pixel-aligned-track");
        assert_eq!(overlay.outlines[1].quality, "detected");
    }

    #[tokio::test]
    async fn historical_exposure_uses_epoch_cache_without_provider_network() {
        let cache = std::env::temp_dir().join(format!(
            "seiza-server-satellite-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&cache).unwrap();
        let midpoint = UtcTimestamp::parse("2024-05-02T12:00:00Z").unwrap();
        let query_millis = (midpoint.unix_seconds() * 1_000.0).round() as i64;
        std::fs::write(
            cache.join(format!(
                "satchecker-epoch-{query_millis}-cached-1714651200.tle"
            )),
            HISTORICAL_TLE,
        )
        .unwrap();
        let source = OrbitalCatalogSource::new(&cache)
            .unwrap()
            .with_mirror_base_url("http://127.0.0.1:1/never-called")
            .with_satchecker_endpoint("http://127.0.0.1:1/never-called");
        let engine = SatelliteEngine::with_source(source);
        let observer = ObserverLocation::geodetic(37.3, -122.0, 50.0).unwrap();
        let exposure = SingleExposure::from_start_and_duration(
            midpoint.add_seconds(-15.0).unwrap(),
            30.0,
            observer,
            ExposureProvenance::Explicit,
        )
        .unwrap();

        let loaded = engine.catalog_for(&exposure).await.unwrap();

        assert_eq!(loaded.provider, OrbitalCatalogProvider::IauSatChecker);
        assert_eq!(loaded.cache_state, CacheState::Cached);
        assert_eq!(loaded.query_time, Some(midpoint));
        assert_eq!(loaded.catalog.len(), 1);
        std::fs::remove_dir_all(cache).unwrap();
    }
}
