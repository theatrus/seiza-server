use crate::{
    models::{JobId, JobStatus},
    repository::{SqlDialect, SqlxJobRepository},
};
use anyhow::{Context, Result, bail};
use aws_sdk_dynamodb::{
    Client,
    types::{AttributeValue, Put, TransactWriteItem},
};
use chrono::{DateTime, Utc};
use sqlx::{AnyConnection, AssertSqlSafe, Row};
use std::{
    collections::{HashMap, HashSet},
    env,
};
use uuid::Uuid;

type DynamoItem = HashMap<String, AttributeValue>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreBackend {
    Sqlx,
    DynamoDb,
}

impl StoreBackend {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "sqlx" | "sqlite" | "postgres" | "postgresql" => Ok(Self::Sqlx),
            "dynamodb" | "dynamo" => Ok(Self::DynamoDb),
            _ => bail!("unknown store backend {value}; use sqlx or dynamodb"),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Sqlx => "sqlx",
            Self::DynamoDb => "dynamodb",
        }
    }
}

pub struct MigrationArgs {
    from: StoreBackend,
    to: StoreBackend,
    sqlx_url: String,
    dynamodb_table: String,
    dry_run: bool,
    resume: bool,
}

impl MigrationArgs {
    pub fn from_env_and_args(args: &[String]) -> Result<Self> {
        let mut from = None;
        let mut to = None;
        let mut sqlx_url = env::var("SEIZA_SQL_DATABASE_URL").ok();
        let mut dynamodb_table = env::var("SEIZA_DYNAMODB_TABLE").ok();
        let mut dry_run = false;
        let mut resume = false;
        let mut values = args.iter();

        while let Some(flag) = values.next() {
            match flag.as_str() {
                "--from" => {
                    from = Some(StoreBackend::parse(
                        values.next().context("--from requires a backend")?,
                    )?)
                }
                "--to" => {
                    to = Some(StoreBackend::parse(
                        values.next().context("--to requires a backend")?,
                    )?)
                }
                "--sqlx-url" => {
                    sqlx_url = Some(values.next().context("--sqlx-url requires a URL")?.clone())
                }
                "--dynamodb-table" => {
                    dynamodb_table = Some(
                        values
                            .next()
                            .context("--dynamodb-table requires a table name")?
                            .clone(),
                    )
                }
                "--dry-run" => dry_run = true,
                "--resume" => resume = true,
                "--help" | "-h" => bail!(migration_usage()),
                value => bail!(
                    "unknown migrate-store option {value}\n{}",
                    migration_usage()
                ),
            }
        }

        let from = from.context("--from is required")?;
        let to = to.context("--to is required")?;
        if from == to {
            bail!("--from and --to must select different store backends");
        }
        let sqlx_url = sqlx_url
            .filter(|value| !value.trim().is_empty())
            .context("SEIZA_SQL_DATABASE_URL or --sqlx-url is required")?;
        let dynamodb_table = dynamodb_table
            .filter(|value| !value.trim().is_empty())
            .context("SEIZA_DYNAMODB_TABLE or --dynamodb-table is required")?;
        Ok(Self {
            from,
            to,
            sqlx_url,
            dynamodb_table,
            dry_run,
            resume,
        })
    }
}

pub fn migration_usage() -> &'static str {
    "usage: seiza-server migrate-store --from sqlx|dynamodb --to sqlx|dynamodb \
       [--sqlx-url URL] [--dynamodb-table TABLE] [--dry-run] [--resume]"
}

pub async fn run(args: MigrationArgs) -> Result<()> {
    // Initializing only the SQL destination keeps a SQL source read-only,
    // including during --dry-run. Legacy source rows are converted in memory.
    let sqlx = MigrationStore::Sqlx(
        SqlxStore::connect(&args.sqlx_url, args.to == StoreBackend::Sqlx).await?,
    );
    let dynamodb =
        MigrationStore::DynamoDb(DynamoStore::connect(args.dynamodb_table.clone()).await);
    let (source, destination) = match args.from {
        StoreBackend::Sqlx => (sqlx, dynamodb),
        StoreBackend::DynamoDb => (dynamodb, sqlx),
    };

    let source_snapshot = source
        .snapshot()
        .await
        .with_context(|| format!("reading {} source store", args.from.label()))?;
    source_snapshot.validate()?;
    let destination_snapshot = destination
        .snapshot()
        .await
        .with_context(|| format!("reading {} destination store", args.to.label()))?;
    destination_snapshot.validate()?;

    if args.resume {
        destination_snapshot.ensure_subset_of(&source_snapshot)?;
    } else if !destination_snapshot.is_empty() {
        bail!(
            "{} destination is not empty; use a new store or pass --resume only after verifying it contains a partial copy of this source",
            args.to.label()
        );
    }

    println!(
        "{} -> {}: {} jobs, {} validation contributions, {} client fairness records",
        args.from.label(),
        args.to.label(),
        source_snapshot.jobs.len(),
        source_snapshot.donations.len(),
        source_snapshot.clients.len()
    );
    if args.dry_run {
        println!("dry run complete; no records copied");
        return Ok(());
    }

    destination
        .import(&source_snapshot, args.resume)
        .await
        .with_context(|| format!("writing {} destination store", args.to.label()))?;
    let verified = destination
        .snapshot()
        .await
        .with_context(|| format!("verifying {} destination store", args.to.label()))?;
    verified.validate()?;
    if verified != source_snapshot {
        bail!(
            "destination verification failed: source has {} jobs/{} contributions/{} clients, destination has {} jobs/{} contributions/{} clients",
            source_snapshot.jobs.len(),
            source_snapshot.donations.len(),
            source_snapshot.clients.len(),
            verified.jobs.len(),
            verified.donations.len(),
            verified.clients.len()
        );
    }
    println!("migration complete; destination snapshot verified");
    Ok(())
}

enum MigrationStore {
    Sqlx(SqlxStore),
    DynamoDb(DynamoStore),
}

impl MigrationStore {
    async fn snapshot(&self) -> Result<StoreSnapshot> {
        match self {
            Self::Sqlx(store) => store.snapshot().await,
            Self::DynamoDb(store) => store.snapshot().await,
        }
    }

    async fn import(&self, snapshot: &StoreSnapshot, resume: bool) -> Result<()> {
        match self {
            Self::Sqlx(store) => store.import(snapshot).await,
            Self::DynamoDb(store) => store.import(snapshot, resume).await,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct StoreSnapshot {
    jobs: Vec<StoredJob>,
    donations: Vec<StoredDonation>,
    clients: Vec<StoredClient>,
}

impl StoreSnapshot {
    fn new(
        mut jobs: Vec<StoredJob>,
        mut donations: Vec<StoredDonation>,
        mut clients: Vec<StoredClient>,
    ) -> Self {
        jobs.sort_by_key(|job| job.id);
        donations.sort_by_key(|donation| donation.job_id);
        clients.sort_by(|left, right| left.owner.cmp(&right.owner));
        Self {
            jobs,
            donations,
            clients,
        }
    }

    fn is_empty(&self) -> bool {
        self.jobs.is_empty() && self.donations.is_empty() && self.clients.is_empty()
    }

    fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        let mut astrometry_ids = HashSet::new();
        let mut legacy_ids = HashSet::new();
        let mut object_keys = HashSet::new();
        for job in &self.jobs {
            if !ids.insert(job.id) {
                bail!("store contains duplicate job ID {}", job.id);
            }
            if !astrometry_ids.insert(job.astrometry_id) {
                bail!(
                    "store contains duplicate Astrometry.net ID {}",
                    job.astrometry_id
                );
            }
            if job.astrometry_id == 0 || job.astrometry_id > i64::MAX as u64 {
                bail!("job {} has an invalid Astrometry.net ID", job.id);
            }
            if let Some(legacy_id) = job.legacy_id {
                if legacy_id > i64::MAX as u64 {
                    bail!("legacy ID {legacy_id} exceeds SQL BIGINT range");
                }
                if !legacy_ids.insert(legacy_id) {
                    bail!("store contains duplicate legacy job ID {legacy_id}");
                }
            }
            if !object_keys.insert(job.object_key.as_str()) {
                bail!("store contains duplicate object key {}", job.object_key);
            }
            if job.attempts > i64::MAX as u64 {
                bail!("attempt count for job {} exceeds SQL BIGINT range", job.id);
            }
            if !job.queue_weight.is_finite() {
                bail!("job {} has a non-finite queue weight", job.id);
            }
            JobStatus::parse(&job.status).map_err(anyhow::Error::msg)?;
            serde_json::from_str::<serde_json::Value>(&job.options_json)
                .with_context(|| format!("job {} has invalid options JSON", job.id))?;
            if let Some(solution) = &job.solution_json {
                serde_json::from_str::<serde_json::Value>(solution)
                    .with_context(|| format!("job {} has invalid solution JSON", job.id))?;
            }
            for (name, value) in [
                ("created_at", Some(job.created_at.as_str())),
                ("started_at", job.started_at.as_deref()),
                ("completed_at", job.completed_at.as_deref()),
                ("lease_expires_at", job.lease_expires_at.as_deref()),
                (
                    "notification_delivered_at",
                    job.notification_delivered_at.as_deref(),
                ),
            ] {
                if let Some(value) = value {
                    parse_time(value)
                        .with_context(|| format!("job {} has invalid {name}", job.id))?;
                }
            }
        }

        let mut donated_jobs = HashSet::new();
        let mut donation_keys = HashSet::new();
        for donation in &self.donations {
            if !ids.contains(&donation.job_id) {
                bail!(
                    "validation contribution references missing job {}",
                    donation.job_id
                );
            }
            if !donated_jobs.insert(donation.job_id) {
                bail!(
                    "store contains duplicate validation contributions for job {}",
                    donation.job_id
                );
            }
            if !donation_keys.insert(donation.object_key.as_str()) {
                bail!(
                    "store contains duplicate validation object key {}",
                    donation.object_key
                );
            }
            parse_time(&donation.donated_at).with_context(|| {
                format!(
                    "validation contribution for job {} has invalid donated_at",
                    donation.job_id
                )
            })?;
        }

        let mut owners = HashSet::new();
        for client in &self.clients {
            if !owners.insert(client.owner.as_str()) {
                bail!("store contains duplicate client {}", client.owner);
            }
            parse_time(&client.last_served_at)
                .with_context(|| format!("client {} has invalid last_served_at", client.owner))?;
        }
        Ok(())
    }

    fn ensure_subset_of(&self, source: &Self) -> Result<()> {
        let source_jobs: HashMap<_, _> = source.jobs.iter().map(|job| (job.id, job)).collect();
        for job in &self.jobs {
            match source_jobs.get(&job.id) {
                Some(source_job) if *source_job == job => {}
                Some(_) => bail!("destination job {} differs from the source", job.id),
                None => bail!("destination job {} does not exist in the source", job.id),
            }
        }
        let source_donations: HashMap<_, _> = source
            .donations
            .iter()
            .map(|donation| (donation.job_id, donation))
            .collect();
        for donation in &self.donations {
            match source_donations.get(&donation.job_id) {
                Some(source_donation) if *source_donation == donation => {}
                Some(_) => bail!(
                    "destination validation contribution for job {} differs from the source",
                    donation.job_id
                ),
                None => bail!(
                    "destination validation contribution for job {} does not exist in the source",
                    donation.job_id
                ),
            }
        }
        let source_clients: HashMap<_, _> = source
            .clients
            .iter()
            .map(|client| (client.owner.as_str(), client))
            .collect();
        for client in &self.clients {
            match source_clients.get(client.owner.as_str()) {
                Some(source_client) if *source_client == client => {}
                Some(_) => bail!(
                    "destination client fairness record {} differs from the source",
                    client.owner
                ),
                None => bail!(
                    "destination client fairness record {} does not exist in the source",
                    client.owner
                ),
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
struct StoredJob {
    id: JobId,
    astrometry_id: u64,
    legacy_id: Option<u64>,
    owner: String,
    queue_weight: f64,
    object_key: String,
    original_filename: String,
    content_type: Option<String>,
    options_json: String,
    status: String,
    created_at: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    solution_json: Option<String>,
    error: Option<String>,
    lease_token: Option<String>,
    lease_expires_at: Option<String>,
    attempts: u64,
    notification_delivered_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct StoredClient {
    owner: String,
    last_served_at: String,
}

#[derive(Debug, Clone, PartialEq)]
struct StoredDonation {
    job_id: JobId,
    object_key: String,
    comment: Option<String>,
    solve_is_invalid: bool,
    license_version: String,
    donated_at: String,
}

struct SqlxStore {
    repository: SqlxJobRepository,
}

impl SqlxStore {
    async fn connect(database_url: &str, initialize_uuid_schema: bool) -> Result<Self> {
        Ok(Self {
            repository: SqlxJobRepository::connect_for_migration(
                database_url,
                initialize_uuid_schema,
            )
            .await?,
        })
    }

    async fn snapshot(&self) -> Result<StoreSnapshot> {
        let mut transaction = self.repository.pool().begin().await?;
        if self.repository.dialect() == SqlDialect::Postgres {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
                .execute(&mut *transaction)
                .await?;
        }
        let snapshot =
            if table_exists(&mut transaction, self.repository.dialect(), "jobs_v2").await? {
                ensure_sql_schema_supported(
                    &mut transaction,
                    self.repository.dialect(),
                    SqlSchema::Uuid,
                )
                .await?;
                snapshot_uuid_sql(&mut transaction).await?
            } else if table_exists(&mut transaction, self.repository.dialect(), "jobs").await? {
                ensure_sql_schema_supported(
                    &mut transaction,
                    self.repository.dialect(),
                    SqlSchema::Legacy,
                )
                .await?;
                snapshot_legacy_sql(&mut transaction).await?
            } else {
                StoreSnapshot::new(Vec::new(), Vec::new(), Vec::new())
            };
        transaction.commit().await?;
        Ok(snapshot)
    }

    async fn import(&self, snapshot: &StoreSnapshot) -> Result<()> {
        let mut transaction = self.repository.pool().begin().await?;
        if !table_exists(&mut transaction, self.repository.dialect(), "jobs_v2").await? {
            bail!("SQLx destination does not have the UUID job schema");
        }
        for job in &snapshot.jobs {
            sqlx::query(
                "INSERT INTO jobs_v2 (id, astrometry_id, legacy_id, owner, queue_weight, object_key, original_filename, content_type, options_json, status, created_at, started_at, completed_at, solution_json, error, lease_token, lease_expires_at, attempts) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) ON CONFLICT(id) DO UPDATE SET astrometry_id = EXCLUDED.astrometry_id, legacy_id = EXCLUDED.legacy_id, owner = EXCLUDED.owner, queue_weight = EXCLUDED.queue_weight, object_key = EXCLUDED.object_key, original_filename = EXCLUDED.original_filename, content_type = EXCLUDED.content_type, options_json = EXCLUDED.options_json, status = EXCLUDED.status, created_at = EXCLUDED.created_at, started_at = EXCLUDED.started_at, completed_at = EXCLUDED.completed_at, solution_json = EXCLUDED.solution_json, error = EXCLUDED.error, lease_token = EXCLUDED.lease_token, lease_expires_at = EXCLUDED.lease_expires_at, attempts = EXCLUDED.attempts",
            )
            .bind(job.id.to_string())
            .bind(to_i64(job.astrometry_id, "Astrometry.net ID")?)
            .bind(
                job.legacy_id
                    .map(|value| to_i64(value, "legacy job ID"))
                    .transpose()?,
            )
            .bind(&job.owner)
            .bind(job.queue_weight)
            .bind(&job.object_key)
            .bind(&job.original_filename)
            .bind(&job.content_type)
            .bind(&job.options_json)
            .bind(&job.status)
            .bind(&job.created_at)
            .bind(&job.started_at)
            .bind(&job.completed_at)
            .bind(&job.solution_json)
            .bind(&job.error)
            .bind(&job.lease_token)
            .bind(&job.lease_expires_at)
            .bind(to_i64(job.attempts, "attempt count")?)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "INSERT INTO queue_outbox_v2 (job_id, delivered_at) VALUES ($1, $2) ON CONFLICT(job_id) DO UPDATE SET delivered_at = EXCLUDED.delivered_at",
            )
            .bind(job.id.to_string())
            .bind(&job.notification_delivered_at)
            .execute(&mut *transaction)
            .await?;
        }
        for donation in &snapshot.donations {
            sqlx::query(
                "INSERT INTO validation_donations_v2 (job_id, object_key, comment, solve_is_invalid, license_version, donated_at) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT(job_id) DO UPDATE SET object_key = EXCLUDED.object_key, comment = EXCLUDED.comment, solve_is_invalid = EXCLUDED.solve_is_invalid, license_version = EXCLUDED.license_version, donated_at = EXCLUDED.donated_at",
            )
            .bind(donation.job_id.to_string())
            .bind(&donation.object_key)
            .bind(&donation.comment)
            .bind(i64::from(donation.solve_is_invalid))
            .bind(&donation.license_version)
            .bind(&donation.donated_at)
            .execute(&mut *transaction)
            .await?;
        }
        for client in &snapshot.clients {
            sqlx::query(
                "INSERT INTO client_service (owner, last_served_at) VALUES ($1, $2) ON CONFLICT(owner) DO UPDATE SET last_served_at = EXCLUDED.last_served_at",
            )
            .bind(&client.owner)
            .bind(&client.last_served_at)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }
}

async fn snapshot_uuid_sql(connection: &mut AnyConnection) -> Result<StoreSnapshot> {
    let rows = sqlx::query(
        "SELECT j.*, q.job_id AS outbox_job_id, q.delivered_at AS notification_delivered_at FROM jobs_v2 j LEFT JOIN queue_outbox_v2 q ON q.job_id = j.id ORDER BY j.id",
    )
    .fetch_all(&mut *connection)
    .await?;
    let mut jobs = Vec::with_capacity(rows.len());
    for row in rows {
        let id = parse_uuid(&row.try_get::<String, _>("id")?, "SQLx job ID")?;
        if row.try_get::<Option<String>, _>("outbox_job_id")?.is_none() {
            bail!("SQLx job {id} has no queue_outbox_v2 record");
        }
        jobs.push(StoredJob {
            id,
            astrometry_id: from_i64(row.try_get("astrometry_id")?, "Astrometry.net ID")?,
            legacy_id: row
                .try_get::<Option<i64>, _>("legacy_id")?
                .map(|value| from_i64(value, "legacy job ID"))
                .transpose()?,
            owner: row.try_get("owner")?,
            queue_weight: row.try_get("queue_weight")?,
            object_key: row.try_get("object_key")?,
            original_filename: row.try_get("original_filename")?,
            content_type: row.try_get("content_type")?,
            options_json: row.try_get("options_json")?,
            status: row.try_get("status")?,
            created_at: row.try_get("created_at")?,
            started_at: row.try_get("started_at")?,
            completed_at: row.try_get("completed_at")?,
            solution_json: row.try_get("solution_json")?,
            error: row.try_get("error")?,
            lease_token: row.try_get("lease_token")?,
            lease_expires_at: row.try_get("lease_expires_at")?,
            attempts: from_i64(row.try_get("attempts")?, "attempt count")?,
            notification_delivered_at: row.try_get("notification_delivered_at")?,
        });
    }
    ensure_no_orphan_outbox(connection, "queue_outbox_v2", "jobs_v2").await?;
    let donations = sqlx::query(
        "SELECT job_id, object_key, comment, solve_is_invalid, license_version, donated_at FROM validation_donations_v2 ORDER BY job_id",
    )
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|row| {
        Ok(StoredDonation {
            job_id: parse_uuid(
                &row.try_get::<String, _>("job_id")?,
                "validation contribution job ID",
            )?,
            object_key: row.try_get("object_key")?,
            comment: row.try_get("comment")?,
            solve_is_invalid: row.try_get::<i64, _>("solve_is_invalid")? != 0,
            license_version: row.try_get("license_version")?,
            donated_at: row.try_get("donated_at")?,
        })
    })
    .collect::<Result<Vec<_>>>()?;
    let clients = sql_clients(connection).await?;
    Ok(StoreSnapshot::new(jobs, donations, clients))
}

async fn snapshot_legacy_sql(connection: &mut AnyConnection) -> Result<StoreSnapshot> {
    let rows = sqlx::query(
        "SELECT j.*, q.job_id AS outbox_job_id, q.delivered_at AS notification_delivered_at FROM jobs j LEFT JOIN queue_outbox q ON q.job_id = j.id ORDER BY j.id",
    )
    .fetch_all(&mut *connection)
    .await?;
    let mut jobs = Vec::with_capacity(rows.len());
    let mut legacy_to_uuid = HashMap::new();
    for row in rows {
        let legacy_id = from_i64(row.try_get("id")?, "legacy job ID")?;
        if row.try_get::<Option<i64>, _>("outbox_job_id")?.is_none() {
            bail!("legacy SQLx job {legacy_id} has no queue_outbox record");
        }
        let object_key: String = row.try_get("object_key")?;
        let id = job_id_from_object_key(&object_key).with_context(|| {
            format!("legacy SQLx job {legacy_id} object key has no UUID identity")
        })?;
        let status: String = row.try_get("status")?;
        let notification_delivered_at = if status == "queued" {
            None
        } else {
            row.try_get("notification_delivered_at")?
        };
        legacy_to_uuid.insert(legacy_id, id);
        jobs.push(StoredJob {
            id,
            astrometry_id: legacy_id,
            legacy_id: Some(legacy_id),
            owner: row.try_get("owner")?,
            queue_weight: row.try_get("queue_weight")?,
            object_key,
            original_filename: row.try_get("original_filename")?,
            content_type: row.try_get("content_type")?,
            options_json: row.try_get("options_json")?,
            status,
            created_at: row.try_get("created_at")?,
            started_at: row.try_get("started_at")?,
            completed_at: row.try_get("completed_at")?,
            solution_json: row.try_get("solution_json")?,
            error: row.try_get("error")?,
            lease_token: row.try_get("lease_token")?,
            lease_expires_at: row.try_get("lease_expires_at")?,
            attempts: from_i64(row.try_get("attempts")?, "attempt count")?,
            notification_delivered_at,
        });
    }
    ensure_no_orphan_outbox(connection, "queue_outbox", "jobs").await?;
    let donations = sqlx::query(
        "SELECT job_id, object_key, comment, solve_is_invalid, license_version, donated_at FROM validation_donations ORDER BY job_id",
    )
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|row| {
        let legacy_id = from_i64(row.try_get("job_id")?, "validation contribution job ID")?;
        Ok(StoredDonation {
            job_id: *legacy_to_uuid.get(&legacy_id).with_context(|| {
                format!("validation contribution references missing legacy job {legacy_id}")
            })?,
            object_key: row.try_get("object_key")?,
            comment: row.try_get("comment")?,
            solve_is_invalid: row.try_get::<i64, _>("solve_is_invalid")? != 0,
            license_version: row.try_get("license_version")?,
            donated_at: row.try_get("donated_at")?,
        })
    })
    .collect::<Result<Vec<_>>>()?;
    let clients = sql_clients(connection).await?;
    Ok(StoreSnapshot::new(jobs, donations, clients))
}

async fn sql_clients(connection: &mut AnyConnection) -> Result<Vec<StoredClient>> {
    sqlx::query("SELECT owner, last_served_at FROM client_service ORDER BY owner")
        .fetch_all(&mut *connection)
        .await?
        .into_iter()
        .map(|row| {
            Ok(StoredClient {
                owner: row.try_get("owner")?,
                last_served_at: row.try_get("last_served_at")?,
            })
        })
        .collect()
}

async fn ensure_no_orphan_outbox(
    connection: &mut AnyConnection,
    outbox: &str,
    jobs: &str,
) -> Result<()> {
    let query = AssertSqlSafe(format!(
        "SELECT COUNT(*) AS count FROM {outbox} q LEFT JOIN {jobs} j ON j.id = q.job_id WHERE j.id IS NULL"
    ));
    let count = sqlx::query(query)
        .fetch_one(&mut *connection)
        .await?
        .try_get::<i64, _>("count")?;
    if count != 0 {
        bail!("SQLx store contains {count} orphaned {outbox} records");
    }
    Ok(())
}

struct DynamoStore {
    client: Client,
    table: String,
}

#[derive(Debug)]
enum JobReference {
    Uuid(Uuid),
    Legacy(u64),
}

impl DynamoStore {
    async fn connect(table: String) -> Self {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        Self {
            client: Client::new(&config),
            table,
        }
    }

    async fn snapshot(&self) -> Result<StoreSnapshot> {
        let mut jobs = Vec::new();
        let mut donations = Vec::new();
        let mut clients = Vec::new();
        let mut object_indices = Vec::new();
        let mut astrometry_indices = Vec::new();
        let mut legacy_indices = Vec::new();
        for item in self.scan_all().await? {
            let pk = required_string(&item, "pk")?;
            match optional_string(&item, "entity").as_deref() {
                None if pk == "COUNTER#jobs" => {
                    ensure_only_attributes(&item, &pk, &["pk", "value"])?;
                    let _ = required_u64(&item, "value")?;
                }
                Some("job") => {
                    ensure_only_attributes(
                        &item,
                        &pk,
                        &[
                            "pk",
                            "entity",
                            "id",
                            "astrometry_id",
                            "legacy_id",
                            "owner",
                            "queue_weight",
                            "object_key",
                            "original_filename",
                            "content_type",
                            "options_json",
                            "status",
                            "created_at",
                            "started_at",
                            "completed_at",
                            "solution_json",
                            "error",
                            "lease_token",
                            "lease_expires_at",
                            "attempts",
                            "notification_delivered_at",
                            "validation_object_key",
                            "validation_comment",
                            "validation_solve_is_invalid",
                            "validation_license_version",
                            "validation_donated_at",
                        ],
                    )?;
                    let job = job_from_item(&item)?;
                    if let Some(donation) = donation_from_item(&item, job.id)? {
                        donations.push(donation);
                    }
                    jobs.push(job);
                }
                Some("client") => {
                    ensure_only_attributes(&item, &pk, &["pk", "entity", "last_served_at"])?;
                    let owner = pk
                        .strip_prefix("CLIENT#")
                        .context("DynamoDB client record has an invalid partition key")?;
                    clients.push(StoredClient {
                        owner: owner.to_owned(),
                        last_served_at: required_string(&item, "last_served_at")?,
                    });
                }
                Some("object_index") => {
                    ensure_only_attributes(&item, &pk, &["pk", "entity", "job_id"])?;
                    object_indices.push((
                        pk.strip_prefix("OBJECT#")
                            .context("DynamoDB object index has an invalid partition key")?
                            .to_owned(),
                        required_job_reference(&item, "job_id")?,
                    ));
                }
                Some("astrometry_index") => {
                    ensure_only_attributes(&item, &pk, &["pk", "entity", "job_id"])?;
                    let id: u64 = pk
                        .strip_prefix("ASTROMETRY#")
                        .context("DynamoDB Astrometry.net index has an invalid partition key")?
                        .parse()
                        .context("DynamoDB Astrometry.net index ID is not a u64")?;
                    astrometry_indices.push((id, required_job_reference(&item, "job_id")?));
                }
                Some("legacy_index") => {
                    ensure_only_attributes(&item, &pk, &["pk", "entity", "job_id"])?;
                    let id: u64 = pk
                        .strip_prefix("LEGACY#")
                        .context("DynamoDB legacy index has an invalid partition key")?
                        .parse()
                        .context("DynamoDB legacy index ID is not a u64")?;
                    legacy_indices.push((id, required_job_reference(&item, "job_id")?));
                }
                entity => bail!(
                    "DynamoDB table contains unsupported item {pk} with entity {:?}",
                    entity
                ),
            }
        }

        let jobs_by_id: HashMap<_, _> = jobs.iter().map(|job| (job.id, job)).collect();
        let legacy_to_uuid: HashMap<_, _> = jobs
            .iter()
            .filter_map(|job| job.legacy_id.map(|legacy_id| (legacy_id, job.id)))
            .collect();
        for (object_key, reference) in object_indices {
            let job_id = resolve_job_reference(reference, &legacy_to_uuid)?;
            let job = jobs_by_id.get(&job_id).with_context(|| {
                format!("DynamoDB object index {object_key} references missing job {job_id}")
            })?;
            if job.object_key != object_key {
                bail!(
                    "DynamoDB object index {object_key} does not match job {job_id} object key {}",
                    job.object_key
                );
            }
        }
        for (astrometry_id, reference) in astrometry_indices {
            let job_id = resolve_job_reference(reference, &legacy_to_uuid)?;
            let job = jobs_by_id.get(&job_id).with_context(|| {
                format!(
                    "DynamoDB Astrometry.net index {astrometry_id} references missing job {job_id}"
                )
            })?;
            if job.astrometry_id != astrometry_id {
                bail!("DynamoDB Astrometry.net index {astrometry_id} does not match job {job_id}");
            }
        }
        for (legacy_id, reference) in legacy_indices {
            let job_id = resolve_job_reference(reference, &legacy_to_uuid)?;
            let job = jobs_by_id.get(&job_id).with_context(|| {
                format!("DynamoDB legacy index {legacy_id} references missing job {job_id}")
            })?;
            if job.legacy_id != Some(legacy_id) {
                bail!("DynamoDB legacy index {legacy_id} does not match job {job_id}");
            }
        }
        Ok(StoreSnapshot::new(jobs, donations, clients))
    }

    async fn scan_all(&self) -> Result<Vec<DynamoItem>> {
        let mut items = Vec::new();
        let mut start_key = None;
        loop {
            let output = self
                .client
                .scan()
                .table_name(&self.table)
                .consistent_read(true)
                .set_exclusive_start_key(start_key)
                .send()
                .await
                .with_context(|| format!("scanning DynamoDB table {}", self.table))?;
            items.extend(output.items().iter().cloned());
            start_key = output.last_evaluated_key().cloned();
            if start_key.is_none() {
                return Ok(items);
            }
        }
    }

    async fn import(&self, snapshot: &StoreSnapshot, resume: bool) -> Result<()> {
        let donations: HashMap<_, _> = snapshot
            .donations
            .iter()
            .map(|donation| (donation.job_id, donation))
            .collect();
        for job in &snapshot.jobs {
            let mut items = Vec::new();
            for item in [
                job_to_item(job, donations.get(&job.id).copied()),
                HashMap::from([
                    ("pk".into(), string(object_index_key(&job.object_key))),
                    ("entity".into(), string("object_index")),
                    ("job_id".into(), string(job.id)),
                ]),
                HashMap::from([
                    ("pk".into(), string(astrometry_index_key(job.astrometry_id))),
                    ("entity".into(), string("astrometry_index")),
                    ("job_id".into(), string(job.id)),
                ]),
            ] {
                items.push(dynamo_put(&self.table, item, !resume)?);
            }
            if let Some(legacy_id) = job.legacy_id {
                items.push(dynamo_put(
                    &self.table,
                    HashMap::from([
                        ("pk".into(), string(legacy_index_key(legacy_id))),
                        ("entity".into(), string("legacy_index")),
                        ("job_id".into(), string(job.id)),
                    ]),
                    !resume,
                )?);
            }
            self.client
                .transact_write_items()
                .set_transact_items(Some(items))
                .send()
                .await
                .with_context(|| format!("importing DynamoDB job {}", job.id))?;
        }
        for client in &snapshot.clients {
            self.put_item(
                HashMap::from([
                    ("pk".into(), string(client_key(&client.owner))),
                    ("entity".into(), string("client")),
                    ("last_served_at".into(), string(&client.last_served_at)),
                ]),
                !resume,
            )
            .await
            .with_context(|| {
                format!("importing DynamoDB client fairness record {}", client.owner)
            })?;
        }
        Ok(())
    }

    async fn put_item(&self, item: DynamoItem, require_empty: bool) -> Result<()> {
        let mut request = self
            .client
            .put_item()
            .table_name(&self.table)
            .set_item(Some(item));
        if require_empty {
            request = request.condition_expression("attribute_not_exists(pk)");
        }
        request.send().await?;
        Ok(())
    }
}

fn dynamo_put(table: &str, item: DynamoItem, require_empty: bool) -> Result<TransactWriteItem> {
    let mut put = Put::builder().table_name(table).set_item(Some(item));
    if require_empty {
        put = put.condition_expression("attribute_not_exists(pk)");
    }
    Ok(TransactWriteItem::builder().put(put.build()?).build())
}

fn job_from_item(item: &DynamoItem) -> Result<StoredJob> {
    let object_key = required_string(item, "object_key")?;
    let (id, astrometry_id, legacy_id, expected_pk, converted_from_numeric) = match item.get("id") {
        Some(AttributeValue::S(value)) => {
            let id = parse_uuid(value, "DynamoDB job ID")?;
            (
                id,
                required_u64(item, "astrometry_id")?,
                optional_u64(item, "legacy_id")?,
                job_key(id),
                false,
            )
        }
        Some(AttributeValue::N(value)) => {
            let legacy_id: u64 = value
                .parse()
                .context("DynamoDB legacy job ID is not a u64")?;
            let id = job_id_from_object_key(&object_key).with_context(|| {
                format!("legacy DynamoDB job {legacy_id} object key has no UUID identity")
            })?;
            (
                id,
                legacy_id,
                Some(legacy_id),
                format!("JOB#{legacy_id}"),
                true,
            )
        }
        _ => bail!("DynamoDB item is missing job ID"),
    };
    let pk = required_string(item, "pk")?;
    if pk != expected_pk {
        bail!("DynamoDB job {id} has invalid partition key {pk}");
    }
    let status = required_string(item, "status")?;
    let notification_delivered_at = if converted_from_numeric && status == "queued" {
        None
    } else {
        optional_string(item, "notification_delivered_at")
    };
    Ok(StoredJob {
        id,
        astrometry_id,
        legacy_id,
        owner: required_string(item, "owner")?,
        queue_weight: required_number(item, "queue_weight")?.parse()?,
        object_key,
        original_filename: required_string(item, "original_filename")?,
        content_type: optional_string(item, "content_type"),
        options_json: required_string(item, "options_json")?,
        status,
        created_at: required_string(item, "created_at")?,
        started_at: optional_string(item, "started_at"),
        completed_at: optional_string(item, "completed_at"),
        solution_json: optional_string(item, "solution_json"),
        error: optional_string(item, "error"),
        lease_token: optional_string(item, "lease_token"),
        lease_expires_at: optional_string(item, "lease_expires_at"),
        attempts: required_u64(item, "attempts")?,
        notification_delivered_at,
    })
}

fn donation_from_item(item: &DynamoItem, job_id: JobId) -> Result<Option<StoredDonation>> {
    let Some(object_key) = optional_string(item, "validation_object_key") else {
        if [
            "validation_object_key",
            "validation_comment",
            "validation_solve_is_invalid",
            "validation_license_version",
            "validation_donated_at",
        ]
        .iter()
        .any(|name| item.contains_key(*name))
        {
            bail!("DynamoDB job {job_id} has incomplete validation contribution metadata");
        }
        return Ok(None);
    };
    Ok(Some(StoredDonation {
        job_id,
        object_key,
        comment: optional_string(item, "validation_comment"),
        solve_is_invalid: optional_bool(item, "validation_solve_is_invalid").unwrap_or(false),
        license_version: required_string(item, "validation_license_version")?,
        donated_at: required_string(item, "validation_donated_at")?,
    }))
}

fn job_to_item(job: &StoredJob, donation: Option<&StoredDonation>) -> DynamoItem {
    let mut item = HashMap::from([
        ("pk".into(), string(job_key(job.id))),
        ("entity".into(), string("job")),
        ("id".into(), string(job.id)),
        ("astrometry_id".into(), number(job.astrometry_id)),
        ("owner".into(), string(&job.owner)),
        ("queue_weight".into(), number(job.queue_weight)),
        ("object_key".into(), string(&job.object_key)),
        ("original_filename".into(), string(&job.original_filename)),
        ("options_json".into(), string(&job.options_json)),
        ("status".into(), string(&job.status)),
        ("created_at".into(), string(&job.created_at)),
        ("attempts".into(), number(job.attempts)),
    ]);
    if let Some(legacy_id) = job.legacy_id {
        item.insert("legacy_id".into(), number(legacy_id));
    }
    for (name, value) in [
        ("content_type", job.content_type.as_deref()),
        ("started_at", job.started_at.as_deref()),
        ("completed_at", job.completed_at.as_deref()),
        ("solution_json", job.solution_json.as_deref()),
        ("error", job.error.as_deref()),
        ("lease_token", job.lease_token.as_deref()),
        ("lease_expires_at", job.lease_expires_at.as_deref()),
        (
            "notification_delivered_at",
            job.notification_delivered_at.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            item.insert(name.into(), string(value));
        }
    }
    if let Some(donation) = donation {
        item.insert("validation_object_key".into(), string(&donation.object_key));
        item.insert(
            "validation_license_version".into(),
            string(&donation.license_version),
        );
        item.insert("validation_donated_at".into(), string(&donation.donated_at));
        item.insert(
            "validation_solve_is_invalid".into(),
            AttributeValue::Bool(donation.solve_is_invalid),
        );
        item.insert(
            "validation_comment".into(),
            donation
                .comment
                .as_ref()
                .map_or(AttributeValue::Null(true), string),
        );
    }
    item
}

fn resolve_job_reference(
    reference: JobReference,
    legacy_to_uuid: &HashMap<u64, Uuid>,
) -> Result<Uuid> {
    match reference {
        JobReference::Uuid(id) => Ok(id),
        JobReference::Legacy(id) => legacy_to_uuid
            .get(&id)
            .copied()
            .with_context(|| format!("DynamoDB index references missing legacy job {id}")),
    }
}

fn required_job_reference(item: &DynamoItem, name: &str) -> Result<JobReference> {
    match item.get(name) {
        Some(AttributeValue::S(value)) => Ok(JobReference::Uuid(parse_uuid(
            value,
            "DynamoDB index job ID",
        )?)),
        Some(AttributeValue::N(value)) => Ok(JobReference::Legacy(
            value
                .parse()
                .context("DynamoDB index job ID is not a u64")?,
        )),
        _ => bail!("DynamoDB item is missing job reference {name}"),
    }
}

fn job_key(job_id: Uuid) -> String {
    format!("JOB#{job_id}")
}

fn astrometry_index_key(id: u64) -> String {
    format!("ASTROMETRY#{id}")
}

fn legacy_index_key(id: u64) -> String {
    format!("LEGACY#{id}")
}

fn client_key(owner: &str) -> String {
    format!("CLIENT#{owner}")
}

fn object_index_key(object_key: &str) -> String {
    format!("OBJECT#{object_key}")
}

fn string(value: impl ToString) -> AttributeValue {
    AttributeValue::S(value.to_string())
}

fn number(value: impl ToString) -> AttributeValue {
    AttributeValue::N(value.to_string())
}

fn optional_string(item: &DynamoItem, name: &str) -> Option<String> {
    item.get(name).and_then(|value| value.as_s().ok()).cloned()
}

fn optional_bool(item: &DynamoItem, name: &str) -> Option<bool> {
    item.get(name)
        .and_then(|value| value.as_bool().ok())
        .copied()
}

fn required_string(item: &DynamoItem, name: &str) -> Result<String> {
    optional_string(item, name).with_context(|| format!("DynamoDB item is missing string {name}"))
}

fn required_number(item: &DynamoItem, name: &str) -> Result<String> {
    item.get(name)
        .and_then(|value| value.as_n().ok())
        .cloned()
        .with_context(|| format!("DynamoDB item is missing number {name}"))
}

fn required_u64(item: &DynamoItem, name: &str) -> Result<u64> {
    required_number(item, name)?
        .parse()
        .with_context(|| format!("DynamoDB field {name} is not a u64"))
}

fn optional_u64(item: &DynamoItem, name: &str) -> Result<Option<u64>> {
    item.get(name)
        .map(|value| {
            value
                .as_n()
                .map_err(|_| anyhow::anyhow!("DynamoDB field {name} is not numeric"))?
                .parse()
                .with_context(|| format!("DynamoDB field {name} is not a u64"))
        })
        .transpose()
}

fn ensure_only_attributes(item: &DynamoItem, pk: &str, supported: &[&str]) -> Result<()> {
    let mut unknown = item
        .keys()
        .filter(|name| !supported.contains(&name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    unknown.sort();
    if !unknown.is_empty() {
        bail!(
            "DynamoDB item {pk} has unsupported attributes: {}",
            unknown.join(", ")
        );
    }
    Ok(())
}

fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn parse_uuid(value: &str, field: &str) -> Result<Uuid> {
    Uuid::parse_str(value).with_context(|| format!("{field} is not a UUID"))
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

#[derive(Clone, Copy)]
enum SqlSchema {
    Uuid,
    Legacy,
}

async fn ensure_sql_schema_supported(
    connection: &mut AnyConnection,
    dialect: SqlDialect,
    schema: SqlSchema,
) -> Result<()> {
    let uuid_tables: &[(&str, &[&str])] = &[
        (
            "jobs_v2",
            &[
                "id",
                "astrometry_id",
                "legacy_id",
                "owner",
                "queue_weight",
                "object_key",
                "original_filename",
                "content_type",
                "options_json",
                "status",
                "created_at",
                "started_at",
                "completed_at",
                "solution_json",
                "error",
                "lease_token",
                "lease_expires_at",
                "attempts",
            ],
        ),
        (
            "validation_donations_v2",
            &[
                "job_id",
                "object_key",
                "comment",
                "solve_is_invalid",
                "license_version",
                "donated_at",
            ],
        ),
        ("client_service", &["owner", "last_served_at"]),
        ("queue_outbox_v2", &["job_id", "delivered_at"]),
    ];
    let legacy_tables: &[(&str, &[&str])] = &[
        (
            "jobs",
            &[
                "id",
                "owner",
                "queue_weight",
                "object_key",
                "original_filename",
                "content_type",
                "options_json",
                "status",
                "created_at",
                "started_at",
                "completed_at",
                "solution_json",
                "error",
                "lease_token",
                "lease_expires_at",
                "attempts",
            ],
        ),
        (
            "validation_donations",
            &[
                "job_id",
                "object_key",
                "comment",
                "solve_is_invalid",
                "license_version",
                "donated_at",
            ],
        ),
        ("client_service", &["owner", "last_served_at"]),
        ("queue_outbox", &["job_id", "delivered_at"]),
        ("queue_counters", &["name", "value"]),
    ];
    let tables = match schema {
        SqlSchema::Uuid => uuid_tables,
        SqlSchema::Legacy => legacy_tables,
    };
    for (table, supported) in tables {
        let actual = table_columns(connection, dialect, table).await?;
        let expected = supported
            .iter()
            .map(|column| (*column).to_owned())
            .collect::<HashSet<_>>();
        if actual != expected {
            let mut unsupported = actual.difference(&expected).cloned().collect::<Vec<_>>();
            let mut missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
            unsupported.sort();
            missing.sort();
            bail!(
                "SQLx table {table} has an unsupported schema (unknown columns: {}; missing columns: {})",
                unsupported.join(", "),
                missing.join(", ")
            );
        }
    }
    Ok(())
}

async fn table_exists(
    connection: &mut AnyConnection,
    dialect: SqlDialect,
    table: &str,
) -> Result<bool> {
    Ok(!table_columns(connection, dialect, table).await?.is_empty())
}

async fn table_columns(
    connection: &mut AnyConnection,
    dialect: SqlDialect,
    table: &str,
) -> Result<HashSet<String>> {
    let rows = match dialect {
        SqlDialect::Sqlite => {
            // table only comes from fixed internal names.
            sqlx::query(AssertSqlSafe(format!("PRAGMA table_info({table})")))
                .fetch_all(&mut *connection)
                .await?
        }
        SqlDialect::Postgres => sqlx::query(
            "SELECT column_name AS name FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = $1",
        )
        .bind(table)
        .fetch_all(&mut *connection)
        .await?,
    };
    rows.into_iter()
        .map(|row| row.try_get::<String, _>("name"))
        .collect::<std::result::Result<_, _>>()
        .map_err(Into::into)
}

fn from_i64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).with_context(|| format!("SQLx {field} is negative"))
}

fn to_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).with_context(|| format!("{field} exceeds SQL BIGINT range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_id() -> Uuid {
        Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
    }

    fn sample_snapshot() -> StoreSnapshot {
        StoreSnapshot::new(
            vec![StoredJob {
                id: sample_id(),
                astrometry_id: 7,
                legacy_id: Some(7),
                owner: "client-a".into(),
                queue_weight: 1.5,
                object_key: format!("uploads/public-{}/image.fits", sample_id()),
                original_filename: "example.fits".into(),
                content_type: Some("image/fits".into()),
                options_json: "{\"sigma\":4.0}".into(),
                status: "solving".into(),
                created_at: "2026-07-15T10:00:00+00:00".into(),
                started_at: Some("2026-07-15T10:01:00+00:00".into()),
                completed_at: None,
                solution_json: None,
                error: None,
                lease_token: Some("lease-token".into()),
                lease_expires_at: Some("2026-07-15T10:16:00+00:00".into()),
                attempts: 2,
                notification_delivered_at: Some("2026-07-15T10:00:01+00:00".into()),
            }],
            vec![StoredDonation {
                job_id: sample_id(),
                object_key: "validation/example.fits".into(),
                comment: Some("use for regression coverage".into()),
                solve_is_invalid: true,
                license_version: "validation-image-grant-v2".into(),
                donated_at: "2026-07-15T10:02:00+00:00".into(),
            }],
            vec![StoredClient {
                owner: "client-a".into(),
                last_served_at: "2026-07-15T10:01:00+00:00".into(),
            }],
        )
    }

    #[test]
    fn parses_bidirectional_arguments() {
        let args = MigrationArgs::from_env_and_args(&[
            "--from".into(),
            "dynamodb".into(),
            "--to".into(),
            "sqlx".into(),
            "--sqlx-url".into(),
            "sqlite://jobs.sqlite3".into(),
            "--dynamodb-table".into(),
            "jobs".into(),
            "--dry-run".into(),
            "--resume".into(),
        ])
        .unwrap();
        assert_eq!(args.from, StoreBackend::DynamoDb);
        assert_eq!(args.to, StoreBackend::Sqlx);
        assert!(args.dry_run);
        assert!(args.resume);
    }

    #[tokio::test]
    async fn sqlx_import_round_trips_all_logical_state() {
        let path = env::temp_dir().join(format!("seiza-migration-{}.sqlite3", Uuid::now_v7()));
        let store = SqlxStore::connect(&format!("sqlite://{}?mode=rwc", path.display()), true)
            .await
            .unwrap();
        let expected = sample_snapshot();
        expected.validate().unwrap();
        store.import(&expected).await.unwrap();
        assert_eq!(store.snapshot().await.unwrap(), expected);
    }

    #[tokio::test]
    async fn sqlx_snapshot_rejects_unknown_persisted_columns() {
        let path = env::temp_dir().join(format!("seiza-migration-{}.sqlite3", Uuid::now_v7()));
        let store = SqlxStore::connect(&format!("sqlite://{}?mode=rwc", path.display()), true)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE jobs_v2 ADD COLUMN future_state TEXT")
            .execute(store.repository.pool())
            .await
            .unwrap();
        assert!(store.snapshot().await.is_err());
    }

    #[tokio::test]
    async fn legacy_sql_snapshot_reuses_public_uuid_without_mutating_source() {
        let path = env::temp_dir().join(format!("seiza-migration-{}.sqlite3", Uuid::now_v7()));
        let store = SqlxStore::connect(&format!("sqlite://{}?mode=rwc", path.display()), false)
            .await
            .unwrap();
        for statement in [
            "CREATE TABLE jobs (id BIGINT PRIMARY KEY, owner TEXT NOT NULL, queue_weight DOUBLE PRECISION NOT NULL, object_key TEXT NOT NULL, original_filename TEXT NOT NULL, content_type TEXT, options_json TEXT NOT NULL, status TEXT NOT NULL, created_at TEXT NOT NULL, started_at TEXT, completed_at TEXT, solution_json TEXT, error TEXT, lease_token TEXT, lease_expires_at TEXT, attempts BIGINT NOT NULL DEFAULT 0)",
            "CREATE TABLE validation_donations (job_id BIGINT PRIMARY KEY, object_key TEXT NOT NULL UNIQUE, comment TEXT, solve_is_invalid BIGINT NOT NULL DEFAULT 0, license_version TEXT NOT NULL, donated_at TEXT NOT NULL)",
            "CREATE TABLE client_service (owner TEXT PRIMARY KEY, last_served_at TEXT NOT NULL)",
            "CREATE TABLE queue_outbox (job_id BIGINT PRIMARY KEY, delivered_at TEXT)",
            "CREATE TABLE queue_counters (name TEXT PRIMARY KEY, value BIGINT NOT NULL)",
        ] {
            sqlx::query(statement)
                .execute(store.repository.pool())
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO jobs (id, owner, queue_weight, object_key, original_filename, content_type, options_json, status, created_at, attempts) VALUES (7, 'legacy', 1.0, $1, 'legacy.fits', NULL, '{}', 'queued', '2026-07-15T10:00:00+00:00', 1)")
            .bind(format!("uploads/public-{}/image.fits", sample_id()))
            .execute(store.repository.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO queue_outbox (job_id, delivered_at) VALUES (7, '2026-07-15T10:00:01+00:00')")
            .execute(store.repository.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO queue_counters (name, value) VALUES ('jobs', 7)")
            .execute(store.repository.pool())
            .await
            .unwrap();

        let snapshot = store.snapshot().await.unwrap();
        assert_eq!(snapshot.jobs[0].id, sample_id());
        assert_eq!(snapshot.jobs[0].legacy_id, Some(7));
        assert_eq!(snapshot.jobs[0].astrometry_id, 7);
        assert_eq!(snapshot.jobs[0].notification_delivered_at, None);
        let v2_tables = sqlx::query(
            "SELECT COUNT(*) AS count FROM sqlite_master WHERE type = 'table' AND name = 'jobs_v2'",
        )
        .fetch_one(store.repository.pool())
        .await
        .unwrap()
        .try_get::<i64, _>("count")
        .unwrap();
        assert_eq!(v2_tables, 0);
    }

    #[test]
    fn dynamodb_job_item_round_trips_all_logical_state() {
        let mut snapshot = sample_snapshot();
        let expected_job = snapshot.jobs.remove(0);
        let expected_donation = snapshot.donations.remove(0);
        let item = job_to_item(&expected_job, Some(&expected_donation));
        assert_eq!(job_from_item(&item).unwrap(), expected_job);
        assert_eq!(
            donation_from_item(&item, expected_job.id).unwrap(),
            Some(expected_donation)
        );
    }

    #[test]
    fn converts_legacy_dynamodb_job_to_its_existing_public_uuid() {
        let mut item = job_to_item(&sample_snapshot().jobs[0], None);
        item.insert("pk".into(), string("JOB#7"));
        item.insert("id".into(), number(7));
        item.remove("astrometry_id");
        item.remove("legacy_id");
        item.insert("status".into(), string("queued"));
        item.insert(
            "notification_delivered_at".into(),
            string("2026-07-15T10:00:01+00:00"),
        );
        let converted = job_from_item(&item).unwrap();
        assert_eq!(converted.id, sample_id());
        assert_eq!(converted.astrometry_id, 7);
        assert_eq!(converted.legacy_id, Some(7));
        assert_eq!(converted.notification_delivered_at, None);
    }

    #[test]
    fn dynamodb_snapshot_rejects_unknown_persisted_attributes() {
        let mut item = HashMap::from([("pk".into(), string("JOB#7"))]);
        item.insert("future_state".into(), string("unknown"));
        assert!(ensure_only_attributes(&item, "JOB#7", &["pk"]).is_err());
    }

    #[test]
    fn resume_rejects_different_destination_records() {
        let source = sample_snapshot();
        let mut destination = source.clone();
        destination.jobs[0].attempts += 1;
        assert!(destination.ensure_subset_of(&source).is_err());
    }
}
