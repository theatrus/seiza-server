use crate::{
    annotations::{AnnotationEngine, AnnotationOptions},
    auth::{AuthError, AuthService, AuthenticatedBrowserSession, EmailCredential},
    config::{AuthMode, Config, JobBackend},
    identity::{IdentityRepository, identity_repository},
    models::{
        AstrometryId, JobId, JobLease, JobRecord, JobResponse, JobStatus, SolutionResponse,
        SolveOptions, ValidationDonation, ValidationDonationResponse, WorkerCompletion,
    },
    overlay::{OverlayOptions, render_svg},
    rate_limit::RateLimiter,
    repository::{JobRepository, job_repository},
    solver::{
        FITS_HEADER_PROBE_BYTES, SolverEngine, dimensions_from_bytes, full_png,
        prepare_solve_options, preview_png,
    },
    star_identifiers::StarIdentifierMatch,
    storage::{ObjectStore, object_store},
    transport::{QueueTransport, queue_transport},
    uploads::{PersistedJobId, ResumableUpload, ResumableUploadError, TUS_EXTENSIONS, TUS_VERSION},
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Form, Multipart, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use base64::Engine;
use bytes::Bytes;
use chrono::{Duration as ChronoDuration, Utc};
use cookie::{Cookie, SameSite, time::Duration as CookieDuration};
use seiza::objects::{
    ObjectCatalogCapabilities, ObjectCatalogProvenance, ObjectDetails, ObjectHit, ObjectKind,
    ObjectNameMatch, ObjectQuery, ObjectQueryError, ObjectSort, SkyObject, SkyRegion,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::{Arc, Weak},
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{Mutex, Notify};
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use uuid::Uuid;

mod catalog;
use self::catalog::*;
mod tus;
use self::tus::*;
mod auth;
use self::auth::*;

const VALIDATION_LICENSE_VERSION: &str = "seiza-validation-image-grant-v2";
const MAX_VALIDATION_COMMENT_BYTES: usize = 2_000;

#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
    pub identity: Option<Arc<dyn IdentityRepository>>,
    pub auth: Option<Arc<AuthService>>,
    repository: Arc<dyn JobRepository>,
    transport: Arc<dyn QueueTransport>,
    limiter: RateLimiter,
    store: Arc<dyn ObjectStore>,
    solver: SolverEngine,
    annotations: AnnotationEngine,
    upload_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    embedded_worker_wakeup: Arc<Notify>,
}

impl AppState {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let store = object_store(&config).await?;
        let repository = job_repository(&config).await?;
        let identity = identity_repository(&config).await?;
        let auth = match identity.clone() {
            Some(identity) => Some(Arc::new(AuthService::from_config(&config, identity).await?)),
            None => None,
        };
        let transport = queue_transport(&config).await?;
        let solver = SolverEngine::from_catalog_paths(
            config.catalog_path.as_deref(),
            config.blind_index_path.as_deref(),
        );
        let annotations = AnnotationEngine::new(
            solver.catalog(),
            config.catalog_path.as_deref(),
            config.object_catalog_path.as_deref(),
            config.star_identifier_catalog_path.as_deref(),
            config.transient_catalog_path.as_deref(),
            config.minor_body_catalog_path.as_deref(),
        );
        Ok(Self {
            limiter: RateLimiter::new(config.rate_limit_per_minute, config.rate_limit_burst),
            config: Arc::new(config),
            identity,
            auth,
            repository,
            transport,
            store,
            solver,
            annotations,
            upload_locks: Arc::new(Mutex::new(HashMap::new())),
            embedded_worker_wakeup: Arc::new(Notify::new()),
        })
    }

    pub fn start_background_tasks(&self) {
        let state = self.clone();
        tokio::spawn(async move { state.cleanup_expired_uploads().await });
        if let Some(identity) = self.identity.clone() {
            let interval_seconds = self.config.upload_cleanup_interval_seconds;
            tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(Duration::from_secs(interval_seconds.max(60)));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    match identity.purge_expired(Utc::now()).await {
                        Ok(0) => {}
                        Ok(purged) => {
                            tracing::info!(purged, "purged expired identity records");
                        }
                        Err(error) => {
                            tracing::warn!(%error, "could not purge expired identity records");
                        }
                    }
                }
            });
        }
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
                let fallback_poll = Duration::from_secs(state.config.lease_seconds);
                loop {
                    // Register before checking the repository so a job queued
                    // concurrently with the startup/recovery claim cannot miss
                    // its wakeup.
                    let wakeup = state.embedded_worker_wakeup.notified();
                    tokio::pin!(wakeup);
                    wakeup.as_mut().enable();
                    match state
                        .repository
                        .claim(None, state.config.lease_seconds)
                        .await
                    {
                        Ok(Some(lease)) => state.run_embedded_job(lease).await,
                        Ok(None) => {
                            tokio::select! {
                                _ = &mut wakeup => {}
                                _ = tokio::time::sleep(fallback_poll) => {}
                            }
                        }
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
            match self
                .store
                .delete_older_than(cutoff, std::slice::from_ref(&self.config.validation_prefix))
                .await
            {
                Ok(0) => {}
                Ok(removed) => tracing::info!(removed, "deleted expired uploaded images"),
                Err(error) => tracing::error!(%error, "failed to clean expired uploaded images"),
            }
        }
    }

    async fn run_embedded_job(&self, lease: JobLease) {
        let Some(object_key) = self.repository.input_key(lease.job_id, lease.lease_token.clone()).await.unwrap_or_else(|error| {
            tracing::error!(job_id = %lease.job_id, %error, "failed to resolve durable job input");
            None
        }) else { return };
        tracing::info!(job_id = %lease.job_id, filename = %lease.original_filename, "starting durable queued solve");
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
                    job_id = %lease.job_id,
                    matched_stars = solution.matched_stars,
                    rms_arcsec = solution.rms_arcsec,
                    solver_ms = solution.statistics.as_ref().map(|stats| stats.total_ms),
                    "plate solve succeeded"
                );
                WorkerCompletion {
                    lease_token: lease.lease_token.clone(),
                    solution: Some(solution),
                    error: None,
                }
            }
            Err(error) => {
                tracing::warn!(job_id = %lease.job_id, error = %error, "plate solve failed");
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
                job_id = %lease.job_id,
                "embedded worker lost its lease before completion"
            ),
            Err(error) => {
                tracing::error!(job_id = %lease.job_id, %error, "failed to persist worker completion")
            }
        };
    }

    async fn dispatch_outbox(&self) {
        let retry_interval = match self.config.job_backend {
            JobBackend::Sqlx => Duration::from_secs(2),
            JobBackend::DynamoDb => Duration::from_secs(self.config.lease_seconds.max(1)),
        };
        tracing::info!(
            retry_interval_seconds = retry_interval.as_secs(),
            "external queue recovery dispatcher started"
        );
        loop {
            match self.repository.pending_notifications(100).await {
                Ok(job_ids) => {
                    for job_id in job_ids {
                        let job = match self.repository.get(job_id).await {
                            Ok(Some(job)) => job,
                            Ok(None) => {
                                tracing::warn!(%job_id, "discarding outbox notification for a missing job");
                                if let Err(error) =
                                    self.repository.mark_notification_delivered(job_id).await
                                {
                                    tracing::error!(%error, %job_id, "failed to discard orphaned durable queue notification");
                                }
                                continue;
                            }
                            Err(error) => {
                                tracing::warn!(%error, %job_id, "failed to load durable queue job; keeping outbox record");
                                continue;
                            }
                        };
                        match self.transport.publish(&job).await {
                            Ok(()) => {
                                if let Err(error) =
                                    self.repository.mark_notification_delivered(job_id).await
                                {
                                    tracing::error!(%error, %job_id, "failed to acknowledge durable queue notification");
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, %job_id, "external queue publish failed; keeping outbox record")
                            }
                        }
                    }
                }
                Err(error) => tracing::error!(%error, "failed to read durable queue outbox"),
            }
            tokio::time::sleep(retry_interval).await;
        }
    }

    async fn astrometry_job(
        &self,
        astrometry_id: AstrometryId,
    ) -> Result<Option<JobRecord>, ApiError> {
        self.repository
            .get_by_astrometry_id(astrometry_id)
            .await
            .map_err(ApiError::internal)
    }

    async fn public_job(&self, public_id: &str) -> Result<Option<JobRecord>, ApiError> {
        if let Ok(job_id) = Uuid::parse_str(public_id) {
            return self
                .repository
                .get(job_id)
                .await
                .map_err(ApiError::internal);
        }
        let Some((legacy_id, job_id)) = legacy_public_job_id(public_id) else {
            return Ok(None);
        };
        let job = self
            .repository
            .get_by_legacy_id(legacy_id)
            .await
            .map_err(ApiError::internal)?;
        Ok(job.filter(|job| job.id == job_id))
    }

    fn input_expires_at(&self, job: &JobRecord) -> chrono::DateTime<Utc> {
        job.created_at + ChronoDuration::seconds(self.config.upload_retention_seconds as i64)
    }

    fn input_available(&self, job: &JobRecord) -> bool {
        job.validation_donation.is_some() || Utc::now() < self.input_expires_at(job)
    }

    fn job_response(&self, job: &JobRecord) -> Result<JobResponse, ApiError> {
        let public_id = public_job_id(job);
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
            solve_time_ms: solve_time_ms(job.started_at, job.completed_at),
            original_filename: job.original_filename.clone(),
            options: job.options.clone(),
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
            validation_donation: job
                .validation_donation
                .as_ref()
                .map(ValidationDonationResponse::from),
        })
    }

    async fn submit(
        &self,
        client: Client,
        upload: UploadedFile,
        options: SolveOptions,
    ) -> Result<JobResponse, ApiError> {
        let job = self.submit_job(client, upload, options).await?;
        self.job_response(&job)
    }

    async fn submit_job(
        &self,
        client: Client,
        upload: UploadedFile,
        mut options: SolveOptions,
    ) -> Result<JobRecord, ApiError> {
        prepare_solve_options(&mut options, &upload.data, &upload.filename);
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
        self.enqueue_stored(
            client,
            object_key,
            upload.filename,
            upload.content_type,
            options,
        )
        .await
    }

    fn new_object_key(&self, filename: &str) -> String {
        let extension = safe_extension(filename);
        let prefix = self.config.s3_prefix.trim_matches('/');
        let job_id = Uuid::new_v4();
        let storage_token = Uuid::now_v7();
        if prefix.is_empty() {
            format!("public-{job_id}/{storage_token}.{extension}")
        } else {
            format!("{prefix}/public-{job_id}/{storage_token}.{extension}")
        }
    }

    fn validation_object_key(&self, job: &JobRecord) -> Result<String, ApiError> {
        let stored_name = job
            .object_key
            .rsplit('/')
            .next()
            .ok_or_else(|| ApiError::internal("job object key has no filename"))?;
        Ok(format!(
            "{}/public-{}/{stored_name}",
            self.config.validation_prefix, job.id
        ))
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
            if job.status == JobStatus::Queued {
                self.embedded_worker_wakeup.notify_waiters();
            }
            return Ok(job);
        }
        let created_at = Utc::now();
        let job_id = job_id_from_object_key(&object_key)
            .ok_or_else(|| ApiError::internal("job object key has no job UUID"))?;
        let job = JobRecord {
            id: job_id,
            astrometry_id: 0,
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
            validation_donation: None,
        };
        let job = self
            .repository
            .enqueue(job)
            .await
            .map_err(ApiError::internal)?;
        if job.status == JobStatus::Queued {
            self.embedded_worker_wakeup.notify_waiters();
        }
        if self.transport.uses_external_queue() {
            match self.transport.publish(&job).await {
                Ok(()) => {
                    if let Err(error) = self.repository.mark_notification_delivered(job.id).await {
                        // The job and its input are already durable, and SQS may
                        // already deliver this notification. Leave the outbox
                        // pending so recovery can safely publish a duplicate.
                        tracing::warn!(job_id = %job.id, %error, "external queue publish succeeded but outbox acknowledgement failed; keeping the notification pending");
                    }
                }
                Err(error) => {
                    tracing::warn!(job_id = %job.id, %error, "external queue publish deferred to durable outbox")
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
            let job = match job_id {
                PersistedJobId::Uuid(job_id) => self.repository.get(job_id).await,
                PersistedJobId::Legacy(job_id) => self.repository.get_by_legacy_id(job_id).await,
            }
            .map_err(ApiError::internal)?;
            return job.ok_or_else(|| ApiError::internal("completed upload job is missing"));
        }
        let header_prefix = upload
            .read_prefix(&self.store, FITS_HEADER_PROBE_BYTES)
            .await
            .map_err(resumable_api_error)?;
        prepare_solve_options(
            &mut upload.options,
            &header_prefix,
            &upload.original_filename,
        );
        let compose_started = Instant::now();
        upload
            .compose(&self.store)
            .await
            .map_err(resumable_api_error)?;
        tracing::info!(
            upload_id = %upload.id,
            bytes = upload.total_size,
            compose_ms = compose_started.elapsed().as_secs_f64() * 1_000.0,
            "composed resumable upload"
        );
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
        upload.job_id = Some(job.id.into());
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
    let frontend_index = frontend_dir.join("index.html");
    let cors = cors_layer(&state);
    Router::new()
        .route("/api/v1/health", get(get_health))
        .route("/api/v1/auth/email/start", post(start_email_sign_in))
        .route("/api/v1/auth/email/complete", post(complete_email_sign_in))
        .route(
            "/api/v1/auth/passkeys/authentication/start",
            post(start_passkey_sign_in),
        )
        .route(
            "/api/v1/auth/passkeys/authentication/complete",
            post(complete_passkey_sign_in),
        )
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/logout-all", post(logout_all))
        .route("/api/v1/account", get(get_account))
        .route("/api/v1/account/passkeys", get(list_passkeys))
        .route(
            "/api/v1/account/passkeys/registration/start",
            post(start_passkey_registration),
        )
        .route(
            "/api/v1/account/passkeys/registration/complete",
            post(complete_passkey_registration),
        )
        .route(
            "/api/v1/account/passkeys/{passkey_id}",
            axum::routing::delete(revoke_passkey),
        )
        .route(
            "/api/v1/account/api-keys",
            get(list_api_keys).post(create_api_key),
        )
        .route(
            "/api/v1/account/api-keys/{key_id}",
            axum::routing::delete(revoke_api_key),
        )
        .route(
            "/api/v1/account/sessions/{session_id}",
            axum::routing::delete(revoke_account_session),
        )
        // Auth and account bodies are small JSON documents; they must not
        // inherit the multi-hundred-megabyte upload body limit.
        .route_layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT_BYTES))
        .route("/api/v1/catalog/objects", get(get_catalog_objects))
        .route(
            "/api/v1/catalog/objects/search",
            get(search_catalog_objects),
        )
        .route(
            "/api/v1/catalog/objects/details/{canonical_id}",
            get(get_catalog_object_details),
        )
        .route("/api/v1/catalog/stars/search", get(search_star_identifiers))
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
        .route("/api/v1/solves/{job_id}/resolve", post(resolve_solve))
        // Backward-compatible alias for clients using the original failed-job
        // retry endpoint. Both routes now create a new immutable job UUID.
        .route("/api/v1/solves/{job_id}/retry", post(resolve_solve))
        .route(
            "/api/v1/solves/{job_id}/validation-donation",
            post(donate_validation_image),
        )
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
        // Known client-side routes must return a successful document response
        // when loaded directly or refreshed. The final fallback keeps the SPA's
        // rendered not-found page while preserving its 404 status.
        .route_service("/", ServeFile::new(frontend_index.clone()))
        .route_service("/solve", ServeFile::new(frontend_index.clone()))
        .route_service("/docs/api", ServeFile::new(frontend_index.clone()))
        .route_service("/data-sources", ServeFile::new(frontend_index.clone()))
        .route_service("/signin", ServeFile::new(frontend_index.clone()))
        .route_service("/account", ServeFile::new(frontend_index.clone()))
        .route_service(
            "/solutions/{job_id}",
            ServeFile::new(frontend_index.clone()),
        )
        .fallback_service(
            ServeDir::new(&frontend_dir).not_found_service(ServeFile::new(frontend_index)),
        )
        .with_state(state.clone())
        .layer(DefaultBodyLimit::max(state.config.max_upload_bytes))
        .layer(RequestBodyLimitLayer::new(state.config.max_upload_bytes))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
}

fn cors_layer(state: &AppState) -> CorsLayer {
    let layer = CorsLayer::new()
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
            header::IF_NONE_MATCH,
            http::HeaderName::from_static("x-api-key"),
            http::HeaderName::from_static("x-csrf-token"),
            http::HeaderName::from_static("tus-resumable"),
            http::HeaderName::from_static("upload-length"),
            http::HeaderName::from_static("upload-offset"),
            http::HeaderName::from_static("upload-metadata"),
            http::HeaderName::from_static("upload-concat"),
        ])
        .expose_headers([
            header::LOCATION,
            header::CACHE_CONTROL,
            header::ETAG,
            http::HeaderName::from_static("tus-resumable"),
            http::HeaderName::from_static("upload-length"),
            http::HeaderName::from_static("upload-offset"),
            http::HeaderName::from_static("upload-metadata"),
            http::HeaderName::from_static("upload-concat"),
        ]);
    if let Some(auth) = state.auth.as_ref() {
        let origin = HeaderValue::from_str(auth.public_origin())
            .expect("validated public base URL produces a header-safe origin");
        layer.allow_origin(origin).allow_credentials(true)
    } else {
        layer.allow_origin(Any)
    }
}

async fn get_health(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let status = if state.solver.is_ready() {
        "ready"
    } else {
        "degraded"
    };
    Ok(Json(json!({
        "status": status,
        "versions": {
            "seiza_server": env!("CARGO_PKG_VERSION"),
            "seiza": env!("SEIZA_DEP_VERSION"),
        },
        "solver_ready": state.solver.is_ready(),
        "queue_depth": state.repository.queue_depth().await.map_err(ApiError::internal)?,
        "auth_mode": match state.config.auth_mode { AuthMode::Public => "public", AuthMode::StubApiKey => "stub-api-key", AuthMode::Accounts => "accounts" },
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
    let client = client_from_headers(&state, &headers, None, true).await?;
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
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let job = state
        .public_job(&public_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    cached_json_response(
        &headers,
        job_cache_control(job.status),
        &state.job_response(&job)?,
    )
}

async fn resolve_solve(
    State(state): State<AppState>,
    Path(public_id): Path<String>,
    headers: HeaderMap,
    Json(mut options): Json<SolveOptions>,
) -> Result<(StatusCode, Json<JobResponse>), ApiError> {
    let client = client_from_headers(&state, &headers, None, true).await?;
    let job = state
        .public_job(&public_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if !matches!(job.status, JobStatus::Succeeded | JobStatus::Failed) {
        return Err(ApiError::retry_conflict(
            "only a completed solve can be re-solved",
        ));
    }
    ensure_input_available(&state, &job)?;
    if !state
        .store
        .exists(job.input_object_key())
        .await
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::gone(
            "the retained upload is no longer available; upload the image again",
        ));
    }
    if options.capture_time.is_none() {
        options.capture_time = job.options.capture_time;
    }
    prepare_solve_options(&mut options, &[], &job.original_filename);
    options.validate().map_err(ApiError::bad_request)?;
    state
        .limiter
        .check(&client.id)
        .await
        .map_err(ApiError::rate_limited)?;
    let object_key = state.new_object_key(&job.original_filename);
    state
        .store
        .copy(
            job.input_object_key(),
            &object_key,
            job.content_type.as_deref(),
        )
        .await
        .map_err(ApiError::internal)?;
    let resolved = match state
        .enqueue_stored(
            client,
            object_key.clone(),
            job.original_filename.clone(),
            job.content_type.clone(),
            options,
        )
        .await
    {
        Ok(job) => job,
        Err(error) => {
            // A database/network error can be ambiguous after persistence. Do
            // not delete an input that a durable queued job may already own.
            let safe_to_delete = match state.repository.find_by_object_key(&object_key).await {
                Ok(Some(job)) => {
                    tracing::warn!(job_id = %job.id, %object_key, "re-solve enqueue returned an error after the job was persisted; preserving its input");
                    false
                }
                Ok(None) => true,
                Err(lookup_error) => {
                    tracing::warn!(%lookup_error, %object_key, "could not confirm whether the failed re-solve enqueue persisted; preserving its input");
                    false
                }
            };
            if safe_to_delete && let Err(cleanup_error) = state.store.delete(&object_key).await {
                tracing::warn!(%cleanup_error, %object_key, "could not clean up failed re-solve copy");
            }
            return Err(error);
        }
    };
    Ok((StatusCode::ACCEPTED, Json(state.job_response(&resolved)?)))
}

#[derive(Debug, Deserialize)]
struct ValidationDonationRequest {
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    solve_is_invalid: bool,
    #[serde(default)]
    license_agreed: bool,
}

async fn donate_validation_image(
    State(state): State<AppState>,
    Path(public_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ValidationDonationRequest>,
) -> Result<Json<JobResponse>, ApiError> {
    let _client = client_from_headers(&state, &headers, None, true).await?;
    if !request.license_agreed {
        return Err(ApiError::bad_request(
            "license_agreed must be true to contribute an image",
        ));
    }
    let comment = request.comment.and_then(|comment| {
        let comment = comment.trim().to_owned();
        (!comment.is_empty()).then_some(comment)
    });
    if comment
        .as_ref()
        .is_some_and(|comment| comment.len() > MAX_VALIDATION_COMMENT_BYTES)
    {
        return Err(ApiError::bad_request(format!(
            "validation comment must not exceed {MAX_VALIDATION_COMMENT_BYTES} bytes"
        )));
    }
    let job = state
        .public_job(&public_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if !matches!(job.status, JobStatus::Succeeded | JobStatus::Failed) {
        return Err(ApiError::validation_conflict(
            "only a completed solve can be contributed to the validation set",
        ));
    }

    let validation_object_key = job
        .validation_donation
        .as_ref()
        .map(|donation| donation.object_key.clone())
        .unwrap_or(state.validation_object_key(&job)?);
    if job.validation_donation.is_none() {
        ensure_input_available(&state, &job)?;
        let source_key = job.input_object_key();
        if !state
            .store
            .exists(source_key)
            .await
            .map_err(ApiError::internal)?
        {
            return Err(ApiError::gone(
                "the temporary upload is no longer available to contribute",
            ));
        }
        state
            .store
            .copy(
                source_key,
                &validation_object_key,
                job.content_type.as_deref(),
            )
            .await
            .map_err(ApiError::internal)?;
    }

    let donated_at = job
        .validation_donation
        .as_ref()
        .map_or_else(Utc::now, |donation| donation.donated_at);
    if !state
        .repository
        .donate_validation(
            job.id,
            ValidationDonation {
                object_key: validation_object_key,
                comment,
                solve_is_invalid: request.solve_is_invalid,
                license_version: VALIDATION_LICENSE_VERSION.into(),
                donated_at,
            },
        )
        .await
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::validation_conflict(
            "the solve is no longer in a completed state",
        ));
    }
    let donated = state
        .repository
        .get(job.id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::internal("contributed solve job is missing"))?;
    Ok(Json(state.job_response(&donated)?))
}

async fn get_solve_preview(
    State(state): State<AppState>,
    Path(public_id): Path<String>,
    Query(query): Query<PreviewQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let job = state
        .public_job(&public_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    ensure_input_available(&state, &job)?;
    let content = state
        .store
        .get(job.input_object_key())
        .await
        .map_err(ApiError::internal)?;
    let preview = if query.full {
        full_png(content, job.original_filename).await
    } else {
        preview_png(content, job.original_filename).await
    }
    .map_err(ApiError::bad_request)?;
    Ok(cached_body_response(
        &headers,
        "image/png",
        "private, max-age=300",
        preview,
    ))
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
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let job = state
        .public_job(&public_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let solution = job.solution.as_ref().ok_or_else(|| {
        ApiError::artifact_not_ready("the solve has not produced annotations yet")
    })?;
    let annotations = state.annotations.annotate(
        &public_id,
        solution,
        job.options.capture_time,
        &query.options(),
    );
    cached_json_response(
        &headers,
        "public, max-age=300, stale-while-revalidate=3600",
        &annotations,
    )
}

async fn get_solve_overlay(
    State(state): State<AppState>,
    Path(public_id): Path<String>,
    Query(query): Query<OverlayQuery>,
    headers: HeaderMap,
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
        .get(job.input_object_key())
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
            outlines: query.outlines,
        },
    );
    let mut response = cached_body_response(
        &headers,
        "image/svg+xml; charset=utf-8",
        "private, max-age=300",
        Bytes::from(svg),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline; filename=seiza-overlay.svg"),
    );
    Ok(response)
}

#[derive(Debug, Deserialize)]
struct OverlayQuery {
    #[serde(default = "default_true")]
    objects: bool,
    #[serde(default = "default_true")]
    outlines: bool,
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
    star_identifiers: bool,
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
    #[serde(default = "default_star_identifier_magnitude")]
    star_identifier_mag_limit: f32,
    #[serde(default = "default_star_identifier_limit")]
    max_star_identifiers: usize,
}

impl AnnotationQuery {
    fn options(&self) -> AnnotationOptions {
        AnnotationOptions {
            deep_sky: self.deep_sky,
            named_stars: self.named_stars,
            star_identifiers: self.star_identifiers,
            field_stars: self.field_stars,
            transients: self.transients,
            minor_bodies: self.minor_bodies,
            historical_transients: self.historical_transients,
            field_star_mag_limit: self.field_star_mag_limit.clamp(-2.0, 20.0),
            max_field_stars: self.max_field_stars.clamp(1, 2_000),
            star_identifier_mag_limit: self.star_identifier_mag_limit.clamp(-2.0, 20.0),
            max_star_identifiers: self.max_star_identifiers.clamp(1, 1_000),
        }
    }
}

fn default_field_star_magnitude() -> f32 {
    10.0
}

fn default_field_star_limit() -> usize {
    300
}

fn default_star_identifier_magnitude() -> f32 {
    10.0
}

fn default_star_identifier_limit() -> usize {
    150
}

fn default_true() -> bool {
    true
}

async fn get_solve_wcs(
    State(state): State<AppState>,
    Path(public_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let job = state
        .public_job(&public_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let solution = job
        .solution
        .as_ref()
        .ok_or_else(|| ApiError::artifact_not_ready("the solve has not produced WCS data yet"))?;
    let mut response = cached_body_response(
        &headers,
        "text/plain; charset=utf-8",
        "public, max-age=31536000, immutable",
        Bytes::from(solution.fits_wcs_header()),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=seiza-solution.wcs"),
    );
    Ok(response)
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
    if let Some(solution) = &completion.solution {
        solution.validate().map_err(ApiError::bad_request)?;
    }
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
    if state.config.auth_mode == AuthMode::Accounts {
        let api_key = request
            .apikey
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ApiError::unauthorized("an account API key is required"))?;
        let session = auth_service(&state)?
            .create_astrometry_session(api_key)
            .await
            .map_err(auth_api_error)?;
        return Ok(Json(json!({
            "status": "success",
            "message": "authenticated by Seiza account API key",
            "session": session.token,
        })));
    }
    if state.config.auth_mode == AuthMode::StubApiKey
        && request.apikey.as_deref().is_none_or(str::is_empty)
    {
        return Err(ApiError::unauthorized(
            "an API key is required while SEIZA_AUTH_MODE=stub-api-key",
        ));
    }
    Ok(Json(json!({
        "status": "success",
        "message": "authenticated by Seiza server (public/stub mode)",
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
    let client = client_from_headers(&state, &headers, request.session.as_deref(), true).await?;
    let dimensions =
        dimensions_from_bytes(&upload.data, &upload.filename).map_err(ApiError::bad_request)?;
    let options = request.into_options(dimensions)?;
    let job = state.submit_job(client, upload, options).await?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "status": "success",
            "subid": job.astrometry_id,
            "hash": format!("seiza-job-{}", job.astrometry_id),
        })),
    ))
}

async fn astrometry_submission(
    State(state): State<AppState>,
    Path(job_id): Path<AstrometryId>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let job = state
        .astrometry_job(job_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    cached_json_response(
        &headers,
        job_cache_control(job.status),
        &json!({
            "processing_started": job.started_at,
            "processing_finished": job.completed_at,
            "jobs": [job.astrometry_id],
            "job_calibrations": if job.status == JobStatus::Succeeded { vec![json!([job.astrometry_id, job.astrometry_id])] } else { Vec::new() },
            "user_images": [job.astrometry_id],
        }),
    )
}

async fn astrometry_job(
    State(state): State<AppState>,
    Path(job_id): Path<AstrometryId>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let job = state
        .astrometry_job(job_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    cached_json_response(
        &headers,
        job_cache_control(job.status),
        &json!({ "status": astro_status(job.status) }),
    )
}

async fn astrometry_calibration(
    State(state): State<AppState>,
    Path(job_id): Path<AstrometryId>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let job = state
        .astrometry_job(job_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let (cache_control, response) = match job.solution {
        Some(solution) => (
            "public, max-age=31536000, immutable",
            calibration_json(&solution),
        ),
        None => (
            job_cache_control(job.status),
            json!({ "status": astro_status(job.status) }),
        ),
    };
    cached_json_response(&headers, cache_control, &response)
}

async fn astrometry_info(
    State(state): State<AppState>,
    Path(job_id): Path<AstrometryId>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let job = state
        .astrometry_job(job_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
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
    cached_json_response(
        &headers,
        if matches!(job.status, JobStatus::Queued | JobStatus::Solving) {
            "no-store"
        } else {
            "private, max-age=300, stale-while-revalidate=3600"
        },
        &result,
    )
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

#[derive(Clone, Debug)]
struct Client {
    id: String,
    queue_weight: f64,
}

async fn client_from_headers(
    state: &AppState,
    headers: &HeaderMap,
    astrometry_session: Option<&str>,
    mutation: bool,
) -> Result<Client, ApiError> {
    if state.config.auth_mode == AuthMode::Accounts {
        let api_key = request_api_key(headers);
        if astrometry_session.is_some() && api_key.is_some() {
            return Err(ApiError::unauthorized(
                "provide exactly one account credential",
            ));
        }
        if let Some(session) = astrometry_session {
            let authenticated = auth_service(state)?
                .authenticate_astrometry_session(session)
                .await
                .map_err(auth_api_error)?;
            return Ok(Client {
                id: format!("account:{}", authenticated.account.id),
                queue_weight: authenticated.queue_weight,
            });
        }
        if let Some(api_key) = api_key {
            let scope = if mutation {
                crate::auth::SCOPE_SOLVE_SUBMIT
            } else {
                crate::auth::SCOPE_SOLVE_READ
            };
            let authenticated = auth_service(state)?
                .authenticate_api_key(api_key, scope)
                .await
                .map_err(auth_api_error)?;
            return Ok(Client {
                id: format!("account:{}", authenticated.account.id),
                queue_weight: authenticated.api_key.queue_weight,
            });
        }
        let authenticated = if mutation {
            authenticated_browser_for_mutation(state, headers).await?
        } else {
            authenticated_browser(state, headers).await?
        };
        return Ok(Client {
            id: format!("account:{}", authenticated.account.id),
            queue_weight: 1.0,
        });
    }

    let api_key = request_api_key(headers)
        .or(astrometry_session)
        .filter(|value| !value.trim().is_empty());
    let id = match (state.config.auth_mode, api_key) {
        (AuthMode::Accounts, _) => unreachable!("accounts mode is handled above"),
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
            public_client_id(source_ip)
        }
    };
    Ok(Client {
        id,
        queue_weight: state.config.queue_weight_for_api_key(api_key),
    })
}

fn request_api_key(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        })
        .filter(|value| !value.trim().is_empty())
}

fn public_client_id(source_ip: &str) -> String {
    match source_ip.trim().parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => format!("public:{address}"),
        Ok(IpAddr::V6(address)) => {
            let prefix = u128::from(address) & (u128::MAX << 64);
            format!("public:{}", Ipv6Addr::from(prefix))
        }
        Err(_) => "public:anonymous".into(),
    }
}

fn stable_hash(value: &str) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn solve_time_ms(
    started_at: Option<chrono::DateTime<Utc>>,
    completed_at: Option<chrono::DateTime<Utc>>,
) -> Option<u64> {
    started_at
        .zip(completed_at)
        .map(|(started, completed)| (completed - started).num_milliseconds().max(0) as u64)
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

fn public_job_id(job: &JobRecord) -> String {
    job.id.to_string()
}

fn legacy_public_job_id(public_id: &str) -> Option<(u64, Uuid)> {
    let (sequence, token) = public_id.split_once('-')?;
    Some((sequence.parse().ok()?, Uuid::parse_str(token).ok()?))
}

fn job_id_from_object_key(object_key: &str) -> Option<Uuid> {
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

fn cached_json_response<T: Serialize>(
    request_headers: &HeaderMap,
    cache_control: &'static str,
    value: &T,
) -> Result<Response, ApiError> {
    let body = serde_json::to_vec(value).map_err(ApiError::internal)?;
    Ok(cached_body_response(
        request_headers,
        "application/json",
        cache_control,
        Bytes::from(body),
    ))
}

fn job_cache_control(status: JobStatus) -> &'static str {
    if matches!(status, JobStatus::Queued | JobStatus::Solving) {
        "no-store"
    } else {
        "private, max-age=30, stale-while-revalidate=120"
    }
}

fn cached_body_response(
    request_headers: &HeaderMap,
    content_type: &'static str,
    cache_control: &'static str,
    body: Bytes,
) -> Response {
    let mut hasher = DefaultHasher::new();
    body.hash(&mut hasher);
    let etag = format!("W/\"{:016x}\"", hasher.finish());
    let not_modified = request_headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|candidate| candidate.trim() == "*" || candidate.trim() == etag);
    let mut response = if not_modified {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        body.into_response()
    };
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).expect("valid ETag"),
    );
    response
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
    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
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
    fn retry_conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "retry_conflict",
            message: message.into(),
            retry_after: None,
        }
    }
    fn validation_conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "validation_conflict",
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
    fn service_unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "catalog_unavailable",
            message: message.into(),
            retry_after: None,
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
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{JobBackend, QueueDelivery, StorageBackend};
    use crate::{
        email::{EmailSender, SignInEmail},
        sqlx_identity::SqlxIdentityRepository,
    };
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use seiza::objects::{ObjectCatalog, ObjectMetadata};
    use seiza::star_ids::{
        StarIdentifier, StarIdentifierCatalogBuilder, StarNameCatalog, StarNameKind,
    };
    use tower::ServiceExt;
    use url::Url;

    #[derive(Default)]
    struct CapturingEmailSender(Mutex<Vec<SignInEmail>>);

    #[async_trait::async_trait]
    impl EmailSender for CapturingEmailSender {
        async fn send_sign_in(&self, email: SignInEmail) -> anyhow::Result<()> {
            self.0.lock().await.push(email);
            Ok(())
        }
    }

    fn solved_fixture() -> SolutionResponse {
        SolutionResponse {
            center_ra_deg: 202.47,
            center_dec_deg: 47.2,
            pixel_scale_arcsec_per_pixel: 1.35,
            matched_stars: 42,
            rms_arcsec: 0.8,
            image_width: 1200,
            image_height: 800,
            wcs: crate::models::WcsResponse {
                crval: [202.47, 47.2],
                crpix: [600.0, 400.0],
                cd: [[-0.000375, 0.0], [0.0, 0.000375]],
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
        }
    }

    #[tokio::test]
    async fn frontend_routes_are_successful_on_refresh_and_unknown_paths_remain_not_found() {
        let root = std::env::temp_dir().join(format!("seiza-api-frontend-{}", Uuid::now_v7()));
        let frontend_dir = root.join("frontend");
        std::fs::create_dir_all(&frontend_dir).unwrap();
        std::fs::write(frontend_dir.join("index.html"), "seiza frontend").unwrap();
        let app = router(AppState::new(test_config(&root)).await.unwrap());

        for path in [
            "/",
            "/solve",
            "/docs/api",
            "/data-sources",
            "/solutions/550e8400-e29b-41d4-a716-446655440000",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "refreshing {path}");
            assert_eq!(
                &to_bytes(response.into_body(), usize::MAX).await.unwrap()[..],
                b"seiza frontend"
            );
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/not-a-real-page")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            &to_bytes(response.into_body(), usize::MAX).await.unwrap()[..],
            b"seiza frontend"
        );

        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn solve_cache_policy_tracks_job_mutability_and_etags_revalidate() {
        let root = std::env::temp_dir().join(format!("seiza-api-cache-{}", Uuid::now_v7()));
        let state = AppState::new(test_config(&root)).await.unwrap();
        let object_key = state.new_object_key("cache.fits");
        state
            .store
            .put(&object_key, Bytes::from_static(b"cache image"), None)
            .await
            .unwrap();
        let job = state
            .enqueue_stored(
                Client {
                    id: "cache-test".into(),
                    queue_weight: 1.0,
                },
                object_key,
                "cache.fits".into(),
                None,
                SolveOptions::default(),
            )
            .await
            .unwrap();
        let public_id = public_job_id(&job);

        let queued = get_solve(
            State(state.clone()),
            Path(public_id.clone()),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(queued.headers()[header::CACHE_CONTROL], "no-store");

        let lease = state.repository.claim(None, 60).await.unwrap().unwrap();
        assert!(
            state
                .repository
                .complete(
                    lease.job_id,
                    lease.lease_token,
                    Some(solved_fixture()),
                    None,
                )
                .await
                .unwrap()
        );
        let settled = get_solve(
            State(state.clone()),
            Path(public_id.clone()),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            settled.headers()[header::CACHE_CONTROL],
            "private, max-age=30, stale-while-revalidate=120"
        );
        let etag = settled.headers()[header::ETAG].clone();
        let mut conditional_headers = HeaderMap::new();
        conditional_headers.insert(header::IF_NONE_MATCH, etag.clone());
        let revalidated = get_solve(State(state.clone()), Path(public_id), conditional_headers)
            .await
            .unwrap();
        assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(revalidated.headers()[header::ETAG], etag);
        assert!(
            to_bytes(revalidated.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );

        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn public_solution_locators_use_the_job_uuid() {
        let job_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let legacy_token = Uuid::parse_str("019f5c5d-af6b-7930-b0ca-371b62e32bc0").unwrap();
        let storage_token = Uuid::parse_str("019f5c5d-af6b-7930-b0ca-371b62e32bc1").unwrap();
        let new_key = format!("uploads/public-{job_id}/{storage_token}.fits");
        let legacy_key = format!("uploads/{legacy_token}.fits");
        let locator = format!("42-{job_id}");

        assert_eq!(job_id_from_object_key(&new_key), Some(job_id));
        assert_eq!(job_id_from_object_key(&legacy_key), Some(legacy_token));
        assert_eq!(legacy_public_job_id(&locator), Some((42, job_id)));
        assert_eq!(
            legacy_public_job_id(&format!("42-{storage_token}")),
            Some((42, storage_token))
        );
        assert_eq!(legacy_public_job_id("42"), None);
        assert_eq!(legacy_public_job_id("42-not-a-token"), None);
    }

    #[tokio::test]
    async fn enqueue_wakes_embedded_workers() {
        let root = std::env::temp_dir().join(format!("seiza-api-wakeup-{}", Uuid::now_v7()));
        let state = AppState::new(test_config(&root)).await.unwrap();
        {
            let wakeup_signal = Arc::clone(&state.embedded_worker_wakeup);
            let wakeup = wakeup_signal.notified();
            tokio::pin!(wakeup);
            wakeup.as_mut().enable();

            let job = state
                .enqueue_stored(
                    Client {
                        id: "public".into(),
                        queue_weight: 1.0,
                    },
                    state.new_object_key("wakeup.fits"),
                    "wakeup.fits".into(),
                    Some("application/fits".into()),
                    SolveOptions::default(),
                )
                .await
                .unwrap();

            tokio::time::timeout(Duration::from_secs(1), wakeup.as_mut())
                .await
                .expect("enqueue did not wake embedded workers");
            assert_eq!(job.status, JobStatus::Queued);
        }

        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn solve_time_reports_the_completed_worker_attempt() {
        let started = "2026-07-15T10:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();
        let completed = "2026-07-15T10:00:02.345Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();

        assert_eq!(solve_time_ms(Some(started), Some(completed)), Some(2_345));
        assert_eq!(solve_time_ms(Some(started), None), None);
        assert_eq!(solve_time_ms(Some(completed), Some(started)), Some(0));
    }

    #[tokio::test]
    async fn health_reports_server_and_solver_versions() {
        let root = std::env::temp_dir().join(format!("seiza-api-health-{}", Uuid::now_v7()));
        let state = AppState::new(test_config(&root)).await.unwrap();

        let Json(health) = get_health(State(state)).await.unwrap();
        assert_eq!(
            health["versions"]["seiza_server"],
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(health["versions"]["seiza"], env!("SEIZA_DEP_VERSION"));

        std::fs::remove_dir_all(root).unwrap();
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

    #[tokio::test]
    async fn resumable_fits_headers_promote_a_hinted_solve_before_queueing() {
        let root = std::env::temp_dir().join(format!("seiza-api-fits-hint-{}", Uuid::now_v7()));
        let mut config = test_config(&root);
        config.max_upload_bytes = 4_096;
        let state = AppState::new(config).await.unwrap();
        let mut fits = vec![b' '; 2_880];
        for (index, card) in [
            "SIMPLE  =                    T",
            "RA      =                202.5",
            "DEC     =                 47.2",
            "PIXSCALE=                 1.25",
            "END",
        ]
        .into_iter()
        .enumerate()
        {
            fits[index * 80..index * 80 + card.len()].copy_from_slice(card.as_bytes());
        }
        let metadata = [("filename", "hinted.fits"), ("options", "{}")]
            .map(|(key, value)| {
                format!(
                    "{key} {}",
                    base64::engine::general_purpose::STANDARD.encode(value)
                )
            })
            .join(",");
        let mut headers = tus_request_headers();
        headers.insert("upload-length", HeaderValue::from_static("2880"));
        headers.insert("upload-metadata", HeaderValue::from_str(&metadata).unwrap());
        let response = create_resumable_upload(State(state.clone()), headers)
            .await
            .unwrap();
        let upload_id = response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned();

        patch_resumable_upload(
            State(state.clone()),
            Path(upload_id.clone()),
            chunk_headers(0),
            Bytes::from(fits),
        )
        .await
        .unwrap();
        let Json(result) =
            get_resumable_upload_result(State(state.clone()), Path(upload_id), HeaderMap::new())
                .await
                .unwrap();

        assert_eq!(result.options.center_ra_deg, Some(202.5));
        assert_eq!(result.options.center_dec_deg, Some(47.2));
        assert_eq!(result.options.scale_arcsec_per_pixel, Some(1.25));
        assert_eq!(
            result.options.hint_source,
            Some(crate::models::SolveHintSource::FitsHeader)
        );
        assert_eq!(result.options.hint_keywords, ["RA", "DEC", "PIXSCALE"]);

        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn concatenates_parallel_tus_parts_into_one_solve() {
        let root = std::env::temp_dir().join(format!("seiza-api-concat-{}", Uuid::now_v7()));
        let state = AppState::new(test_config(&root)).await.unwrap();
        let mut part_ids = Vec::new();
        for bytes in [Bytes::from_static(b"abcd"), Bytes::from_static(b"efgh")] {
            let mut headers = tus_request_headers();
            headers.insert("upload-length", HeaderValue::from_static("4"));
            headers.insert("upload-concat", HeaderValue::from_static("partial"));
            let response = create_resumable_upload(State(state.clone()), headers)
                .await
                .unwrap();
            let id = response.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_owned();
            patch_resumable_upload(
                State(state.clone()),
                Path(id.clone()),
                chunk_headers(0),
                bytes,
            )
            .await
            .unwrap();
            let partial = ResumableUpload::load(&state.store, &state.config.s3_prefix, &id)
                .await
                .unwrap();
            assert!(partial.partial);
            assert!(partial.job_id.is_none());
            part_ids.push(id);
        }

        let metadata = [("filename", "parallel.fits"), ("options", "{}")]
            .map(|(key, value)| {
                format!(
                    "{key} {}",
                    base64::engine::general_purpose::STANDARD.encode(value)
                )
            })
            .join(",");
        let mut final_headers = tus_request_headers();
        final_headers.insert(
            "upload-concat",
            HeaderValue::from_str(&format!(
                "final;/api/v1/uploads/{} /api/v1/uploads/{}",
                part_ids[0], part_ids[1]
            ))
            .unwrap(),
        );
        final_headers.insert("upload-metadata", HeaderValue::from_str(&metadata).unwrap());
        let response = create_resumable_upload(State(state.clone()), final_headers)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.headers()["upload-offset"], "8");
        let final_id = response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned();
        let Json(result) =
            get_resumable_upload_result(State(state.clone()), Path(final_id), HeaderMap::new())
                .await
                .unwrap();
        assert_eq!(result.original_filename, "parallel.fits");
        assert_eq!(state.repository.queue_depth().await.unwrap(), 1);
        let job = state
            .repository
            .get(Uuid::parse_str(&result.id).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            state.store.get(&job.object_key).await.unwrap(),
            b"abcdefgh"[..]
        );
        for id in part_ids {
            assert!(matches!(
                ResumableUpload::load(&state.store, &state.config.s3_prefix, &id).await,
                Err(ResumableUploadError::NotFound)
            ));
        }

        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_solve_creates_a_new_job_with_hints_without_a_new_upload() {
        let root = std::env::temp_dir().join(format!("seiza-api-retry-{}", Uuid::now_v7()));
        let state = AppState::new(test_config(&root)).await.unwrap();
        let capture_time = "2026-07-14T02:30:00Z";
        let metadata = [
            ("filename", "retry.fits"),
            (
                "options",
                &format!(r#"{{"capture_time":"{capture_time}"}}"#),
            ),
        ]
        .map(|(key, value)| {
            format!(
                "{key} {}",
                base64::engine::general_purpose::STANDARD.encode(value)
            )
        })
        .join(",");
        let mut create_headers = tus_request_headers();
        create_headers.insert("upload-length", HeaderValue::from_static("4"));
        create_headers.insert("upload-metadata", HeaderValue::from_str(&metadata).unwrap());
        let response = create_resumable_upload(State(state.clone()), create_headers)
            .await
            .unwrap();
        let upload_id = response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned();
        patch_resumable_upload(
            State(state.clone()),
            Path(upload_id.clone()),
            chunk_headers(0),
            Bytes::from_static(b"data"),
        )
        .await
        .unwrap();
        let Json(job) =
            get_resumable_upload_result(State(state.clone()), Path(upload_id), HeaderMap::new())
                .await
                .unwrap();
        let lease = state.repository.claim(None, 60).await.unwrap().unwrap();
        assert!(
            state
                .repository
                .complete(
                    lease.job_id,
                    lease.lease_token,
                    None,
                    Some("no match".into())
                )
                .await
                .unwrap()
        );
        let object_key = state
            .repository
            .get(lease.job_id)
            .await
            .unwrap()
            .unwrap()
            .object_key;

        let hints = SolveOptions {
            center_ra_deg: Some(210.802),
            center_dec_deg: Some(54.349),
            scale_arcsec_per_pixel: Some(1.24),
            ..SolveOptions::default()
        };
        let (status, Json(retried)) = {
            let wakeup_signal = Arc::clone(&state.embedded_worker_wakeup);
            let wakeup = wakeup_signal.notified();
            tokio::pin!(wakeup);
            wakeup.as_mut().enable();
            let response = resolve_solve(
                State(state.clone()),
                Path(job.id.clone()),
                HeaderMap::new(),
                Json(hints),
            )
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(1), wakeup.as_mut())
                .await
                .expect("retry did not wake embedded workers");
            response
        };
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_ne!(retried.id, job.id);
        assert_eq!(retried.status, JobStatus::Queued);
        assert_eq!(retried.options.center_ra_deg, Some(210.802));
        assert_eq!(
            retried.options.capture_time.unwrap(),
            chrono::DateTime::parse_from_rfc3339(capture_time)
                .unwrap()
                .with_timezone(&Utc)
        );
        assert_eq!(state.repository.queue_depth().await.unwrap(), 1);
        let original = state.repository.get(lease.job_id).await.unwrap().unwrap();
        assert_eq!(original.status, JobStatus::Failed);
        assert_eq!(original.object_key, object_key);
        let copied = state
            .repository
            .get(Uuid::parse_str(&retried.id).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(copied.object_key, object_key);
        assert_eq!(
            state.store.get(&copied.object_key).await.unwrap(),
            b"data"[..]
        );

        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn completed_solve_donation_preserves_the_image_and_license_grant() {
        let root = std::env::temp_dir().join(format!("seiza-api-donation-{}", Uuid::now_v7()));
        let state = AppState::new(test_config(&root)).await.unwrap();
        let object_key = state.new_object_key("validation.fits");
        state
            .store
            .put(
                &object_key,
                Bytes::from_static(b"validation image"),
                Some("application/fits"),
            )
            .await
            .unwrap();
        let job = state
            .enqueue_stored(
                Client {
                    id: "public".into(),
                    queue_weight: 1.0,
                },
                object_key.clone(),
                "validation.fits".into(),
                Some("application/fits".into()),
                SolveOptions::default(),
            )
            .await
            .unwrap();
        let lease = state.repository.claim(None, 60).await.unwrap().unwrap();
        assert!(
            state
                .repository
                .complete(
                    lease.job_id,
                    lease.lease_token,
                    Some(solved_fixture()),
                    None,
                )
                .await
                .unwrap()
        );
        let public_id = public_job_id(&job);

        let missing_grant = donate_validation_image(
            State(state.clone()),
            Path(public_id.clone()),
            HeaderMap::new(),
            Json(ValidationDonationRequest {
                comment: None,
                solve_is_invalid: false,
                license_agreed: false,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(missing_grant.status, StatusCode::BAD_REQUEST);

        let Json(donated) = donate_validation_image(
            State(state.clone()),
            Path(public_id.clone()),
            HeaderMap::new(),
            Json(ValidationDonationRequest {
                comment: Some("Useful example of a sparse field".into()),
                solve_is_invalid: true,
                license_agreed: true,
            }),
        )
        .await
        .unwrap();
        let donation = donated.validation_donation.unwrap();
        assert_eq!(
            donation.comment.as_deref(),
            Some("Useful example of a sparse field")
        );
        assert!(donation.solve_is_invalid);
        assert_eq!(donation.license_version, VALIDATION_LICENSE_VERSION);

        let record = state.repository.get(job.id).await.unwrap().unwrap();
        let record_donation = record.validation_donation.unwrap();
        assert!(record_donation.solve_is_invalid);
        let durable_key = record_donation.object_key;
        assert!(durable_key.starts_with("validation/public-"));
        assert_eq!(
            state.store.get(&durable_key).await.unwrap(),
            b"validation image"[..]
        );
        assert_eq!(
            state
                .store
                .delete_older_than(
                    SystemTime::now() + Duration::from_secs(1),
                    std::slice::from_ref(&state.config.validation_prefix),
                )
                .await
                .unwrap(),
            1
        );
        assert!(!state.store.exists(&object_key).await.unwrap());
        assert!(state.store.exists(&durable_key).await.unwrap());

        let response = get_solve(State(state.clone()), Path(public_id), HeaderMap::new())
            .await
            .unwrap();
        let refreshed: JobResponse =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(refreshed.input_available);
        assert!(refreshed.preview_url.is_some());

        let (status, Json(retried)) = resolve_solve(
            State(state.clone()),
            Path(refreshed.id),
            HeaderMap::new(),
            Json(SolveOptions::default()),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_ne!(retried.id, job.id.to_string());
        assert_eq!(retried.status, JobStatus::Queued);
        let lease = state.repository.claim(None, 60).await.unwrap().unwrap();
        let copied_key = state
            .repository
            .input_key(lease.job_id, lease.lease_token)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(copied_key, durable_key);
        assert_eq!(
            state.store.get(&copied_key).await.unwrap(),
            b"validation image"[..]
        );

        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn v4_catalog_api_supports_spatial_alias_and_detail_queries() {
        let root = std::env::temp_dir().join(format!("seiza-api-catalog-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let catalog_path = root.join("objects.bin");
        ObjectCatalog::new(vec![SkyObject {
            kind: ObjectKind::Galaxy,
            ra: 10.684793,
            dec: 41.269065,
            mag: Some(3.44),
            major_arcmin: Some(177.83),
            minor_arcmin: Some(69.66),
            position_angle_deg: Some(35.0),
            name: "NGC 224".into(),
            common_name: "Andromeda Galaxy".into(),
            metadata: ObjectMetadata {
                id: "openngc:NGC224".into(),
                source: "OpenNGC".into(),
                aliases: vec!["M 31".into(), "UGC 00454".into()],
                parent_ids: vec!["curated:local-group".into()],
                alternate_ids: vec!["messier:M31".into()],
                alternate_sources: vec!["Messier catalog".into()],
            },
        }])
        .write_to(&catalog_path)
        .unwrap();

        let mut config = test_config(&root);
        config.object_catalog_path = Some(catalog_path);
        let state = AppState::new(config).await.unwrap();
        let Json(objects) = get_catalog_objects(
            State(state.clone()),
            Query(CatalogObjectsQuery {
                ra: 10.684793,
                dec: 41.269065,
                radius: 1.0,
                kinds: Some("galaxy".into()),
                max_mag: None,
                min_major_arcmin: None,
                common_name_only: false,
                include_extent_overlaps: true,
                limit: 10,
                sort: "prominence".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(objects.catalog_objects, 1);
        assert_eq!(objects.returned, 1);
        assert_eq!(objects.objects[0].object.id, "openngc:NGC224");
        assert_eq!(objects.objects[0].object.aliases[0], "M 31");

        let Json(matches) = search_catalog_objects(
            State(state.clone()),
            Query(CatalogObjectSearchQuery {
                q: "andro".into(),
                prefix: true,
                limit: 10,
            }),
        )
        .await
        .unwrap();
        assert_eq!(matches.returned, 1);
        assert_eq!(matches.matches[0].matched_name, "Andromeda Galaxy");
        assert_eq!(matches.matches[0].object.source, "OpenNGC");

        let Json(details) =
            get_catalog_object_details(State(state.clone()), Path("openngc:NGC224".into()))
                .await
                .unwrap();
        assert_eq!(details.format_version, 4);
        assert!(details.capabilities.source_records);
        assert!(details.capabilities.selections);
        assert!(details.capabilities.ellipses);
        assert_eq!(details.object.id, "openngc:NGC224");
        assert_eq!(details.details.source_records[0].source, "OpenNGC");

        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn stellar_identifier_api_resolves_tycho_and_name_prefixes() {
        let root = std::env::temp_dir().join(format!("seiza-api-star-ids-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let catalog_path = root.join("stars-lite-tycho2.ids.bin");
        let mut builder = StarIdentifierCatalogBuilder::new(2025.5, "test identifiers");
        builder
            .add(
                StarIdentifier::Tycho2 {
                    region: 5949,
                    number: 2777,
                    component: 1,
                },
                291.366,
                42.784,
                7.1,
            )
            .unwrap();
        builder
            .add_name(
                StarNameCatalog::GeneralCatalogOfVariableStars,
                StarNameKind::VariableStar,
                "RR Lyr",
                "gcvs:RR-Lyr",
                "RRAB",
                291.366,
                42.784,
                Some(7.1),
            )
            .unwrap();
        builder.write_to(&catalog_path).unwrap();

        let mut config = test_config(&root);
        config.star_identifier_catalog_path = Some(catalog_path);
        let state = AppState::new(config).await.unwrap();
        let Json(tycho) = search_star_identifiers(
            State(state.clone()),
            Query(StarIdentifierSearchQuery {
                q: "TYC 5949-2777-1".into(),
                prefix: false,
                limit: 10,
            }),
        )
        .await
        .unwrap();
        assert_eq!(tycho.matches[0].stable_id, "tycho2:5949-2777-1");
        assert_eq!(tycho.attribution, "test identifiers");

        let Json(names) = search_star_identifiers(
            State(state.clone()),
            Query(StarIdentifierSearchQuery {
                q: "RR L".into(),
                prefix: true,
                limit: 10,
            }),
        )
        .await
        .unwrap();
        assert_eq!(names.returned, 1);
        assert_eq!(names.matches[0].designation, "RR Lyr");

        drop(state);
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

    #[tokio::test]
    async fn email_sign_in_issues_a_csrf_bound_multi_session_cookie() {
        let root = std::env::temp_dir().join(format!("seiza-account-api-{}", Uuid::now_v7()));
        let mut accounts_config = test_config(&root);
        accounts_config.auth_mode = AuthMode::Accounts;
        accounts_config.public_base_url = Some(Url::parse("https://solve.example.com").unwrap());

        let mut state = AppState::new(test_config(&root)).await.unwrap();
        let identity = Arc::new(
            SqlxIdentityRepository::connect("sqlite::memory:")
                .await
                .unwrap(),
        );
        let sender = Arc::new(CapturingEmailSender::default());
        state.config = Arc::new(accounts_config);
        state.identity = Some(identity.clone());
        state.auth = Some(Arc::new(AuthService::new(
            identity,
            sender.clone(),
            Url::parse("https://solve.example.com").unwrap(),
            vec![42; 32],
        )));

        let start_response = start_email_sign_in(
            State(state.clone()),
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 4000))),
            HeaderMap::new(),
            Json(EmailStartRequest {
                email: "Astronomer@Example.com".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(start_response.status(), StatusCode::ACCEPTED);
        let message = sender.0.lock().await[0].clone();
        let token = Url::parse(&message.link)
            .unwrap()
            .query_pairs()
            .find_map(|(key, value)| (key == "token").then(|| value.into_owned()))
            .unwrap();

        let response = complete_email_sign_in(
            State(state.clone()),
            Json(EmailCompleteRequest {
                link_token: Some(token),
                email: None,
                challenge_id: None,
                code: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| Cookie::parse(value.to_str().unwrap()).unwrap().into_owned())
            .collect::<Vec<_>>();
        let session = cookies
            .iter()
            .find(|cookie| cookie.name() == "__Host-seiza_session")
            .unwrap();
        assert!(session.http_only().unwrap_or(false));
        assert!(session.secure().unwrap_or(false));
        let csrf = cookies
            .iter()
            .find(|cookie| cookie.name() == "__Host-seiza_csrf")
            .unwrap();
        assert!(!csrf.http_only().unwrap_or(false));
        let cookie_header = cookies
            .iter()
            .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
            .collect::<Vec<_>>()
            .join("; ");

        let mut read_headers = HeaderMap::new();
        read_headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&cookie_header).unwrap(),
        );
        assert_eq!(
            get_account(State(state.clone()), read_headers.clone())
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            client_from_headers(&state, &read_headers, None, false)
                .await
                .unwrap()
                .id,
            format!(
                "account:{}",
                state
                    .identity
                    .as_ref()
                    .unwrap()
                    .account_by_email_lookup("astronomer@example.com")
                    .await
                    .unwrap()
                    .unwrap()
                    .id
            )
        );

        let missing_origin = client_from_headers(&state, &read_headers, None, true)
            .await
            .unwrap_err();
        assert_eq!(missing_origin.status, StatusCode::UNAUTHORIZED);

        let mut mutation_headers = read_headers;
        mutation_headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://solve.example.com"),
        );
        mutation_headers.insert("x-csrf-token", HeaderValue::from_str(csrf.value()).unwrap());
        assert!(
            client_from_headers(&state, &mutation_headers, None, true)
                .await
                .is_ok()
        );
        let browser = authenticated_browser(&state, &mutation_headers)
            .await
            .unwrap();
        let created_key = auth_service(&state)
            .unwrap()
            .create_api_key(
                &browser,
                "API test",
                &[
                    crate::auth::SCOPE_SOLVE_READ.into(),
                    crate::auth::SCOPE_SOLVE_SUBMIT.into(),
                ],
            )
            .await
            .unwrap();
        let mut api_headers = HeaderMap::new();
        api_headers.insert(
            "x-api-key",
            HeaderValue::from_str(&created_key.token).unwrap(),
        );
        assert_eq!(
            client_from_headers(&state, &api_headers, None, true)
                .await
                .unwrap()
                .id,
            format!("account:{}", browser.account.id)
        );
        let Json(astrometry_login) = astrometry_login(
            State(state.clone()),
            Form(RequestJsonForm {
                request_json: serde_json::to_string(&json!({
                    "apikey": created_key.token,
                }))
                .unwrap(),
            }),
        )
        .await
        .unwrap();
        let astrometry_session = astrometry_login["session"].as_str().unwrap();
        assert_eq!(
            client_from_headers(&state, &HeaderMap::new(), Some(astrometry_session), true)
                .await
                .unwrap()
                .id,
            format!("account:{}", browser.account.id)
        );
        let (_, astrometry_session_id, _) =
            crate::auth::parse_session_token(astrometry_session).unwrap();
        assert_eq!(
            revoke_account_session(
                State(state.clone()),
                Path(astrometry_session_id),
                mutation_headers.clone(),
            )
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
        assert!(
            client_from_headers(&state, &HeaderMap::new(), Some(astrometry_session), true)
                .await
                .is_err()
        );
        let registration =
            start_passkey_registration(State(state.clone()), mutation_headers.clone())
                .await
                .unwrap();
        assert_eq!(registration.status(), StatusCode::OK);
        let registration: Value = serde_json::from_slice(
            &to_bytes(registration.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(registration["challenge_id"].is_string());
        assert!(registration["options"]["publicKey"].is_object());
        let authentication = start_passkey_sign_in(
            State(state.clone()),
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 4000))),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(authentication.status(), StatusCode::OK);
        let authentication: Value = serde_json::from_slice(
            &to_bytes(authentication.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(authentication["options"]["mediation"], "conditional");
        assert_eq!(
            logout(State(state.clone()), mutation_headers)
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            get_account(State(state.clone()), HeaderMap::new())
                .await
                .unwrap_err()
                .status,
            StatusCode::UNAUTHORIZED
        );

        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn public_client_ids_normalize_ip_addresses_for_queue_fairness() {
        assert_eq!(public_client_id("192.0.2.42"), "public:192.0.2.42");
        assert_eq!(
            public_client_id("2001:db8:1234:5678:90ab:cdef:1234:5678"),
            "public:2001:db8:1234:5678::"
        );
        assert_eq!(public_client_id("not an ip"), "public:anonymous");
    }

    #[tokio::test]
    async fn only_configured_api_keys_receive_priority_weight() {
        let root = std::env::temp_dir().join(format!("seiza-priority-key-{}", Uuid::now_v7()));
        let mut config = test_config(&root);
        config.priority_api_keys =
            crate::config::PriorityApiKeys::parse(Some("operator-secret".into()));
        let state = AppState::new(config).await.unwrap();

        let mut priority_headers = HeaderMap::new();
        priority_headers.insert("x-api-key", HeaderValue::from_static("operator-secret"));
        let priority = client_from_headers(&state, &priority_headers, None, false)
            .await
            .unwrap();
        assert_eq!(priority.queue_weight, 2.0);

        let mut ordinary_headers = HeaderMap::new();
        ordinary_headers.insert("x-api-key", HeaderValue::from_static("unconfigured-key"));
        let ordinary = client_from_headers(&state, &ordinary_headers, None, false)
            .await
            .unwrap();
        assert_eq!(ordinary.queue_weight, 1.0);

        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn test_config(root: &std::path::Path) -> Config {
        Config {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            frontend_dir: root.join("frontend"),
            data_dir: root.to_owned(),
            catalog_path: None,
            blind_index_path: None,
            object_catalog_path: None,
            star_identifier_catalog_path: None,
            transient_catalog_path: None,
            minor_body_catalog_path: None,
            job_backend: JobBackend::Sqlx,
            sql_database_url: format!("sqlite://{}?mode=rwc", root.join("jobs.sqlite3").display()),
            dynamodb_table: None,
            identity_backend: JobBackend::Sqlx,
            identity_sql_database_url: format!(
                "sqlite://{}?mode=rwc",
                root.join("identity.sqlite3").display()
            ),
            identity_dynamodb_table: None,
            queue_transport: QueueDelivery::Local,
            sqs_queue_url: None,
            sqs_priority_queue_url: None,
            sqs_priority_weight: 2,
            priority_api_keys: Default::default(),
            embedded_workers: false,
            worker_token: None,
            lease_seconds: 900,
            worker_count: 1,
            max_upload_bytes: 1_024,
            upload_retention_seconds: 86_400,
            upload_cleanup_interval_seconds: 3_600,
            rate_limit_per_minute: 60.0,
            rate_limit_burst: 10.0,
            trusted_proxy_hops: 0,
            auth_mode: AuthMode::Public,
            public_base_url: None,
            auth_code_pepper_file: None,
            email_provider: None,
            email_from: None,
            ses_from_identity_arn: None,
            ses_role_arn: None,
            ses_role_external_id_file: None,
            smtp_host: None,
            smtp_port: None,
            smtp_username: None,
            smtp_password_file: None,
            smtp_tls: crate::config::SmtpTls::StartTls,
            smtp_timeout_seconds: 30,
            storage_backend: StorageBackend::Local,
            s3_bucket: None,
            s3_prefix: "uploads".into(),
            validation_prefix: "validation".into(),
        }
    }
}
