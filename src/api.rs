use crate::{
    config::{AuthMode, Config},
    models::{
        JobId, JobLease, JobRecord, JobResponse, JobStatus, SolutionResponse, SolveOptions,
        WorkerCompletion,
    },
    overlay::{OverlayOptions, render_svg},
    rate_limit::RateLimiter,
    repository::{JobRepository, job_repository},
    solver::{SolverEngine, dimensions_from_bytes, preview_png},
    storage::{ObjectStore, object_store},
    transport::{QueueTransport, queue_transport},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Form, Multipart, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use chrono::{Duration as ChronoDuration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
    repository: Arc<dyn JobRepository>,
    transport: Arc<dyn QueueTransport>,
    limiter: RateLimiter,
    store: Arc<dyn ObjectStore>,
    solver: SolverEngine,
}

impl AppState {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let store = object_store(&config).await?;
        let repository = job_repository(&config).await?;
        let transport = queue_transport(&config).await?;
        let solver = SolverEngine::from_catalog_paths(
            config.catalog_path.as_deref(),
            config.object_catalog_path.as_deref(),
        );
        Ok(Self {
            limiter: RateLimiter::new(config.rate_limit_per_minute, config.rate_limit_burst),
            config: Arc::new(config),
            repository,
            transport,
            store,
            solver,
        })
    }

    pub fn start_background_tasks(&self) {
        let state = self.clone();
        tokio::spawn(async move { state.cleanup_expired_uploads().await });
        if self.transport.uses_external_queue() {
            let state = self.clone();
            tokio::spawn(async move { state.dispatch_outbox().await });
        }
        if !self.config.embedded_workers {
            tracing::info!("embedded solver workers disabled; use `seiza-server worker`");
            return;
        }
        for worker in 0..self.config.worker_count {
            let state = self.clone();
            tokio::spawn(async move {
                tracing::info!(worker, "solver worker started");
                loop {
                    match state
                        .repository
                        .claim(None, state.config.lease_seconds)
                        .await
                    {
                        Ok(Some(lease)) => state.run_embedded_job(lease).await,
                        Ok(None) => tokio::time::sleep(Duration::from_secs(1)).await,
                        Err(error) => {
                            tracing::error!(%error, "failed to claim durable queue job");
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                    }
                }
            });
        }
    }

    async fn cleanup_expired_uploads(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(
            self.config.upload_cleanup_interval_seconds,
        ));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let retention = Duration::from_secs(self.config.upload_retention_seconds);
            let cutoff = SystemTime::now()
                .checked_sub(retention)
                .unwrap_or(SystemTime::UNIX_EPOCH);
            match self.store.delete_older_than(cutoff).await {
                Ok(0) => {}
                Ok(removed) => tracing::info!(removed, "deleted expired uploaded images"),
                Err(error) => tracing::error!(%error, "failed to clean expired uploaded images"),
            }
        }
    }

    async fn run_embedded_job(&self, lease: JobLease) {
        let Some(object_key) = self.repository.input_key(lease.job_id, lease.lease_token.clone()).await.unwrap_or_else(|error| {
            tracing::error!(job_id = lease.job_id, %error, "failed to resolve durable job input");
            None
        }) else { return };
        tracing::info!(job_id = lease.job_id, filename = %lease.original_filename, "starting durable queued solve");
        let outcome = async {
            let bytes = self.store.get(&object_key).await?;
            self.solver
                .solve(
                    bytes,
                    lease.original_filename.clone(),
                    lease.options.clone(),
                )
                .await
        }
        .await;
        let completion = match outcome {
            Ok(solution) => {
                tracing::info!(
                    job_id = lease.job_id,
                    matched_stars = solution.matched_stars,
                    rms_arcsec = solution.rms_arcsec,
                    "plate solve succeeded"
                );
                WorkerCompletion {
                    lease_token: lease.lease_token.clone(),
                    solution: Some(solution),
                    error: None,
                }
            }
            Err(error) => {
                tracing::warn!(job_id = lease.job_id, error = %error, "plate solve failed");
                WorkerCompletion {
                    lease_token: lease.lease_token.clone(),
                    solution: None,
                    error: Some(format!("{error:#}")),
                }
            }
        };
        match self
            .repository
            .complete(
                lease.job_id,
                completion.lease_token,
                completion.solution,
                completion.error,
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                job_id = lease.job_id,
                "embedded worker lost its lease before completion"
            ),
            Err(error) => {
                tracing::error!(job_id = lease.job_id, %error, "failed to persist worker completion")
            }
        };
    }

    async fn dispatch_outbox(&self) {
        tracing::info!("external queue dispatcher started");
        loop {
            match self.repository.pending_notifications(100).await {
                Ok(job_ids) => {
                    for job_id in job_ids {
                        match self.transport.publish(job_id).await {
                            Ok(()) => {
                                if let Err(error) =
                                    self.repository.mark_notification_delivered(job_id).await
                                {
                                    tracing::error!(%error, job_id, "failed to acknowledge durable queue notification");
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, job_id, "external queue publish failed; keeping outbox record")
                            }
                        }
                    }
                }
                Err(error) => tracing::error!(%error, "failed to read durable queue outbox"),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    async fn job(&self, job_id: JobId) -> Result<Option<JobRecord>, ApiError> {
        self.repository
            .get(job_id)
            .await
            .map_err(ApiError::internal)
    }

    fn input_expires_at(&self, job: &JobRecord) -> chrono::DateTime<Utc> {
        job.created_at + ChronoDuration::seconds(self.config.upload_retention_seconds as i64)
    }

    fn input_available(&self, job: &JobRecord) -> bool {
        Utc::now() < self.input_expires_at(job)
    }

    fn job_response(&self, job: &JobRecord) -> JobResponse {
        let input_available = self.input_available(job);
        JobResponse {
            id: job.id,
            status: job.status,
            created_at: job.created_at,
            started_at: job.started_at,
            completed_at: job.completed_at,
            original_filename: job.original_filename.clone(),
            input_expires_at: self.input_expires_at(job),
            input_available,
            preview_url: input_available.then(|| format!("/api/v1/solves/{}/preview", job.id)),
            overlay_url: (input_available && job.solution.is_some())
                .then(|| format!("/api/v1/solves/{}/overlay.svg", job.id)),
            wcs_url: job
                .solution
                .as_ref()
                .map(|_| format!("/api/v1/solves/{}/wcs", job.id)),
            solution: job.solution.clone(),
            error: job.error.clone(),
        }
    }

    async fn submit(
        &self,
        client: Client,
        upload: UploadedFile,
        options: SolveOptions,
    ) -> Result<JobResponse, ApiError> {
        options.validate().map_err(ApiError::bad_request)?;
        self.limiter
            .check(&client.id)
            .await
            .map_err(ApiError::rate_limited)?;
        let extension = safe_extension(&upload.filename);
        let prefix = self.config.s3_prefix.trim_matches('/');
        let object_key = if prefix.is_empty() {
            format!("{}.{}", Uuid::now_v7(), extension)
        } else {
            format!("{prefix}/{}.{}", Uuid::now_v7(), extension)
        };
        self.store
            .put(&object_key, upload.data, upload.content_type.as_deref())
            .await
            .map_err(ApiError::internal)?;
        let created_at = Utc::now();
        let job = JobRecord {
            id: 0,
            owner: client.id.clone(),
            queue_weight: client.queue_weight,
            object_key,
            original_filename: upload.filename,
            content_type: upload.content_type,
            options,
            status: JobStatus::Queued,
            created_at,
            started_at: None,
            completed_at: None,
            solution: None,
            error: None,
        };
        let job = self
            .repository
            .enqueue(job)
            .await
            .map_err(ApiError::internal)?;
        if self.transport.uses_external_queue() {
            match self.transport.publish(job.id).await {
                Ok(()) => self
                    .repository
                    .mark_notification_delivered(job.id)
                    .await
                    .map_err(ApiError::internal)?,
                Err(error) => {
                    tracing::warn!(job_id = job.id, %error, "external queue publish deferred to durable outbox")
                }
            }
        }
        Ok(self.job_response(&job))
    }
}

pub fn router(state: AppState) -> Router {
    let frontend_dir = state.config.frontend_dir.clone();
    Router::new()
        .route("/api/v1/health", get(get_health))
        .route("/api/v1/solves", post(post_solve))
        .route("/api/v1/solves/{job_id}", get(get_solve))
        .route("/api/v1/solves/{job_id}/preview", get(get_solve_preview))
        .route(
            "/api/v1/solves/{job_id}/overlay.svg",
            get(get_solve_overlay),
        )
        .route("/api/v1/solves/{job_id}/wcs", get(get_solve_wcs))
        .route("/api/v1/internal/worker/claim", post(worker_claim_next))
        .route(
            "/api/v1/internal/worker/claim/{job_id}",
            post(worker_claim_job),
        )
        .route(
            "/api/v1/internal/worker/jobs/{job_id}/input",
            get(worker_input),
        )
        .route(
            "/api/v1/internal/worker/jobs/{job_id}/heartbeat",
            post(worker_heartbeat),
        )
        .route(
            "/api/v1/internal/worker/jobs/{job_id}/complete",
            post(worker_complete),
        )
        // Astrometry.net-compatible subset: login, multipart upload,
        // submission polling, job status, calibration, and job info.
        .route("/api/login", post(astrometry_login))
        .route("/api/upload", post(astrometry_upload))
        .route("/api/submissions/{job_id}", get(astrometry_submission))
        .route("/api/jobs/{job_id}", get(astrometry_job))
        .route(
            "/api/jobs/{job_id}/calibration",
            get(astrometry_calibration),
        )
        .route(
            "/api/jobs/{job_id}/calibration/",
            get(astrometry_calibration),
        )
        .route("/api/jobs/{job_id}/info", get(astrometry_info))
        .route("/api/jobs/{job_id}/info/", get(astrometry_info))
        .fallback_service(
            ServeDir::new(&frontend_dir)
                .not_found_service(ServeFile::new(frontend_dir.join("index.html"))),
        )
        .with_state(state.clone())
        .layer(DefaultBodyLimit::max(state.config.max_upload_bytes))
        .layer(RequestBodyLimitLayer::new(state.config.max_upload_bytes))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([http::Method::GET, http::Method::POST])
                .allow_headers([
                    header::CONTENT_TYPE,
                    header::AUTHORIZATION,
                    http::HeaderName::from_static("x-api-key"),
                ]),
        )
        .layer(TraceLayer::new_for_http())
}

async fn get_health(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let status = if state.solver.is_ready() {
        "ready"
    } else {
        "degraded"
    };
    Ok(Json(json!({
        "status": status,
        "solver_ready": state.solver.is_ready(),
        "queue_depth": state.repository.queue_depth().await.map_err(ApiError::internal)?,
        "auth_mode": match state.config.auth_mode { AuthMode::Public => "public", AuthMode::StubApiKey => "stub-api-key" },
        "job_backend": match state.config.job_backend { crate::config::JobBackend::Sqlx => "sqlx", crate::config::JobBackend::DynamoDb => "dynamodb" },
        "queue_transport": match state.config.queue_transport { crate::config::QueueDelivery::Local => "local", crate::config::QueueDelivery::Sqs => "sqs" },
        "embedded_workers": state.config.embedded_workers,
    })))
}

async fn post_solve(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<(StatusCode, Json<JobResponse>), ApiError> {
    let client = client_from_headers(&state, &headers, None)?;
    let (upload, options_json, _) =
        read_multipart(multipart, state.config.max_upload_bytes).await?;
    let options = options_json
        .map(|raw| {
            serde_json::from_str(&raw)
                .map_err(|error| ApiError::bad_request(format!("invalid options JSON: {error}")))
        })
        .transpose()?
        .unwrap_or_default();
    let job = state.submit(client, upload, options).await?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn get_solve(
    State(state): State<AppState>,
    Path(job_id): Path<JobId>,
) -> Result<Json<JobResponse>, ApiError> {
    state
        .job(job_id)
        .await?
        .map(|job| Json(state.job_response(&job)))
        .ok_or_else(ApiError::not_found)
}

async fn get_solve_preview(
    State(state): State<AppState>,
    Path(job_id): Path<JobId>,
) -> Result<Response, ApiError> {
    let job = state.job(job_id).await?.ok_or_else(ApiError::not_found)?;
    ensure_input_available(&state, &job)?;
    let content = state
        .store
        .get(&job.object_key)
        .await
        .map_err(ApiError::internal)?;
    let preview = preview_png(content, job.original_filename)
        .await
        .map_err(ApiError::bad_request)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "private, max-age=300"),
        ],
        preview,
    )
        .into_response())
}

async fn get_solve_overlay(
    State(state): State<AppState>,
    Path(job_id): Path<JobId>,
    Query(query): Query<OverlayQuery>,
) -> Result<Response, ApiError> {
    let job = state.job(job_id).await?.ok_or_else(ApiError::not_found)?;
    ensure_input_available(&state, &job)?;
    let solution = job
        .solution
        .as_ref()
        .ok_or_else(|| ApiError::artifact_not_ready("the solve has not produced an overlay yet"))?;
    let content = state
        .store
        .get(&job.object_key)
        .await
        .map_err(ApiError::internal)?;
    let preview = preview_png(content, job.original_filename)
        .await
        .map_err(ApiError::bad_request)?;
    let svg = render_svg(
        solution,
        &preview,
        OverlayOptions {
            objects: query.objects,
            grid: query.grid,
        },
    );
    Ok((
        [
            (header::CONTENT_TYPE, "image/svg+xml; charset=utf-8"),
            (header::CACHE_CONTROL, "private, max-age=300"),
            (
                header::CONTENT_DISPOSITION,
                "inline; filename=seiza-overlay.svg",
            ),
        ],
        svg,
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
struct OverlayQuery {
    #[serde(default = "default_true")]
    objects: bool,
    #[serde(default)]
    grid: bool,
}

fn default_true() -> bool {
    true
}

async fn get_solve_wcs(
    State(state): State<AppState>,
    Path(job_id): Path<JobId>,
) -> Result<Response, ApiError> {
    let job = state.job(job_id).await?.ok_or_else(ApiError::not_found)?;
    let solution = job
        .solution
        .as_ref()
        .ok_or_else(|| ApiError::artifact_not_ready("the solve has not produced WCS data yet"))?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=seiza-solution.wcs",
            ),
        ],
        solution.fits_wcs_header(),
    )
        .into_response())
}

fn ensure_input_available(state: &AppState, job: &JobRecord) -> Result<(), ApiError> {
    if state.input_available(job) {
        Ok(())
    } else {
        Err(ApiError::gone(
            "the uploaded image and generated preview expired; WCS metadata remains available",
        ))
    }
}

async fn worker_claim_next(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authenticate_worker(&state, &headers)?;
    claim_response(&state, None).await
}

async fn worker_claim_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<JobId>,
) -> Result<Response, ApiError> {
    authenticate_worker(&state, &headers)?;
    claim_response(&state, Some(job_id)).await
}

async fn claim_response(
    state: &AppState,
    requested_job_id: Option<JobId>,
) -> Result<Response, ApiError> {
    match state
        .repository
        .claim(requested_job_id, state.config.lease_seconds)
        .await
        .map_err(ApiError::internal)?
    {
        Some(lease) => Ok(Json(lease).into_response()),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

async fn worker_input(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<JobId>,
) -> Result<Response, ApiError> {
    authenticate_worker(&state, &headers)?;
    let lease_token = headers
        .get("x-seiza-lease-token")
        .and_then(|value| value.to_str().ok())
        .filter(|token| !token.is_empty())
        .ok_or_else(|| ApiError::unauthorized("X-Seiza-Lease-Token is required"))?
        .to_owned();
    let object_key = state
        .repository
        .input_key(job_id, lease_token)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::conflict("job lease is no longer active"))?;
    let content = state
        .store
        .get(&object_key)
        .await
        .map_err(ApiError::internal)?;
    Ok((
        [(header::CONTENT_TYPE, "application/octet-stream")],
        content,
    )
        .into_response())
}

#[derive(Deserialize)]
struct LeaseTokenRequest {
    lease_token: String,
}

async fn worker_heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<JobId>,
    Json(request): Json<LeaseTokenRequest>,
) -> Result<Json<Value>, ApiError> {
    authenticate_worker(&state, &headers)?;
    let active = state
        .repository
        .heartbeat(job_id, request.lease_token, state.config.lease_seconds)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "active": active })))
}

async fn worker_complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<JobId>,
    Json(completion): Json<WorkerCompletion>,
) -> Result<Json<Value>, ApiError> {
    authenticate_worker(&state, &headers)?;
    let accepted = state
        .repository
        .complete(
            job_id,
            completion.lease_token,
            completion.solution,
            completion.error,
        )
        .await
        .map_err(ApiError::bad_request)?;
    Ok(Json(json!({ "accepted": accepted })))
}

fn authenticate_worker(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = state
        .config
        .worker_token
        .as_deref()
        .ok_or_else(|| ApiError::not_found_message("worker API is disabled"))?;
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if supplied != Some(expected) {
        return Err(ApiError::unauthorized("invalid worker token"));
    }
    Ok(())
}

#[derive(Deserialize)]
struct RequestJsonForm {
    #[serde(rename = "request-json")]
    request_json: String,
}

#[derive(Default, Deserialize)]
struct AstroLoginRequest {
    apikey: Option<String>,
}

async fn astrometry_login(
    State(state): State<AppState>,
    Form(form): Form<RequestJsonForm>,
) -> Result<Json<Value>, ApiError> {
    let request: AstroLoginRequest = serde_json::from_str(&form.request_json)
        .map_err(|error| ApiError::bad_request(format!("invalid request-json: {error}")))?;
    if state.config.auth_mode == AuthMode::StubApiKey
        && request.apikey.as_deref().is_none_or(str::is_empty)
    {
        return Err(ApiError::unauthorized(
            "an API key is required while SEIZA_AUTH_MODE=stub-api-key",
        ));
    }
    // Sessions intentionally are opaque-but-unvalidated until a real API-key
    // store is introduced. Keeping this response shape lets existing clients
    // integrate now without locking in that future auth implementation.
    Ok(Json(json!({
        "status": "success",
        "message": "authenticated by Seiza server (authentication stub)",
        "session": format!("seiza-{}", Uuid::now_v7()),
    })))
}

async fn astrometry_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (upload, _, request_json) =
        read_multipart(multipart, state.config.max_upload_bytes).await?;
    let request_json =
        request_json.ok_or_else(|| ApiError::bad_request("missing request-json field"))?;
    let request: AstroUploadRequest = serde_json::from_str(&request_json)
        .map_err(|error| ApiError::bad_request(format!("invalid request-json: {error}")))?;
    let client = client_from_headers(&state, &headers, request.session.as_deref())?;
    let dimensions =
        dimensions_from_bytes(&upload.data, &upload.filename).map_err(ApiError::bad_request)?;
    let options = request.into_options(dimensions)?;
    let job = state.submit(client, upload, options).await?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "status": "success",
            "subid": job.id,
            "hash": format!("seiza-job-{}", job.id),
        })),
    ))
}

async fn astrometry_submission(
    State(state): State<AppState>,
    Path(job_id): Path<JobId>,
) -> Result<Json<Value>, ApiError> {
    let job = state.job(job_id).await?.ok_or_else(ApiError::not_found)?;
    Ok(Json(json!({
        "processing_started": job.started_at,
        "processing_finished": job.completed_at,
        "jobs": [job.id],
        "job_calibrations": if job.status == JobStatus::Succeeded { vec![json!([job.id, job.id])] } else { Vec::new() },
        "user_images": [job.id],
    })))
}

async fn astrometry_job(
    State(state): State<AppState>,
    Path(job_id): Path<JobId>,
) -> Result<Json<Value>, ApiError> {
    let job = state.job(job_id).await?.ok_or_else(ApiError::not_found)?;
    Ok(Json(json!({ "status": astro_status(job.status) })))
}

async fn astrometry_calibration(
    State(state): State<AppState>,
    Path(job_id): Path<JobId>,
) -> Result<Json<Value>, ApiError> {
    let job = state.job(job_id).await?.ok_or_else(ApiError::not_found)?;
    match job.solution {
        Some(solution) => Ok(Json(calibration_json(&solution))),
        None => Ok(Json(json!({ "status": astro_status(job.status) }))),
    }
}

async fn astrometry_info(
    State(state): State<AppState>,
    Path(job_id): Path<JobId>,
) -> Result<Json<Value>, ApiError> {
    let job = state.job(job_id).await?.ok_or_else(ApiError::not_found)?;
    let mut result = json!({
        "status": astro_status(job.status),
        "original_filename": job.original_filename,
        "machine_tags": [],
        "tags": [],
        "objects_in_field": [],
    });
    if let Some(solution) = job.solution {
        result["calibration"] = calibration_json(&solution);
    }
    Ok(Json(result))
}

fn astro_status(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued | JobStatus::Solving => "solving",
        JobStatus::Succeeded => "success",
        JobStatus::Failed => "failure",
    }
}

fn calibration_json(solution: &SolutionResponse) -> Value {
    let cd = solution.wcs.cd;
    let determinant = cd[0][0] * cd[1][1] - cd[0][1] * cd[1][0];
    let orientation = (-cd[0][1]).atan2(cd[1][1]).to_degrees();
    let radius = ((solution.image_width as f64).hypot(solution.image_height as f64) / 2.0)
        * solution.pixel_scale_arcsec_per_pixel
        / 3600.0;
    json!({
        "status": "success",
        "parity": if determinant < 0.0 { 1.0 } else { 0.0 },
        "orientation": orientation,
        "pixscale": solution.pixel_scale_arcsec_per_pixel,
        "radius": radius,
        "ra": solution.center_ra_deg,
        "dec": solution.center_dec_deg,
    })
}

struct UploadedFile {
    filename: String,
    content_type: Option<String>,
    data: Bytes,
}

async fn read_multipart(
    mut multipart: Multipart,
    max_upload_bytes: usize,
) -> Result<(UploadedFile, Option<String>, Option<String>), ApiError> {
    let mut file = None;
    let mut options = None;
    let mut request_json = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(ApiError::bad_request)?
    {
        let name = field.name().unwrap_or_default().to_owned();
        match name.as_str() {
            "file" => {
                if file.is_some() {
                    return Err(ApiError::bad_request("submit exactly one file"));
                }
                let filename = field.file_name().unwrap_or("upload").to_owned();
                let content_type = field.content_type().map(str::to_owned);
                let data = field.bytes().await.map_err(ApiError::bad_request)?;
                if data.len() > max_upload_bytes {
                    return Err(ApiError::payload_too_large());
                }
                file = Some(UploadedFile {
                    filename: safe_filename(&filename),
                    content_type,
                    data,
                });
            }
            "options" => options = Some(field.text().await.map_err(ApiError::bad_request)?),
            "request-json" => {
                request_json = Some(field.text().await.map_err(ApiError::bad_request)?)
            }
            _ => {}
        }
    }
    file.map(|file| (file, options, request_json))
        .ok_or_else(|| ApiError::bad_request("missing file field"))
}

#[derive(Clone)]
struct Client {
    id: String,
    queue_weight: f64,
}

fn client_from_headers(
    state: &AppState,
    headers: &HeaderMap,
    astrometry_session: Option<&str>,
) -> Result<Client, ApiError> {
    let api_key = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        })
        .or(astrometry_session)
        .filter(|value| !value.trim().is_empty());
    let id = match (state.config.auth_mode, api_key) {
        (AuthMode::StubApiKey, None) => {
            return Err(ApiError::unauthorized(
                "provide X-API-Key, Bearer token, or Astrometry session",
            ));
        }
        (_, Some(key)) => format!("key:{:016x}", stable_hash(key)),
        (AuthMode::Public, None) => {
            let source_ip = headers
                .get("x-forwarded-for")
                .or_else(|| headers.get("x-real-ip"))
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(',').next())
                .unwrap_or("anonymous");
            format!("public:{source_ip}")
        }
    };
    Ok(Client {
        id,
        queue_weight: 1.0,
    })
}

fn stable_hash(value: &str) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn safe_filename(filename: &str) -> String {
    let name = filename.rsplit(['/', '\\']).next().unwrap_or("upload");
    let filename = name
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
        .take(160)
        .collect::<String>()
        .trim_matches('.')
        .to_owned();
    if filename.is_empty() {
        "upload".to_owned()
    } else {
        filename
    }
}

fn safe_extension(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "fit" | "fits" | "fts" => "fits",
        "jpg" | "jpeg" => "jpg",
        "png" => "png",
        "tif" | "tiff" => "tiff",
        "webp" => "webp",
        _ => "bin",
    }
}

#[derive(Default, Deserialize)]
struct AstroUploadRequest {
    session: Option<String>,
    center_ra: Option<f64>,
    center_dec: Option<f64>,
    radius: Option<f64>,
    scale_units: Option<String>,
    scale_type: Option<String>,
    scale_lower: Option<f64>,
    scale_upper: Option<f64>,
    scale_est: Option<f64>,
    scale_err: Option<f64>,
    downsample_factor: Option<f64>,
}

impl AstroUploadRequest {
    fn into_options(self, dimensions: (u32, u32)) -> Result<SolveOptions, ApiError> {
        let mut options = SolveOptions::default();
        if let Some(downsample) = self.downsample_factor
            && downsample > 1.0
        {
            return Err(ApiError::bad_request(
                "downsample_factor is not supported yet; resize before upload",
            ));
        }
        let range = match self.scale_type.as_deref().unwrap_or("ul") {
            "ul" => self.scale_lower.zip(self.scale_upper),
            "ev" => self.scale_est.map(|estimate| {
                let error = self.scale_err.unwrap_or(20.0).clamp(0.0, 99.0) / 100.0;
                (estimate * (1.0 - error), estimate * (1.0 + error))
            }),
            other => {
                return Err(ApiError::bad_request(format!(
                    "unsupported scale_type `{other}`"
                )));
            }
        };
        if let Some((lower, upper)) = range {
            let convert = |value: f64| match self.scale_units.as_deref().unwrap_or("degwidth") {
                "arcsecperpix" => Ok(value),
                "degwidth" => Ok(value * 3600.0 / dimensions.0 as f64),
                "arcminwidth" => Ok(value * 60.0 / dimensions.0 as f64),
                other => Err(ApiError::bad_request(format!(
                    "unsupported scale_units `{other}`"
                ))),
            };
            let lower = convert(lower)?;
            let upper = convert(upper)?;
            if lower <= 0.0 || upper < lower {
                return Err(ApiError::bad_request("invalid Astrometry scale range"));
            }
            options.min_scale_arcsec_per_pixel = lower;
            options.max_scale_arcsec_per_pixel = upper;
            if let (Some(ra), Some(dec)) = (self.center_ra, self.center_dec)
                && self.radius.unwrap_or(180.0) < 180.0
            {
                options.center_ra_deg = Some(ra);
                options.center_dec_deg = Some(dec);
                options.radius_deg = self.radius;
                options.scale_arcsec_per_pixel = Some((lower + upper) / 2.0);
                options.scale_tolerance = ((upper - lower) / (upper + lower)).clamp(0.01, 1.0);
            }
        }
        options.validate().map_err(ApiError::bad_request)?;
        Ok(options)
    }
}

struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    retry_after: Option<u64>,
}

impl ApiError {
    fn bad_request(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: error.to_string(),
            retry_after: None,
        }
    }
    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: message.into(),
            retry_after: None,
        }
    }
    fn not_found() -> Self {
        Self::not_found_message("solve job not found")
    }
    fn not_found_message(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: message.into(),
            retry_after: None,
        }
    }
    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "lease_conflict",
            message: message.into(),
            retry_after: None,
        }
    }
    fn artifact_not_ready(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "artifact_not_ready",
            message: message.into(),
            retry_after: None,
        }
    }
    fn gone(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::GONE,
            code: "input_expired",
            message: message.into(),
            retry_after: None,
        }
    }
    fn payload_too_large() -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "payload_too_large",
            message: "image exceeds SEIZA_MAX_UPLOAD_BYTES".into(),
            retry_after: None,
        }
    }
    fn rate_limited(retry_after: u64) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "rate_limited",
            message: "submission rate limit exceeded".into(),
            retry_after: Some(retry_after),
        }
    }
    fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "internal server error");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: "internal server error".into(),
            retry_after: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(json!({ "error": { "code": self.code, "message": self.message } })),
        )
            .into_response();
        if let Some(retry_after) = self.retry_after
            && let Ok(value) = HeaderValue::from_str(&retry_after.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}
