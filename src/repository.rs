use crate::{
    config::{Config, JobBackend},
    models::{
        AstrometryId, JobHistoryRecord, JobId, JobLease, JobRecord, JobStatus, LegacyJobId,
        SolutionResponse, ValidationDonation, astrometry_id_for_job,
    },
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::{AnyConnection, AnyPool, Row, any::AnyPoolOptions};
use std::{cmp::Ordering, sync::Arc};
use uuid::Uuid;

/// The durable job, priority, lease, and outbox boundary. Implementations
/// must make `claim` exclusive: a worker may only write results for the lease
/// token it received from this call.
#[async_trait]
pub trait JobRepository: Send + Sync {
    async fn enqueue(&self, job: JobRecord) -> Result<JobRecord>;
    async fn get(&self, job_id: JobId) -> Result<Option<JobRecord>>;
    async fn get_by_legacy_id(&self, legacy_id: LegacyJobId) -> Result<Option<JobRecord>>;
    async fn get_by_astrometry_id(&self, astrometry_id: AstrometryId) -> Result<Option<JobRecord>>;
    async fn find_by_object_key(&self, object_key: &str) -> Result<Option<JobRecord>>;
    async fn list_by_owner(&self, owner: &str, limit: usize) -> Result<Vec<JobHistoryRecord>>;
    async fn queue_depth(&self) -> Result<usize>;
    async fn claim(
        &self,
        requested_job_id: Option<JobId>,
        lease_seconds: u64,
    ) -> Result<Option<JobLease>>;
    async fn heartbeat(
        &self,
        job_id: JobId,
        lease_token: String,
        lease_seconds: u64,
    ) -> Result<bool>;
    async fn input_key(&self, job_id: JobId, lease_token: String) -> Result<Option<String>>;
    async fn complete(
        &self,
        job_id: JobId,
        lease_token: String,
        solution: Option<SolutionResponse>,
        error: Option<String>,
    ) -> Result<bool>;
    async fn donate_validation(&self, job_id: JobId, donation: ValidationDonation) -> Result<bool>;
    async fn pending_notifications(&self, limit: usize) -> Result<Vec<JobId>>;
    async fn mark_notification_delivered(&self, job_id: JobId) -> Result<()>;
}

/// SQLx implementation for `sqlite://...` and `postgres://...` URLs. The
/// schema intentionally uses the SQL subset shared by both engines, and all
/// lease updates are conditional so independent worker processes are safe.
#[derive(Clone)]
pub struct SqlxJobRepository {
    pool: AnyPool,
    dialect: SqlDialect,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqlDialect {
    Sqlite,
    Postgres,
}

impl SqlxJobRepository {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let repository = Self::open(database_url, true).await?;
        repository.migrate().await?;
        Ok(repository)
    }

    #[cfg(feature = "aws")]
    pub(crate) async fn connect_for_migration(
        database_url: &str,
        initialize_uuid_schema: bool,
    ) -> Result<Self> {
        let repository = Self::open(database_url, initialize_uuid_schema).await?;
        if initialize_uuid_schema {
            repository.migrate().await?;
        }
        Ok(repository)
    }

    async fn open(database_url: &str, configure_sqlite_wal: bool) -> Result<Self> {
        sqlx::any::install_default_drivers();
        let dialect = if database_url.starts_with("sqlite:") {
            SqlDialect::Sqlite
        } else if database_url.starts_with("postgres:") || database_url.starts_with("postgresql:") {
            SqlDialect::Postgres
        } else {
            bail!("SEIZA_SQL_DATABASE_URL must use a sqlite:// or postgres:// URL");
        };
        let pool = AnyPoolOptions::new()
            .max_connections(if dialect == SqlDialect::Sqlite { 1 } else { 12 })
            .connect(database_url)
            .await
            .with_context(|| format!("connecting SQLx job repository at {database_url}"))?;
        if dialect == SqlDialect::Sqlite && configure_sqlite_wal {
            sqlx::query("PRAGMA journal_mode = WAL")
                .execute(&pool)
                .await
                .context("enabling SQLite WAL mode")?;
        }
        Ok(Self { pool, dialect })
    }

    #[cfg(feature = "aws")]
    pub(crate) fn pool(&self) -> &AnyPool {
        &self.pool
    }

    #[cfg(feature = "aws")]
    pub(crate) fn dialect(&self) -> SqlDialect {
        self.dialect
    }

    async fn migrate(&self) -> Result<()> {
        for statement in [
            "CREATE TABLE IF NOT EXISTS jobs_v2 (id TEXT PRIMARY KEY, astrometry_id BIGINT NOT NULL UNIQUE, legacy_id BIGINT UNIQUE, owner TEXT NOT NULL, queue_weight DOUBLE PRECISION NOT NULL, object_key TEXT NOT NULL, original_filename TEXT NOT NULL, content_type TEXT, options_json TEXT NOT NULL, status TEXT NOT NULL, created_at TEXT NOT NULL, started_at TEXT, completed_at TEXT, solution_json TEXT, error TEXT, lease_token TEXT, lease_expires_at TEXT, attempts BIGINT NOT NULL DEFAULT 0)",
            "CREATE INDEX IF NOT EXISTS jobs_v2_status_created_idx ON jobs_v2(status, created_at)",
            "CREATE INDEX IF NOT EXISTS jobs_v2_owner_created_idx ON jobs_v2(owner, created_at)",
            "CREATE INDEX IF NOT EXISTS jobs_v2_lease_idx ON jobs_v2(status, lease_expires_at)",
            "CREATE UNIQUE INDEX IF NOT EXISTS jobs_v2_object_key_idx ON jobs_v2(object_key)",
            "CREATE TABLE IF NOT EXISTS validation_donations_v2 (job_id TEXT PRIMARY KEY, object_key TEXT NOT NULL UNIQUE, comment TEXT, solve_is_invalid BIGINT NOT NULL DEFAULT 0, license_version TEXT NOT NULL, donated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS client_service (owner TEXT PRIMARY KEY, last_served_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS queue_outbox_v2 (job_id TEXT PRIMARY KEY, delivered_at TEXT)",
        ] {
            sqlx::query(statement).execute(&self.pool).await?;
        }
        self.migrate_legacy_jobs().await?;
        Ok(())
    }

    async fn table_exists(&self, table: &str) -> Result<bool> {
        let present = match self.dialect {
            SqlDialect::Sqlite => sqlx::query(
                "SELECT COUNT(*) AS count FROM sqlite_master WHERE type = 'table' AND name = $1",
            )
            .bind(table)
            .fetch_one(&self.pool)
            .await?
            .try_get::<i64, _>("count")?
                > 0,
            SqlDialect::Postgres => sqlx::query(
                "SELECT COUNT(*) AS count FROM information_schema.tables WHERE table_schema = current_schema() AND table_name = $1",
            )
            .bind(table)
            .fetch_one(&self.pool)
            .await?
            .try_get::<i64, _>("count")?
                > 0,
        };
        Ok(present)
    }

    async fn migrate_legacy_jobs(&self) -> Result<()> {
        if !self.table_exists("jobs").await? {
            return Ok(());
        }

        for row in sqlx::query("SELECT * FROM jobs")
            .fetch_all(&self.pool)
            .await?
        {
            let legacy_id = row.try_get::<i64, _>("id")?;
            let object_key = row.try_get::<String, _>("object_key")?;
            let job_id = job_id_from_object_key(&object_key).unwrap_or_else(Uuid::new_v4);
            sqlx::query("INSERT INTO jobs_v2 (id, astrometry_id, legacy_id, owner, queue_weight, object_key, original_filename, content_type, options_json, status, created_at, started_at, completed_at, solution_json, error, lease_token, lease_expires_at, attempts) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) ON CONFLICT(legacy_id) DO NOTHING")
                .bind(job_id.to_string())
                .bind(legacy_id)
                .bind(legacy_id)
                .bind(row.try_get::<String, _>("owner")?)
                .bind(row.try_get::<f64, _>("queue_weight")?)
                .bind(object_key)
                .bind(row.try_get::<String, _>("original_filename")?)
                .bind(row.try_get::<Option<String>, _>("content_type")?)
                .bind(row.try_get::<String, _>("options_json")?)
                .bind(row.try_get::<String, _>("status")?)
                .bind(row.try_get::<String, _>("created_at")?)
                .bind(row.try_get::<Option<String>, _>("started_at")?)
                .bind(row.try_get::<Option<String>, _>("completed_at")?)
                .bind(row.try_get::<Option<String>, _>("solution_json")?)
                .bind(row.try_get::<Option<String>, _>("error")?)
                .bind(row.try_get::<Option<String>, _>("lease_token")?)
                .bind(row.try_get::<Option<String>, _>("lease_expires_at")?)
                .bind(row.try_get::<i64, _>("attempts")?)
                .execute(&self.pool)
                .await?;
        }

        if self.table_exists("validation_donations").await? {
            for row in sqlx::query("SELECT * FROM validation_donations")
                .fetch_all(&self.pool)
                .await?
            {
                let legacy_id = row.try_get::<i64, _>("job_id")?;
                let Some(job_id) = self.uuid_for_legacy_id(legacy_id).await? else {
                    continue;
                };
                sqlx::query("INSERT INTO validation_donations_v2 (job_id, object_key, comment, solve_is_invalid, license_version, donated_at) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT(job_id) DO NOTHING")
                    .bind(job_id.to_string())
                    .bind(row.try_get::<String, _>("object_key")?)
                    .bind(row.try_get::<Option<String>, _>("comment")?)
                    .bind(row.try_get::<i64, _>("solve_is_invalid")?)
                    .bind(row.try_get::<String, _>("license_version")?)
                    .bind(row.try_get::<String, _>("donated_at")?)
                    .execute(&self.pool)
                    .await?;
            }
        }

        if self.table_exists("queue_outbox").await? {
            for row in sqlx::query("SELECT queue_outbox.job_id, queue_outbox.delivered_at, jobs.status FROM queue_outbox JOIN jobs ON jobs.id = queue_outbox.job_id")
                .fetch_all(&self.pool)
                .await?
            {
                let legacy_id = row.try_get::<i64, _>("job_id")?;
                let Some(job_id) = self.uuid_for_legacy_id(legacy_id).await? else {
                    continue;
                };
                let delivered_at = if row.try_get::<String, _>("status")? == "queued" {
                    None
                } else {
                    row.try_get::<Option<String>, _>("delivered_at")?
                };
                sqlx::query("INSERT INTO queue_outbox_v2 (job_id, delivered_at) VALUES ($1, $2) ON CONFLICT(job_id) DO NOTHING")
                    .bind(job_id.to_string())
                    .bind(delivered_at)
                    .execute(&self.pool)
                    .await?;
            }
        }
        Ok(())
    }

    async fn uuid_for_legacy_id(&self, legacy_id: i64) -> Result<Option<JobId>> {
        sqlx::query("SELECT id FROM jobs_v2 WHERE legacy_id = $1")
            .bind(legacy_id)
            .fetch_optional(&self.pool)
            .await?
            .map(|row| decode_job_id(&row.try_get::<String, _>("id")?))
            .transpose()
    }

    async fn validation_donation(&self, job_id: JobId) -> Result<Option<ValidationDonation>> {
        sqlx::query(
            "SELECT object_key, comment, solve_is_invalid, license_version, donated_at FROM validation_donations_v2 WHERE job_id = $1",
        )
        .bind(job_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .map(|row| {
            Ok(ValidationDonation {
                object_key: row.try_get("object_key")?,
                comment: row.try_get("comment")?,
                solve_is_invalid: row.try_get::<i64, _>("solve_is_invalid")? != 0,
                license_version: row.try_get("license_version")?,
                donated_at: decode_time(&row.try_get::<String, _>("donated_at")?)?,
            })
        })
        .transpose()
    }

    async fn reclaim_expired(
        &self,
        connection: &mut AnyConnection,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let expired =
            sqlx::query("SELECT id FROM jobs_v2 WHERE status = $1 AND lease_expires_at <= $2")
                .bind("solving")
                .bind(encode_time(now))
                .fetch_all(&mut *connection)
                .await?;
        for row in expired {
            let id = decode_job_id(&row.try_get::<String, _>("id")?)?;
            sqlx::query("UPDATE jobs_v2 SET status = 'queued', lease_token = NULL, lease_expires_at = NULL WHERE id = $1 AND status = 'solving'")
                .bind(id.to_string())
                .execute(&mut *connection)
                .await?;
            sqlx::query("INSERT INTO queue_outbox_v2 (job_id, delivered_at) VALUES ($1, NULL) ON CONFLICT(job_id) DO UPDATE SET delivered_at = NULL")
                .bind(id.to_string())
                .execute(&mut *connection)
                .await?;
        }
        Ok(())
    }

    async fn last_served(
        &self,
        connection: &mut AnyConnection,
        owner: &str,
    ) -> Result<Option<DateTime<Utc>>> {
        let value = sqlx::query("SELECT last_served_at FROM client_service WHERE owner = $1")
            .bind(owner)
            .fetch_optional(&mut *connection)
            .await?
            .map(|row| row.try_get::<String, _>("last_served_at"))
            .transpose()?;
        value.map(|value| decode_time(&value)).transpose()
    }

    async fn select_candidate(
        &self,
        connection: &mut AnyConnection,
        requested_job_id: Option<JobId>,
        now: DateTime<Utc>,
    ) -> Result<Option<JobRecord>> {
        if let Some(job_id) = requested_job_id {
            return sqlx::query("SELECT * FROM jobs_v2 WHERE id = $1 AND status = 'queued'")
                .bind(job_id.to_string())
                .fetch_optional(&mut *connection)
                .await?
                .map(record_from_row)
                .transpose();
        }
        let jobs =
            sqlx::query("SELECT * FROM jobs_v2 WHERE status = 'queued' ORDER BY created_at ASC")
                .fetch_all(&mut *connection)
                .await?;
        let mut best: Option<(f64, JobRecord)> = None;
        for row in jobs {
            let job = record_from_row(row)?;
            let score = match self.last_served(&mut *connection, &job.owner).await? {
                Some(last) => {
                    (now - last).num_milliseconds() as f64 / 1_000.0 * job.queue_weight.max(0.01)
                }
                None => f64::MAX / 4.0,
            };
            if best.as_ref().is_none_or(|(best_score, best_job)| {
                score.total_cmp(best_score) == Ordering::Greater
                    || (score.total_cmp(best_score) == Ordering::Equal
                        && job.created_at < best_job.created_at)
            }) {
                best = Some((score, job));
            }
        }
        Ok(best.map(|(_, job)| job))
    }
}

#[async_trait]
impl JobRepository for SqlxJobRepository {
    async fn enqueue(&self, mut job: JobRecord) -> Result<JobRecord> {
        let mut transaction = self.pool.begin().await?;
        job.astrometry_id = astrometry_id_for_job(job.id);
        job.status = JobStatus::Queued;
        let inserted = sqlx::query("INSERT INTO jobs_v2 (id, astrometry_id, legacy_id, owner, queue_weight, object_key, original_filename, content_type, options_json, status, created_at) VALUES ($1, $2, NULL, $3, $4, $5, $6, $7, $8, 'queued', $9) ON CONFLICT(object_key) DO NOTHING")
            .bind(job.id.to_string())
            .bind(i64::try_from(job.astrometry_id).context("Astrometry ID exceeds SQL BIGINT range")?)
            .bind(&job.owner)
            .bind(job.queue_weight)
            .bind(&job.object_key)
            .bind(&job.original_filename)
            .bind(&job.content_type)
            .bind(serde_json::to_string(&job.options)?)
            .bind(encode_time(job.created_at))
            .execute(&mut *transaction)
            .await?;
        if inserted.rows_affected() == 0 {
            transaction.rollback().await?;
            return self
                .find_by_object_key(&job.object_key)
                .await?
                .context("idempotent SQL enqueue could not find the existing job");
        }
        sqlx::query("INSERT INTO queue_outbox_v2 (job_id, delivered_at) VALUES ($1, NULL)")
            .bind(job.id.to_string())
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(job)
    }

    async fn get(&self, job_id: JobId) -> Result<Option<JobRecord>> {
        let mut job = sqlx::query("SELECT * FROM jobs_v2 WHERE id = $1")
            .bind(job_id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .map(record_from_row)
            .transpose()?;
        if let Some(job) = &mut job {
            job.validation_donation = self.validation_donation(job.id).await?;
        }
        Ok(job)
    }

    async fn get_by_legacy_id(&self, legacy_id: LegacyJobId) -> Result<Option<JobRecord>> {
        let legacy_id =
            i64::try_from(legacy_id).context("legacy job ID exceeds SQL BIGINT range")?;
        let mut job = sqlx::query("SELECT * FROM jobs_v2 WHERE legacy_id = $1")
            .bind(legacy_id)
            .fetch_optional(&self.pool)
            .await?
            .map(record_from_row)
            .transpose()?;
        if let Some(job) = &mut job {
            job.validation_donation = self.validation_donation(job.id).await?;
        }
        Ok(job)
    }

    async fn get_by_astrometry_id(&self, astrometry_id: AstrometryId) -> Result<Option<JobRecord>> {
        let astrometry_id =
            i64::try_from(astrometry_id).context("Astrometry ID exceeds SQL BIGINT range")?;
        let mut job = sqlx::query("SELECT * FROM jobs_v2 WHERE astrometry_id = $1")
            .bind(astrometry_id)
            .fetch_optional(&self.pool)
            .await?
            .map(record_from_row)
            .transpose()?;
        if let Some(job) = &mut job {
            job.validation_donation = self.validation_donation(job.id).await?;
        }
        Ok(job)
    }

    async fn find_by_object_key(&self, object_key: &str) -> Result<Option<JobRecord>> {
        let mut job = sqlx::query("SELECT * FROM jobs_v2 WHERE object_key = $1")
            .bind(object_key)
            .fetch_optional(&self.pool)
            .await?
            .map(record_from_row)
            .transpose()?;
        if let Some(job) = &mut job {
            job.validation_donation = self.validation_donation(job.id).await?;
        }
        Ok(job)
    }

    async fn list_by_owner(&self, owner: &str, limit: usize) -> Result<Vec<JobHistoryRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        sqlx::query("SELECT id, status, original_filename, created_at, started_at, completed_at FROM jobs_v2 WHERE owner = $1 ORDER BY created_at DESC LIMIT $2")
            .bind(owner)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(history_record_from_row)
            .collect()
    }

    async fn queue_depth(&self) -> Result<usize> {
        let row = sqlx::query("SELECT COUNT(*) AS count FROM jobs_v2 WHERE status = 'queued'")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get::<i64, _>("count")? as usize)
    }

    async fn claim(
        &self,
        requested_job_id: Option<JobId>,
        lease_seconds: u64,
    ) -> Result<Option<JobLease>> {
        let mut transaction = if self.dialect == SqlDialect::Sqlite {
            // Acquire SQLite's write reservation before reading the LRU state.
            // This serializes scheduler decisions even across API processes.
            self.pool.begin_with("BEGIN IMMEDIATE").await?
        } else {
            self.pool.begin().await?
        };
        let now = Utc::now();
        if self.dialect == SqlDialect::Postgres {
            // The selection policy is global, so use one transaction-scoped
            // advisory lock to keep concurrent API replicas from evaluating
            // stale client-service timestamps.
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(0x0073_6569_7a61_i64)
                .execute(&mut *transaction)
                .await?;
        }
        self.reclaim_expired(&mut transaction, now).await?;
        let Some(job) = self
            .select_candidate(&mut transaction, requested_job_id, now)
            .await?
        else {
            transaction.commit().await?;
            return Ok(None);
        };
        let lease_token = Uuid::now_v7().to_string();
        let lease_expires_at = now + Duration::seconds(lease_seconds.max(1) as i64);
        let claimed = sqlx::query("UPDATE jobs_v2 SET status = 'solving', started_at = $1, lease_token = $2, lease_expires_at = $3, attempts = attempts + 1 WHERE id = $4 AND status = 'queued'")
            .bind(encode_time(now))
            .bind(&lease_token)
            .bind(encode_time(lease_expires_at))
            .bind(job.id.to_string())
            .execute(&mut *transaction)
            .await?
            .rows_affected() == 1;
        if !claimed {
            transaction.rollback().await?;
            return Ok(None);
        }
        sqlx::query("INSERT INTO client_service (owner, last_served_at) VALUES ($1, $2) ON CONFLICT(owner) DO UPDATE SET last_served_at = EXCLUDED.last_served_at")
            .bind(&job.owner)
            .bind(encode_time(now))
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(Some(JobLease {
            job_id: job.id,
            lease_token,
            lease_expires_at,
            original_filename: job.original_filename,
            options: job.options,
        }))
    }

    async fn heartbeat(
        &self,
        job_id: JobId,
        lease_token: String,
        lease_seconds: u64,
    ) -> Result<bool> {
        let now = Utc::now();
        let result = sqlx::query("UPDATE jobs_v2 SET lease_expires_at = $1 WHERE id = $2 AND status = 'solving' AND lease_token = $3 AND lease_expires_at > $4")
            .bind(encode_time(now + Duration::seconds(lease_seconds.max(1) as i64)))
            .bind(job_id.to_string())
            .bind(lease_token)
            .bind(encode_time(now))
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn input_key(&self, job_id: JobId, lease_token: String) -> Result<Option<String>> {
        sqlx::query("SELECT COALESCE((SELECT object_key FROM validation_donations_v2 WHERE job_id = jobs_v2.id), object_key) AS object_key FROM jobs_v2 WHERE id = $1 AND status = 'solving' AND lease_token = $2 AND lease_expires_at > $3")
            .bind(job_id.to_string())
            .bind(lease_token)
            .bind(encode_time(Utc::now()))
            .fetch_optional(&self.pool)
            .await?
            .map(|row| row.try_get("object_key").map_err(Into::into))
            .transpose()
    }

    async fn complete(
        &self,
        job_id: JobId,
        lease_token: String,
        solution: Option<SolutionResponse>,
        error: Option<String>,
    ) -> Result<bool> {
        if solution.is_none() && error.is_none() {
            bail!("worker completion requires a solution or an error");
        }
        let now = Utc::now();
        let result = sqlx::query("UPDATE jobs_v2 SET status = $1, completed_at = $2, solution_json = $3, error = $4, lease_token = NULL, lease_expires_at = NULL WHERE id = $5 AND status = 'solving' AND lease_token = $6 AND lease_expires_at > $7")
            .bind(if solution.is_some() { "succeeded" } else { "failed" })
            .bind(encode_time(now))
            .bind(solution.map(|value| serde_json::to_string(&value)).transpose()?)
            .bind(error)
            .bind(job_id.to_string())
            .bind(lease_token)
            .bind(encode_time(now))
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn donate_validation(&self, job_id: JobId, donation: ValidationDonation) -> Result<bool> {
        let mut transaction = self.pool.begin().await?;
        let status_query = if self.dialect == SqlDialect::Postgres {
            "SELECT status FROM jobs_v2 WHERE id = $1 FOR UPDATE"
        } else {
            "SELECT status FROM jobs_v2 WHERE id = $1"
        };
        let status = sqlx::query(status_query)
            .bind(job_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?
            .map(|row| row.try_get::<String, _>("status"))
            .transpose()?;
        if !status.is_some_and(|status| status == "succeeded" || status == "failed") {
            transaction.rollback().await?;
            return Ok(false);
        }
        sqlx::query("INSERT INTO validation_donations_v2 (job_id, object_key, comment, solve_is_invalid, license_version, donated_at) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT(job_id) DO UPDATE SET object_key = EXCLUDED.object_key, comment = EXCLUDED.comment, solve_is_invalid = EXCLUDED.solve_is_invalid")
            .bind(job_id.to_string())
            .bind(donation.object_key)
            .bind(donation.comment)
            .bind(i64::from(donation.solve_is_invalid))
            .bind(donation.license_version)
            .bind(encode_time(donation.donated_at))
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(true)
    }

    async fn pending_notifications(&self, limit: usize) -> Result<Vec<JobId>> {
        let mut transaction = self.pool.begin().await?;
        self.reclaim_expired(&mut transaction, Utc::now()).await?;
        let rows = sqlx::query("SELECT queue_outbox_v2.job_id FROM queue_outbox_v2 JOIN jobs_v2 ON jobs_v2.id = queue_outbox_v2.job_id WHERE queue_outbox_v2.delivered_at IS NULL ORDER BY jobs_v2.created_at ASC LIMIT $1")
            .bind(limit as i64)
            .fetch_all(&mut *transaction)
            .await?;
        transaction.commit().await?;
        rows.into_iter()
            .map(|row| decode_job_id(&row.try_get::<String, _>("job_id")?))
            .collect()
    }

    async fn mark_notification_delivered(&self, job_id: JobId) -> Result<()> {
        sqlx::query("UPDATE queue_outbox_v2 SET delivered_at = $1 WHERE job_id = $2")
            .bind(encode_time(Utc::now()))
            .bind(job_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

pub async fn job_repository(config: &Config) -> Result<Arc<dyn JobRepository>> {
    match config.job_backend {
        JobBackend::Sqlx => Ok(Arc::new(
            SqlxJobRepository::connect(&config.sql_database_url).await?,
        )),
        JobBackend::DynamoDb => {
            #[cfg(feature = "aws")]
            {
                Ok(Arc::new(
                    crate::dynamodb_repository::DynamoDbJobRepository::connect(config).await?,
                ))
            }
            #[cfg(not(feature = "aws"))]
            {
                bail!("DynamoDB job backend requires `cargo run --features aws`")
            }
        }
    }
}

fn record_from_row(row: sqlx::any::AnyRow) -> Result<JobRecord> {
    Ok(JobRecord {
        id: decode_job_id(&row.try_get::<String, _>("id")?)?,
        astrometry_id: u64::try_from(row.try_get::<i64, _>("astrometry_id")?)
            .context("SQL Astrometry ID is negative")?,
        owner: row.try_get("owner")?,
        queue_weight: row.try_get("queue_weight")?,
        object_key: row.try_get("object_key")?,
        original_filename: row.try_get("original_filename")?,
        content_type: row.try_get("content_type")?,
        options: serde_json::from_str(&row.try_get::<String, _>("options_json")?)?,
        status: JobStatus::parse(&row.try_get::<String, _>("status")?)
            .map_err(anyhow::Error::msg)?,
        created_at: decode_time(&row.try_get::<String, _>("created_at")?)?,
        started_at: row
            .try_get::<Option<String>, _>("started_at")?
            .as_deref()
            .map(decode_time)
            .transpose()?,
        completed_at: row
            .try_get::<Option<String>, _>("completed_at")?
            .as_deref()
            .map(decode_time)
            .transpose()?,
        solution: row
            .try_get::<Option<String>, _>("solution_json")?
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?,
        error: row.try_get("error")?,
        validation_donation: None,
    })
}

fn history_record_from_row(row: sqlx::any::AnyRow) -> Result<JobHistoryRecord> {
    Ok(JobHistoryRecord {
        id: decode_job_id(&row.try_get::<String, _>("id")?)?,
        status: JobStatus::parse(&row.try_get::<String, _>("status")?)
            .map_err(anyhow::Error::msg)?,
        original_filename: row.try_get("original_filename")?,
        created_at: decode_time(&row.try_get::<String, _>("created_at")?)?,
        started_at: row
            .try_get::<Option<String>, _>("started_at")?
            .as_deref()
            .map(decode_time)
            .transpose()?,
        completed_at: row
            .try_get::<Option<String>, _>("completed_at")?
            .as_deref()
            .map(decode_time)
            .transpose()?,
    })
}

fn decode_job_id(value: &str) -> Result<JobId> {
    Uuid::parse_str(value).context("SQL job ID is not a UUID")
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

fn encode_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

fn decode_time(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SolveOptions;

    async fn repository() -> SqlxJobRepository {
        let path =
            std::env::temp_dir().join(format!("seiza-server-test-{}.sqlite3", Uuid::now_v7()));
        SqlxJobRepository::connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .unwrap()
    }

    fn job(owner: &str) -> JobRecord {
        let id = Uuid::new_v4();
        JobRecord {
            id,
            astrometry_id: 0,
            owner: owner.into(),
            queue_weight: 1.0,
            object_key: format!("public-{id}/{}.fits", Uuid::now_v7()),
            original_filename: "test.fits".into(),
            content_type: None,
            options: SolveOptions::default(),
            status: JobStatus::Queued,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            solution: None,
            error: None,
            validation_donation: None,
        }
    }

    #[tokio::test]
    async fn persists_and_serves_least_recent_client() {
        let repository = repository().await;
        let first = repository.enqueue(job("a")).await.unwrap();
        assert_eq!(
            repository.claim(None, 60).await.unwrap().unwrap().job_id,
            first.id
        );
        let repeated = repository.enqueue(job("a")).await.unwrap();
        let unseen = repository.enqueue(job("b")).await.unwrap();
        assert_eq!(
            repository.claim(None, 60).await.unwrap().unwrap().job_id,
            unseen.id
        );
        assert_eq!(
            repository.claim(None, 60).await.unwrap().unwrap().job_id,
            repeated.id
        );
    }

    #[tokio::test]
    async fn enqueue_is_idempotent_for_an_uploaded_object() {
        let repository = repository().await;
        let upload = job("client");
        let first = repository.enqueue(upload.clone()).await.unwrap();
        let repeated = repository.enqueue(upload).await.unwrap();

        assert_eq!(repeated.id, first.id);
        assert_eq!(repository.queue_depth().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn lists_bounded_owner_history_newest_first() {
        let repository = repository().await;
        let now = Utc::now();
        let mut older = job("account:one");
        older.created_at = now - Duration::minutes(1);
        older.original_filename = "older.fits".into();
        let older = repository.enqueue(older).await.unwrap();
        let other = repository.enqueue(job("public:192.0.2.1")).await.unwrap();
        let mut newer = job("account:one");
        newer.created_at = now;
        newer.original_filename = "newer.fits".into();
        let newer = repository.enqueue(newer).await.unwrap();

        let history = repository.list_by_owner("account:one", 10).await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].id, newer.id);
        assert_eq!(history[1].id, older.id);
        assert!(!history.iter().any(|job| job.id == other.id));
        assert_eq!(
            repository.list_by_owner("account:one", 1).await.unwrap()[0].id,
            newer.id
        );
        assert!(
            repository
                .list_by_owner("account:one", 0)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rejects_a_stale_worker_completion() {
        let repository = repository().await;
        let queued = repository.enqueue(job("client")).await.unwrap();
        let lease = repository.claim(None, 60).await.unwrap().unwrap();
        assert!(
            !repository
                .complete(queued.id, "stale-token".into(), None, Some("failed".into()))
                .await
                .unwrap()
        );
        assert!(
            repository
                .complete(queued.id, lease.lease_token, None, Some("failed".into()))
                .await
                .unwrap()
        );
        assert_eq!(
            repository.get(queued.id).await.unwrap().unwrap().status,
            JobStatus::Failed
        );
    }

    #[tokio::test]
    async fn migrates_numeric_jobs_and_preserves_legacy_lookup() {
        sqlx::any::install_default_drivers();
        let path = std::env::temp_dir().join(format!(
            "seiza-server-legacy-test-{}.sqlite3",
            Uuid::now_v7()
        ));
        let database_url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = AnyPoolOptions::new().connect(&database_url).await.unwrap();
        for statement in [
            "CREATE TABLE jobs (id BIGINT PRIMARY KEY, owner TEXT NOT NULL, queue_weight DOUBLE PRECISION NOT NULL, object_key TEXT NOT NULL, original_filename TEXT NOT NULL, content_type TEXT, options_json TEXT NOT NULL, status TEXT NOT NULL, created_at TEXT NOT NULL, started_at TEXT, completed_at TEXT, solution_json TEXT, error TEXT, lease_token TEXT, lease_expires_at TEXT, attempts BIGINT NOT NULL DEFAULT 0)",
            "CREATE TABLE validation_donations (job_id BIGINT PRIMARY KEY, object_key TEXT NOT NULL UNIQUE, comment TEXT, solve_is_invalid BIGINT NOT NULL DEFAULT 0, license_version TEXT NOT NULL, donated_at TEXT NOT NULL)",
            "CREATE TABLE queue_outbox (job_id BIGINT PRIMARY KEY, delivered_at TEXT)",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        let job_id = Uuid::new_v4();
        let storage_token = Uuid::now_v7();
        let created_at = Utc::now();
        sqlx::query("INSERT INTO jobs (id, owner, queue_weight, object_key, original_filename, content_type, options_json, status, created_at, attempts) VALUES (67, 'legacy', 1.0, $1, 'legacy.fits', NULL, $2, 'failed', $3, 1)")
            .bind(format!("uploads/public-{job_id}/{storage_token}.fits"))
            .bind(serde_json::to_string(&SolveOptions::default()).unwrap())
            .bind(encode_time(created_at))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO validation_donations (job_id, object_key, comment, solve_is_invalid, license_version, donated_at) VALUES (67, 'validation/legacy.fits', 'legacy contribution', 1, 'validation-image-grant-v2', $1)")
            .bind(encode_time(created_at))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO queue_outbox (job_id, delivered_at) VALUES (67, NULL)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let repository = SqlxJobRepository::connect(&database_url).await.unwrap();
        let migrated = repository.get_by_legacy_id(67).await.unwrap().unwrap();
        assert_eq!(migrated.id, job_id);
        assert_eq!(migrated.astrometry_id, 67);
        assert!(migrated.validation_donation.is_some());
        assert_eq!(
            repository.get(job_id).await.unwrap().unwrap().id,
            migrated.id
        );
        assert_eq!(
            repository.pending_notifications(10).await.unwrap(),
            vec![migrated.id]
        );

        let fresh = repository.enqueue(job("new-client")).await.unwrap();
        assert_eq!(fresh.id.get_version_num(), 4);
        assert_eq!(fresh.astrometry_id, astrometry_id_for_job(fresh.id));
        assert_ne!(fresh.id, migrated.id);
    }
}
