use crate::{
    config::{Config, JobBackend},
    models::{
        JobId, JobLease, JobRecord, JobStatus, SolutionResponse, SolveOptions, ValidationDonation,
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
    async fn find_by_object_key(&self, object_key: &str) -> Result<Option<JobRecord>>;
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
    async fn retry_failed(&self, job_id: JobId, options: SolveOptions) -> Result<bool>;
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
enum SqlDialect {
    Sqlite,
    Postgres,
}

impl SqlxJobRepository {
    pub async fn connect(database_url: &str) -> Result<Self> {
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
        if dialect == SqlDialect::Sqlite {
            sqlx::query("PRAGMA journal_mode = WAL")
                .execute(&pool)
                .await
                .context("enabling SQLite WAL mode")?;
        }
        let repository = Self { pool, dialect };
        repository.migrate().await?;
        Ok(repository)
    }

    async fn migrate(&self) -> Result<()> {
        // BIGINT works in both PostgreSQL and SQLite. Job IDs come from an
        // explicit counter rather than engine-specific AUTOINCREMENT syntax.
        for statement in [
            "CREATE TABLE IF NOT EXISTS queue_counters (name TEXT PRIMARY KEY, value BIGINT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS jobs (id BIGINT PRIMARY KEY, owner TEXT NOT NULL, queue_weight DOUBLE PRECISION NOT NULL, object_key TEXT NOT NULL, original_filename TEXT NOT NULL, content_type TEXT, options_json TEXT NOT NULL, status TEXT NOT NULL, created_at TEXT NOT NULL, started_at TEXT, completed_at TEXT, solution_json TEXT, error TEXT, lease_token TEXT, lease_expires_at TEXT, attempts BIGINT NOT NULL DEFAULT 0)",
            "CREATE INDEX IF NOT EXISTS jobs_status_created_idx ON jobs(status, created_at)",
            "CREATE INDEX IF NOT EXISTS jobs_lease_idx ON jobs(status, lease_expires_at)",
            "CREATE UNIQUE INDEX IF NOT EXISTS jobs_object_key_idx ON jobs(object_key)",
            "CREATE TABLE IF NOT EXISTS validation_donations (job_id BIGINT PRIMARY KEY, object_key TEXT NOT NULL UNIQUE, comment TEXT, solve_is_invalid BIGINT NOT NULL DEFAULT 0, license_version TEXT NOT NULL, donated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS client_service (owner TEXT PRIMARY KEY, last_served_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS queue_outbox (job_id BIGINT PRIMARY KEY, delivered_at TEXT)",
            "INSERT INTO queue_counters (name, value) VALUES ('jobs', 0) ON CONFLICT(name) DO NOTHING",
        ] {
            sqlx::query(statement).execute(&self.pool).await?;
        }
        Ok(())
    }

    async fn validation_donation(&self, job_id: JobId) -> Result<Option<ValidationDonation>> {
        sqlx::query(
            "SELECT object_key, comment, solve_is_invalid, license_version, donated_at FROM validation_donations WHERE job_id = $1",
        )
        .bind(as_i64(job_id)?)
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
            sqlx::query("SELECT id FROM jobs WHERE status = $1 AND lease_expires_at <= $2")
                .bind("solving")
                .bind(encode_time(now))
                .fetch_all(&mut *connection)
                .await?;
        for row in expired {
            let id = job_id(row.try_get::<i64, _>("id")?)?;
            sqlx::query("UPDATE jobs SET status = 'queued', lease_token = NULL, lease_expires_at = NULL WHERE id = $1 AND status = 'solving'")
                .bind(as_i64(id)?)
                .execute(&mut *connection)
                .await?;
            sqlx::query("INSERT INTO queue_outbox (job_id, delivered_at) VALUES ($1, NULL) ON CONFLICT(job_id) DO UPDATE SET delivered_at = NULL")
                .bind(as_i64(id)?)
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
            return sqlx::query("SELECT * FROM jobs WHERE id = $1 AND status = 'queued'")
                .bind(as_i64(job_id)?)
                .fetch_optional(&mut *connection)
                .await?
                .map(record_from_row)
                .transpose();
        }
        let jobs =
            sqlx::query("SELECT * FROM jobs WHERE status = 'queued' ORDER BY created_at ASC")
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
        let row = sqlx::query(
            "UPDATE queue_counters SET value = value + 1 WHERE name = 'jobs' RETURNING value",
        )
        .fetch_one(&mut *transaction)
        .await?;
        job.id = job_id(row.try_get::<i64, _>("value")?)?;
        job.status = JobStatus::Queued;
        let inserted = sqlx::query("INSERT INTO jobs (id, owner, queue_weight, object_key, original_filename, content_type, options_json, status, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, 'queued', $8) ON CONFLICT(object_key) DO NOTHING")
            .bind(as_i64(job.id)?)
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
        sqlx::query("INSERT INTO queue_outbox (job_id, delivered_at) VALUES ($1, NULL)")
            .bind(as_i64(job.id)?)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(job)
    }

    async fn get(&self, job_id: JobId) -> Result<Option<JobRecord>> {
        let mut job = sqlx::query("SELECT * FROM jobs WHERE id = $1")
            .bind(as_i64(job_id)?)
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
        let mut job = sqlx::query("SELECT * FROM jobs WHERE object_key = $1")
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

    async fn queue_depth(&self) -> Result<usize> {
        let row = sqlx::query("SELECT COUNT(*) AS count FROM jobs WHERE status = 'queued'")
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
        let claimed = sqlx::query("UPDATE jobs SET status = 'solving', started_at = COALESCE(started_at, $1), lease_token = $2, lease_expires_at = $3, attempts = attempts + 1 WHERE id = $4 AND status = 'queued'")
            .bind(encode_time(now))
            .bind(&lease_token)
            .bind(encode_time(lease_expires_at))
            .bind(as_i64(job.id)?)
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
        let result = sqlx::query("UPDATE jobs SET lease_expires_at = $1 WHERE id = $2 AND status = 'solving' AND lease_token = $3 AND lease_expires_at > $4")
            .bind(encode_time(now + Duration::seconds(lease_seconds.max(1) as i64)))
            .bind(as_i64(job_id)?)
            .bind(lease_token)
            .bind(encode_time(now))
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn input_key(&self, job_id: JobId, lease_token: String) -> Result<Option<String>> {
        sqlx::query("SELECT COALESCE((SELECT object_key FROM validation_donations WHERE job_id = jobs.id), object_key) AS object_key FROM jobs WHERE id = $1 AND status = 'solving' AND lease_token = $2 AND lease_expires_at > $3")
            .bind(as_i64(job_id)?)
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
        let result = sqlx::query("UPDATE jobs SET status = $1, completed_at = $2, solution_json = $3, error = $4, lease_token = NULL, lease_expires_at = NULL WHERE id = $5 AND status = 'solving' AND lease_token = $6 AND lease_expires_at > $7")
            .bind(if solution.is_some() { "succeeded" } else { "failed" })
            .bind(encode_time(now))
            .bind(solution.map(|value| serde_json::to_string(&value)).transpose()?)
            .bind(error)
            .bind(as_i64(job_id)?)
            .bind(lease_token)
            .bind(encode_time(now))
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn retry_failed(&self, job_id: JobId, options: SolveOptions) -> Result<bool> {
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query("UPDATE jobs SET options_json = $1, status = 'queued', started_at = NULL, completed_at = NULL, solution_json = NULL, error = NULL, lease_token = NULL, lease_expires_at = NULL WHERE id = $2 AND status = 'failed'")
            .bind(serde_json::to_string(&options)?)
            .bind(as_i64(job_id)?)
            .execute(&mut *transaction)
            .await?;
        if result.rows_affected() == 1 {
            sqlx::query("INSERT INTO queue_outbox (job_id, delivered_at) VALUES ($1, NULL) ON CONFLICT(job_id) DO UPDATE SET delivered_at = NULL")
                .bind(as_i64(job_id)?)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            Ok(true)
        } else {
            transaction.rollback().await?;
            Ok(false)
        }
    }

    async fn donate_validation(&self, job_id: JobId, donation: ValidationDonation) -> Result<bool> {
        let mut transaction = self.pool.begin().await?;
        let status_query = if self.dialect == SqlDialect::Postgres {
            "SELECT status FROM jobs WHERE id = $1 FOR UPDATE"
        } else {
            "SELECT status FROM jobs WHERE id = $1"
        };
        let status = sqlx::query(status_query)
            .bind(as_i64(job_id)?)
            .fetch_optional(&mut *transaction)
            .await?
            .map(|row| row.try_get::<String, _>("status"))
            .transpose()?;
        if !status.is_some_and(|status| status == "succeeded" || status == "failed") {
            transaction.rollback().await?;
            return Ok(false);
        }
        sqlx::query("INSERT INTO validation_donations (job_id, object_key, comment, solve_is_invalid, license_version, donated_at) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT(job_id) DO UPDATE SET object_key = EXCLUDED.object_key, comment = EXCLUDED.comment, solve_is_invalid = EXCLUDED.solve_is_invalid")
            .bind(as_i64(job_id)?)
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
        let rows = sqlx::query("SELECT job_id FROM queue_outbox WHERE delivered_at IS NULL ORDER BY job_id ASC LIMIT $1")
            .bind(limit as i64)
            .fetch_all(&mut *transaction)
            .await?;
        transaction.commit().await?;
        rows.into_iter()
            .map(|row| job_id(row.try_get::<i64, _>("job_id")?))
            .collect()
    }

    async fn mark_notification_delivered(&self, job_id: JobId) -> Result<()> {
        sqlx::query("UPDATE queue_outbox SET delivered_at = $1 WHERE job_id = $2")
            .bind(encode_time(Utc::now()))
            .bind(as_i64(job_id)?)
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
        id: job_id(row.try_get::<i64, _>("id")?)?,
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

fn as_i64(value: JobId) -> Result<i64> {
    i64::try_from(value).context("job ID exceeds SQL BIGINT range")
}

fn job_id(value: i64) -> Result<JobId> {
    u64::try_from(value).context("SQL job ID is negative")
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
        JobRecord {
            id: 0,
            owner: owner.into(),
            queue_weight: 1.0,
            object_key: format!("test-{owner}-{}.fits", Uuid::now_v7()),
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
}
