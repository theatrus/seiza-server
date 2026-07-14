use crate::{
    annotations::{AnnotationEngine, AnnotationOptions},
    config::{AuthMode, Config},
    models::{
        AnnotationResponse, JobId, JobLease, JobRecord, JobResponse, JobStatus, SolutionResponse,
        SolveOptions, WorkerCompletion,
    },
    overlay::{OverlayOptions, render_svg},
    rate_limit::RateLimiter,
    repository::{JobRepository, job_repository},
    solver::{SolverEngine, capture_time_from_bytes, dimensions_from_bytes, full_png, preview_png},
    storage::{ObjectStore, object_store},
    transport::{QueueTransport, queue_transport},
    uploads::{ResumableUpload, ResumableUploadError, TUS_EXTENSIONS, TUS_VERSION},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Form, Multipart, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use base64::Engine;
use bytes::Bytes;
use chrono::{Duration as ChronoDuration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{Arc, Weak},
    time::{Duration, SystemTime},
};
use tokio::sync::Mutex;
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
    annotations: AnnotationEngine,
    upload_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
}

impl AppState {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let store = object_store(&config).await?;
        let repository = job_repository(&config).await?;
        let transport = queue_transport(&config).await?;
        let solver = SolverEngine::from_catalog_paths(
            config.catalog_path.as_deref(),
            config.blind_index_path.as_deref(),
        );
        let annotations = AnnotationEngine::new(
            solver.catalog(),
            config.catalog_path.as_deref(),
            config.object_catalog_path.as_deref(),
            config.transient_catalog_path.as_deref(),
            config.minor_body_catalog_path.as_deref(),
        );
        Ok(Self {
            limiter: RateLimiter::new(config.rate_limit_per_minute, config.rate_limit_burst),
            config: Arc::new(config),
            repository,
            transport,
            store,
            solver,
            annotations,
            upload_locks: Arc::new(Mutex::new(HashMap::new())),
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

    async fn public_job(&self, public_id: &str) -> Result<Option<JobRecord>, ApiError> {
        let Some(job_id) = public_job_sequence(public_id) else {
            return Ok(None);
        };
        let Some(job) = self.job(job_id).await? else {
            return Ok(None);
        };
        Ok(public_id_matches_job(public_id, job.id, &job.object_key).then_some(job))
    }

    fn input_expires_at(&self, job: &JobRecord) -> chrono::DateTime<Utc> {
        job.created_at + ChronoDuration::seconds(self.config.upload_retention_seconds as i64)
    }

    fn input_available(&self, job: &JobRecord) -> bool {
        Utc::now() < self.input_expires_at(job)
    }

    fn job_response(&self, job: &JobRecord) -> Result<JobResponse, ApiError> {
        let public_id = public_job_id(job)
            .ok_or_else(|| ApiError::internal("job object key has no public UUID"))?;
        let input_available = self.input_available(job);
        let solution = job.solution.as_ref().map(|solution| {
            let mut solution = solution.clone();
            let annotations = self.annotations.annotate(
                &public_id,
                &solution,
                job.options.capture_time,
                &AnnotationOptions::default(),
            );
            if self.annotations.is_configured() {
                solution.objects = annotations.objects;
            }
            solution.catalog_version = Some(annotations.catalog_version);
            solution.capture_time = job.options.capture_time;
            solution
        });
        Ok(JobResponse {
            id: public_id.clone(),
            status: job.status,
            created_at: job.created_at,
            started_at: job.started_at,
            completed_at: job.completed_at,
            original_filename: job.original_filename.clone(),
            input_expires_at: self.input_expires_at(job),
            input_available,
            preview_url: input_available.then(|| format!("/api/v1/solves/{public_id}/preview")),
            overlay_url: (input_available && solution.is_some())
                .then(|| format!("/api/v1/solves/{public_id}/overlay.svg")),
            annotations_url: solution
                .as_ref()
                .map(|_| format!("/api/v1/solves/{public_id}/annotations")),
            wcs_url: job
                .solution
                .as_ref()
                .map(|_| format!("/api/v1/solves/{public_id}/wcs")),
            solution,
            error: job.error.clone(),
        })
    }

    async fn submit(
        &self,
        client: Client,
        upload: UploadedFile,
        mut options: SolveOptions,
    ) -> Result<JobResponse, ApiError> {
        if options.capture_time.is_none() {
            options.capture_time = capture_time_from_bytes(&upload.data, &upload.filename);
        }
        options.validate().map_err(ApiError::bad_request)?;
        self.limiter
            .check(&client.id)
            .await
            .map_err(ApiError::rate_limited)?;
        let object_key = self.new_object_key(&upload.filename);
        self.store
            .put(&object_key, upload.data, upload.content_type.as_deref())
            .await
            .map_err(ApiError::internal)?;
        let job = self
            .enqueue_stored(
                client,
                object_key,
                upload.filename,
                upload.content_type,
                options,
            )
            .await?;
        self.job_response(&job)
    }

    fn new_object_key(&self, filename: &str) -> String {
        let extension = safe_extension(filename);
        let prefix = self.config.s3_prefix.trim_matches('/');
        let public_token = Uuid::new_v4();
        let storage_token = Uuid::now_v7();
        if prefix.is_empty() {
            format!("public-{public_token}/{storage_token}.{extension}")
        } else {
            format!("{prefix}/public-{public_token}/{storage_token}.{extension}")
        }
    }

    async fn enqueue_stored(
        &self,
        client: Client,
        object_key: String,
        original_filename: String,
        content_type: Option<String>,
        options: SolveOptions,
    ) -> Result<JobRecord, ApiError> {
        if let Some(job) = self
            .repository
            .find_by_object_key(&object_key)
            .await
            .map_err(ApiError::internal)?
        {
            return Ok(job);
        }
        let created_at = Utc::now();
        let job = JobRecord {
            id: 0,
            owner: client.id.clone(),
            queue_weight: client.queue_weight,
            object_key,
            original_filename,
            content_type,
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
        Ok(job)
    }

    async fn upload_lock(&self, id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.upload_locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(id.to_owned(), Arc::downgrade(&lock));
        lock
    }

    async fn finalize_resumable(
        &self,
        upload: &mut ResumableUpload,
    ) -> Result<JobRecord, ApiError> {
        if let Some(job_id) = upload.job_id {
            return self
                .repository
                .get(job_id)
                .await
                .map_err(ApiError::internal)?
                .ok_or_else(|| ApiError::internal("completed upload job is missing"));
        }
        let bytes = upload
            .assemble(&self.store)
            .await
            .map_err(resumable_api_error)?;
        if upload.options.capture_time.is_none() {
            upload.options.capture_time =
                capture_time_from_bytes(&bytes, &upload.original_filename);
        }
        self.store
            .put(&upload.object_key, bytes, upload.content_type.as_deref())
            .await
            .map_err(ApiError::internal)?;
        let job = self
            .enqueue_stored(
                Client {
                    id: upload.owner.clone(),
                    queue_weight: upload.queue_weight,
                },
                upload.object_key.clone(),
                upload.original_filename.clone(),
                upload.content_type.clone(),
                upload.options.clone(),
            )
            .await?;
        upload.job_id = Some(job.id);
        upload
            .save(&self.store, &self.config.s3_prefix)
            .await
            .map_err(resumable_api_error)?;
        upload.cleanup_chunks(&self.store).await;
        if let Err(error) = upload.save(&self.store, &self.config.s3_prefix).await {
            tracing::warn!(upload_id = %upload.id, %error, "could not compact completed upload state");
        }
        Ok(job)
    }
}

pub fn router(state: AppState) -> Router {
    let frontend_dir = state.config.frontend_dir.clone();
    Router::new()
        .route("/api/v1/health", get(get_health))
        .route("/api/v1/solves", post(post_solve))
        .route(
            "/api/v1/uploads",
            post(create_resumable_upload).options(resumable_upload_options),
        )
        .route(
            "/api/v1/uploads/{upload_id}",
            patch(patch_resumable_upload)
                .head(head_resumable_upload)
                .delete(delete_resumable_upload),
        )
        .route(
            "/api/v1/uploads/{upload_id}/result",
            get(get_resumable_upload_result),
        )
        .route("/api/v1/solves/{job_id}", get(get_solve))
        .route(
            "/api/v1/solves/{job_id}/annotations",
            get(get_solve_annotations),
        )
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
                .allow_methods([
                    http::Method::GET,
                    http::Method::POST,
                    http::Method::PATCH,
                    http::Method::DELETE,
                    http::Method::HEAD,
                    http::Method::OPTIONS,
                ])
                .allow_headers([
                    header::CONTENT_TYPE,
                    header::AUTHORIZATION,
                    http::HeaderName::from_static("x-api-key"),
                    http::HeaderName::from_static("tus-resumable"),
                    http::HeaderName::from_static("upload-length"),
                    http::HeaderName::from_static("upload-offset"),
                    http::HeaderName::from_static("upload-metadata"),
                ])
                .expose_headers([
                    header::LOCATION,
                    http::HeaderName::from_static("tus-resumable"),
                    http::HeaderName::from_static("upload-length"),
                    http::HeaderName::from_static("upload-offset"),
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

async fn resumable_upload_options(State(state): State<AppState>) -> Response {
    (
        StatusCode::NO_CONTENT,
        tus_headers(state.config.max_upload_bytes),
    )
        .into_response()
}

async fn create_resumable_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    verify_tus_version(&headers)?;
    let client = client_from_headers(&state, &headers, None)?;
    state
        .limiter
        .check(&client.id)
        .await
        .map_err(ApiError::rate_limited)?;
    let total_size = required_u64_header(&headers, "upload-length")?;
    if total_size == 0 {
        return Err(ApiError::bad_request("uploaded image must not be empty"));
    }
    if total_size > state.config.max_upload_bytes as u64 {
        return Err(ApiError::payload_too_large());
    }
    let metadata = parse_upload_metadata(&headers)?;
    let original_filename = metadata
        .get("filename")
        .map(|filename| safe_filename(filename))
        .filter(|filename| !filename.is_empty())
        .ok_or_else(|| ApiError::bad_request("Upload-Metadata must include filename"))?;
    let content_type = metadata
        .get("filetype")
        .filter(|value| !value.is_empty() && value.len() <= 255)
        .cloned();
    let options = metadata
        .get("options")
        .map(|raw| {
            serde_json::from_str::<SolveOptions>(raw)
                .map_err(|error| ApiError::bad_request(format!("invalid options JSON: {error}")))
        })
        .transpose()?
        .unwrap_or_default();
    options.validate().map_err(ApiError::bad_request)?;
    let object_key = state.new_object_key(&original_filename);
    let upload = ResumableUpload::new(
        original_filename,
        content_type,
        total_size,
        object_key,
        options,
        client.id,
        client.queue_weight,
    );
    upload
        .save(&state.store, &state.config.s3_prefix)
        .await
        .map_err(resumable_api_error)?;

    let mut response_headers = tus_headers(state.config.max_upload_bytes);
    response_headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/api/v1/uploads/{}", upload.id))
            .map_err(ApiError::internal)?,
    );
    response_headers.insert(
        http::HeaderName::from_static("upload-offset"),
        HeaderValue::from_static("0"),
    );
    Ok((StatusCode::CREATED, response_headers).into_response())
}

async fn head_resumable_upload(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    verify_tus_version(&headers)?;
    let client = client_from_headers(&state, &headers, None)?;
    let upload = ResumableUpload::load(&state.store, &state.config.s3_prefix, &upload_id)
        .await
        .map_err(resumable_api_error)?;
    ensure_upload_owner(&upload, &client)?;
    let mut response_headers = tus_headers(state.config.max_upload_bytes);
    insert_u64_header(&mut response_headers, "upload-offset", upload.offset)?;
    insert_u64_header(&mut response_headers, "upload-length", upload.total_size)?;
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((StatusCode::OK, response_headers).into_response())
}

async fn patch_resumable_upload(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    verify_tus_version(&headers)?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if content_type != "application/offset+octet-stream" {
        return Err(ApiError::bad_request(
            "chunk Content-Type must be application/offset+octet-stream",
        ));
    }
    let client = client_from_headers(&state, &headers, None)?;
    let offset = required_u64_header(&headers, "upload-offset")?;
    let lock = state.upload_lock(&upload_id).await;
    let _guard = lock.lock().await;
    let mut upload = ResumableUpload::load(&state.store, &state.config.s3_prefix, &upload_id)
        .await
        .map_err(resumable_api_error)?;
    ensure_upload_owner(&upload, &client)?;
    let new_offset = upload
        .append(&state.store, &state.config.s3_prefix, offset, body)
        .await
        .map_err(resumable_api_error)?;
    if new_offset == upload.total_size {
        state.finalize_resumable(&mut upload).await?;
    }
    let mut response_headers = tus_headers(state.config.max_upload_bytes);
    insert_u64_header(&mut response_headers, "upload-offset", new_offset)?;
    Ok((StatusCode::NO_CONTENT, response_headers).into_response())
}

async fn delete_resumable_upload(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    verify_tus_version(&headers)?;
    let client = client_from_headers(&state, &headers, None)?;
    let lock = state.upload_lock(&upload_id).await;
    let _guard = lock.lock().await;
    let upload = ResumableUpload::load(&state.store, &state.config.s3_prefix, &upload_id)
        .await
        .map_err(resumable_api_error)?;
    ensure_upload_owner(&upload, &client)?;
    upload
        .terminate(&state.store, &state.config.s3_prefix)
        .await
        .map_err(resumable_api_error)?;
    Ok((
        StatusCode::NO_CONTENT,
        tus_headers(state.config.max_upload_bytes),
    )
        .into_response())
}

async fn get_resumable_upload_result(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<JobResponse>, ApiError> {
    let client = client_from_headers(&state, &headers, None)?;
    let lock = state.upload_lock(&upload_id).await;
    let _guard = lock.lock().await;
    let mut upload = ResumableUpload::load(&state.store, &state.config.s3_prefix, &upload_id)
        .await
        .map_err(resumable_api_error)?;
    ensure_upload_owner(&upload, &client)?;
    if upload.offset != upload.total_size {
        return Err(ApiError::artifact_not_ready(format!(
            "upload has received {} of {} bytes",
            upload.offset, upload.total_size
        )));
    }
    let job = state.finalize_resumable(&mut upload).await?;
    Ok(Json(state.job_response(&job)?))
}

fn tus_headers(max_upload_bytes: usize) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::HeaderName::from_static("tus-resumable"),
        HeaderValue::from_static(TUS_VERSION),
    );
    headers.insert(
        http::HeaderName::from_static("tus-version"),
        HeaderValue::from_static(TUS_VERSION),
    );
    headers.insert(
        http::HeaderName::from_static("tus-extension"),
        HeaderValue::from_static(TUS_EXTENSIONS),
    );
    if let Ok(value) = HeaderValue::from_str(&max_upload_bytes.to_string()) {
        headers.insert(http::HeaderName::from_static("tus-max-size"), value);
    }
    headers
}

fn verify_tus_version(headers: &HeaderMap) -> Result<(), ApiError> {
    match headers
        .get("tus-resumable")
        .and_then(|value| value.to_str().ok())
    {
        Some(TUS_VERSION) => Ok(()),
        _ => Err(ApiError::bad_request(format!(
            "Tus-Resumable must be {TUS_VERSION}"
        ))),
    }
}

fn required_u64_header(headers: &HeaderMap, name: &'static str) -> Result<u64, ApiError> {
    headers
        .get(name)
        .ok_or_else(|| ApiError::bad_request(format!("missing {name} header")))?
        .to_str()
        .map_err(ApiError::bad_request)?
        .parse()
        .map_err(|_| ApiError::bad_request(format!("invalid {name} header")))
}

fn insert_u64_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: u64,
) -> Result<(), ApiError> {
    headers.insert(
        http::HeaderName::from_static(name),
        HeaderValue::from_str(&value.to_string()).map_err(ApiError::internal)?,
    );
    Ok(())
}

fn parse_upload_metadata(headers: &HeaderMap) -> Result<HashMap<String, String>, ApiError> {
    let Some(raw) = headers
        .get("upload-metadata")
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(HashMap::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, encoded) = pair.split_once(' ').unwrap_or((pair, ""));
            let value = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| ApiError::bad_request("invalid base64 Upload-Metadata value"))?;
            let value = String::from_utf8(value)
                .map_err(|_| ApiError::bad_request("Upload-Metadata value is not UTF-8"))?;
            Ok((key.to_owned(), value))
        })
        .collect()
}

fn ensure_upload_owner(upload: &ResumableUpload, client: &Client) -> Result<(), ApiError> {
    if upload.owner == client.id {
        Ok(())
    } else {
        Err(ApiError::not_found_message("upload session not found"))
    }
}

fn resumable_api_error(error: ResumableUploadError) -> ApiError {
    match error {
        ResumableUploadError::NotFound => ApiError::not_found_message("upload session not found"),
        ResumableUploadError::OffsetMismatch { expected, actual } => ApiError::upload_conflict(
            format!("upload offset mismatch: expected {expected}, received {actual}"),
        ),
        ResumableUploadError::ExceedsLength => {
            ApiError::bad_request("upload chunk exceeds declared file length")
        }
        ResumableUploadError::Incomplete { offset, total } => {
            ApiError::artifact_not_ready(format!("upload has received {offset} of {total} bytes"))
        }
        ResumableUploadError::Completed => {
            ApiError::upload_conflict("upload has already completed")
        }
        ResumableUploadError::Internal(error) => ApiError::internal(error),
    }
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
    Path(public_id): Path<String>,
) -> Result<Json<JobResponse>, ApiError> {
    let job = state
        .public_job(&public_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(state.job_response(&job)?))
}

async fn get_solve_preview(
    State(state): State<AppState>,
    Path(public_id): Path<String>,
    Query(query): Query<PreviewQuery>,
) -> Result<Response, ApiError> {
    let job = state
        .public_job(&public_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    ensure_input_available(&state, &job)?;
    let content = state
        .store
        .get(&job.object_key)
        .await
        .map_err(ApiError::internal)?;
    let preview = if query.full {
        full_png(content, job.original_filename).await
    } else {
        preview_png(content, job.original_filename).await
    }
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

#[derive(Debug, Default, Deserialize)]
struct PreviewQuery {
    #[serde(default)]
    full: bool,
}

async fn get_solve_annotations(
    State(state): State<AppState>,
    Path(public_id): Path<String>,
    Query(query): Query<AnnotationQuery>,
) -> Result<Json<AnnotationResponse>, ApiError> {
    let job = state
        .public_job(&public_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let solution = job.solution.as_ref().ok_or_else(|| {
        ApiError::artifact_not_ready("the solve has not produced annotations yet")
    })?;
    Ok(Json(state.annotations.annotate(
        &public_id,
        solution,
        job.options.capture_time,
        &query.options(),
    )))
}

async fn get_solve_overlay(
    State(state): State<AppState>,
    Path(public_id): Path<String>,
    Query(query): Query<OverlayQuery>,
) -> Result<Response, ApiError> {
    let job = state
        .public_job(&public_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    ensure_input_available(&state, &job)?;
    let stored_solution = job
        .solution
        .as_ref()
        .ok_or_else(|| ApiError::artifact_not_ready("the solve has not produced an overlay yet"))?;
    let mut solution = stored_solution.clone();
    if query.objects {
        solution.objects = state
            .annotations
            .annotate(
                &public_id,
                stored_solution,
                job.options.capture_time,
                &query.annotations.options(),
            )
            .objects;
    }
    let content = state
        .store
        .get(&job.object_key)
        .await
        .map_err(ApiError::internal)?;
    let preview = preview_png(content, job.original_filename)
        .await
        .map_err(ApiError::bad_request)?;
    let svg = render_svg(
        &solution,
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
    #[serde(flatten)]
    annotations: AnnotationQuery,
}

#[derive(Debug, Deserialize)]
struct AnnotationQuery {
    #[serde(default = "default_true")]
    deep_sky: bool,
    #[serde(default = "default_true")]
    named_stars: bool,
    #[serde(default)]
    field_stars: bool,
    #[serde(default = "default_true")]
    transients: bool,
    #[serde(default = "default_true")]
    minor_bodies: bool,
    #[serde(default)]
    historical_transients: bool,
    #[serde(default = "default_field_star_magnitude")]
    field_star_mag_limit: f32,
    #[serde(default = "default_field_star_limit")]
    max_field_stars: usize,
}

impl AnnotationQuery {
    fn options(&self) -> AnnotationOptions {
        AnnotationOptions {
            deep_sky: self.deep_sky,
            named_stars: self.named_stars,
            field_stars: self.field_stars,
            transients: self.transients,
            minor_bodies: self.minor_bodies,
            historical_transients: self.historical_transients,
            field_star_mag_limit: self.field_star_mag_limit.clamp(-2.0, 20.0),
            max_field_stars: self.max_field_stars.clamp(1, 2_000),
        }
    }
}

fn default_field_star_magnitude() -> f32 {
    10.0
}

fn default_field_star_limit() -> usize {
    300
}

fn default_true() -> bool {
    true
}

async fn get_solve_wcs(
    State(state): State<AppState>,
    Path(public_id): Path<String>,
) -> Result<Response, ApiError> {
    let job = state
        .public_job(&public_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
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
    let objects_in_field = job
        .solution
        .as_ref()
        .map(|solution| {
            state
                .annotations
                .annotate(
                    job.id,
                    solution,
                    job.options.capture_time,
                    &AnnotationOptions::default(),
                )
                .objects
                .into_iter()
                .filter(|object| object.kind != "field-star")
                .map(|object| {
                    if object.common_name.trim().is_empty() {
                        object.name
                    } else {
                        format!("{} ({})", object.common_name, object.name)
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut result = json!({
        "status": astro_status(job.status),
        "original_filename": job.original_filename,
        "machine_tags": [],
        "tags": [],
        "objects_in_field": objects_in_field,
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

fn public_job_id(job: &JobRecord) -> Option<String> {
    public_token_from_object_key(&job.object_key).map(|token| format!("{}-{token}", job.id))
}

fn public_id_matches_job(public_id: &str, job_id: JobId, object_key: &str) -> bool {
    public_job_sequence(public_id) == Some(job_id)
        && public_token_from_object_key(object_key)
            .map(|token| format!("{job_id}-{token}"))
            .as_deref()
            == Some(public_id)
}

fn public_job_sequence(public_id: &str) -> Option<JobId> {
    let (sequence, token) = public_id.split_once('-')?;
    Uuid::parse_str(token).ok()?;
    sequence.parse().ok()
}

fn public_token_from_object_key(object_key: &str) -> Option<Uuid> {
    let mut components = object_key.rsplit('/');
    let filename = components.next()?;
    let tagged_parent = components
        .next()
        .and_then(|value| value.strip_prefix("public-"))
        .and_then(|value| Uuid::parse_str(value).ok());
    tagged_parent.or_else(|| {
        let stem = filename.rsplit_once('.').map_or(filename, |(stem, _)| stem);
        Uuid::parse_str(stem).ok()
    })
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

#[derive(Debug)]
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
    fn upload_conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "upload_conflict",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{JobBackend, QueueDelivery, StorageBackend};

    #[test]
    fn public_solution_locators_require_the_random_upload_token() {
        let public_token = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let legacy_token = Uuid::parse_str("019f5c5d-af6b-7930-b0ca-371b62e32bc0").unwrap();
        let storage_token = Uuid::parse_str("019f5c5d-af6b-7930-b0ca-371b62e32bc1").unwrap();
        let new_key = format!("uploads/public-{public_token}/{storage_token}.fits");
        let legacy_key = format!("uploads/{legacy_token}.fits");
        let locator = format!("42-{public_token}");

        assert_eq!(public_token_from_object_key(&new_key), Some(public_token));
        assert_eq!(
            public_token_from_object_key(&legacy_key),
            Some(legacy_token)
        );
        assert_eq!(public_job_sequence(&locator), Some(42));
        assert!(public_id_matches_job(&locator, 42, &new_key));
        assert!(!public_id_matches_job(
            &format!("42-{storage_token}"),
            42,
            &new_key
        ));
        assert!(!public_id_matches_job(&locator, 43, &new_key));
        assert_eq!(public_job_sequence("42"), None);
        assert_eq!(public_job_sequence("42-not-a-token"), None);
    }

    #[tokio::test]
    async fn resumable_upload_survives_restart_and_queues_once() {
        let root = std::env::temp_dir().join(format!("seiza-api-upload-{}", Uuid::now_v7()));
        let config = test_config(&root);
        let state = AppState::new(config.clone()).await.unwrap();
        let mut create_headers = tus_request_headers();
        create_headers.insert("upload-length", HeaderValue::from_static("8"));
        let metadata = [
            ("filename", "field.fits"),
            ("filetype", "application/fits"),
            ("options", "{}"),
        ]
        .map(|(key, value)| {
            format!(
                "{key} {}",
                base64::engine::general_purpose::STANDARD.encode(value)
            )
        })
        .join(",");
        create_headers.insert("upload-metadata", HeaderValue::from_str(&metadata).unwrap());
        let response = create_resumable_upload(State(state.clone()), create_headers)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let location = response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_owned();
        let upload_id = location.rsplit('/').next().unwrap().to_owned();

        let first = patch_resumable_upload(
            State(state.clone()),
            Path(upload_id.clone()),
            chunk_headers(0),
            Bytes::from_static(b"abcd"),
        )
        .await
        .unwrap();
        assert_eq!(first.status(), StatusCode::NO_CONTENT);
        assert_eq!(first.headers()["upload-offset"], "4");
        drop(state);

        let restarted = AppState::new(config).await.unwrap();
        let head = head_resumable_upload(
            State(restarted.clone()),
            Path(upload_id.clone()),
            tus_request_headers(),
        )
        .await
        .unwrap();
        assert_eq!(head.headers()["upload-offset"], "4");
        let mismatch = patch_resumable_upload(
            State(restarted.clone()),
            Path(upload_id.clone()),
            chunk_headers(0),
            Bytes::from_static(b"bad"),
        )
        .await
        .unwrap_err();
        assert_eq!(mismatch.status, StatusCode::CONFLICT);

        patch_resumable_upload(
            State(restarted.clone()),
            Path(upload_id.clone()),
            chunk_headers(4),
            Bytes::from_static(b"efgh"),
        )
        .await
        .unwrap();
        let Json(first_result) = get_resumable_upload_result(
            State(restarted.clone()),
            Path(upload_id.clone()),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        let Json(second_result) = get_resumable_upload_result(
            State(restarted.clone()),
            Path(upload_id),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(first_result.id, second_result.id);
        assert_eq!(first_result.original_filename, "field.fits");
        assert_eq!(first_result.status, JobStatus::Queued);
        assert_eq!(restarted.repository.queue_depth().await.unwrap(), 1);

        drop(restarted);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn tus_request_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("tus-resumable", HeaderValue::from_static(TUS_VERSION));
        headers
    }

    fn chunk_headers(offset: u64) -> HeaderMap {
        let mut headers = tus_request_headers();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/offset+octet-stream"),
        );
        headers.insert(
            "upload-offset",
            HeaderValue::from_str(&offset.to_string()).unwrap(),
        );
        headers
    }

    fn test_config(root: &std::path::Path) -> Config {
        Config {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            frontend_dir: root.join("frontend"),
            data_dir: root.to_owned(),
            catalog_path: None,
            blind_index_path: None,
            object_catalog_path: None,
            transient_catalog_path: None,
            minor_body_catalog_path: None,
            job_backend: JobBackend::Sqlx,
            sql_database_url: format!("sqlite://{}?mode=rwc", root.join("jobs.sqlite3").display()),
            dynamodb_table: None,
            queue_transport: QueueDelivery::Local,
            sqs_queue_url: None,
            embedded_workers: false,
            worker_token: None,
            lease_seconds: 900,
            worker_count: 1,
            max_upload_bytes: 1_024,
            upload_retention_seconds: 86_400,
            upload_cleanup_interval_seconds: 3_600,
            rate_limit_per_minute: 60.0,
            rate_limit_burst: 10.0,
            auth_mode: AuthMode::Public,
            storage_backend: StorageBackend::Local,
            s3_bucket: None,
            s3_prefix: "uploads".into(),
        }
    }
}
