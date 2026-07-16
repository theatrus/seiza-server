use crate::{
    models::JobStatus,
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
            _ => bail!("unknown store backend `{value}`; use `sqlx` or `dynamodb`"),
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
                    "unknown migrate-store option `{value}`\n{}",
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
    "usage: seiza-server migrate-store --from sqlx|dynamodb --to sqlx|dynamodb \\\n       [--sqlx-url URL] [--dynamodb-table TABLE] [--dry-run] [--resume]"
}

pub async fn run(args: MigrationArgs) -> Result<()> {
    let sqlx = MigrationStore::Sqlx(SqlxStore::connect(&args.sqlx_url).await?);
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
        "{} -> {}: {} jobs, {} validation donations, {} client fairness records, counter {}",
        args.from.label(),
        args.to.label(),
        source_snapshot.jobs.len(),
        source_snapshot.donations.len(),
        source_snapshot.clients.len(),
        source_snapshot.counter
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
            "destination verification failed: source has {} jobs/{} donations/{} clients/counter {}, destination has {} jobs/{} donations/{} clients/counter {}",
            source_snapshot.jobs.len(),
            source_snapshot.donations.len(),
            source_snapshot.clients.len(),
            source_snapshot.counter,
            verified.jobs.len(),
            verified.donations.len(),
            verified.clients.len(),
            verified.counter
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
    counter: u64,
    jobs: Vec<StoredJob>,
    donations: Vec<StoredDonation>,
    clients: Vec<StoredClient>,
}

impl StoreSnapshot {
    fn new(
        counter: u64,
        mut jobs: Vec<StoredJob>,
        mut donations: Vec<StoredDonation>,
        mut clients: Vec<StoredClient>,
    ) -> Self {
        jobs.sort_by_key(|job| job.id);
        donations.sort_by_key(|donation| donation.job_id);
        clients.sort_by(|left, right| left.owner.cmp(&right.owner));
        Self {
            counter,
            jobs,
            donations,
            clients,
        }
    }

    fn is_empty(&self) -> bool {
        self.counter == 0
            && self.jobs.is_empty()
            && self.donations.is_empty()
            && self.clients.is_empty()
    }

    fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        let mut object_keys = HashSet::new();
        let mut max_id = 0;
        for job in &self.jobs {
            if !ids.insert(job.id) {
                bail!("store contains duplicate job ID {}", job.id);
            }
            if !object_keys.insert(job.object_key.as_str()) {
                bail!("store contains duplicate object key `{}`", job.object_key);
            }
            if job.id > i64::MAX as u64 {
                bail!("job ID {} exceeds SQL BIGINT range", job.id);
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
            max_id = max_id.max(job.id);
        }
        if self.counter < max_id {
            bail!(
                "job counter {} is lower than maximum job ID {max_id}",
                self.counter
            );
        }

        let mut donated_jobs = HashSet::new();
        let mut donation_keys = HashSet::new();
        for donation in &self.donations {
            if !ids.contains(&donation.job_id) {
                bail!(
                    "validation donation references missing job {}",
                    donation.job_id
                );
            }
            if !donated_jobs.insert(donation.job_id) {
                bail!(
                    "store contains duplicate validation donations for job {}",
                    donation.job_id
                );
            }
            if !donation_keys.insert(donation.object_key.as_str()) {
                bail!(
                    "store contains duplicate validation object key `{}`",
                    donation.object_key
                );
            }
            parse_time(&donation.donated_at).with_context(|| {
                format!(
                    "validation donation for job {} has invalid donated_at",
                    donation.job_id
                )
            })?;
        }

        let mut owners = HashSet::new();
        for client in &self.clients {
            if !owners.insert(client.owner.as_str()) {
                bail!("store contains duplicate client `{}`", client.owner);
            }
            parse_time(&client.last_served_at)
                .with_context(|| format!("client `{}` has invalid last_served_at", client.owner))?;
        }
        Ok(())
    }

    fn ensure_subset_of(&self, source: &Self) -> Result<()> {
        if self.counter > source.counter {
            bail!(
                "destination counter {} is ahead of source counter {}",
                self.counter,
                source.counter
            );
        }
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
                    "destination validation donation for job {} differs from the source",
                    donation.job_id
                ),
                None => bail!(
                    "destination validation donation for job {} does not exist in the source",
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
                    "destination client fairness record `{}` differs from the source",
                    client.owner
                ),
                None => bail!(
                    "destination client fairness record `{}` does not exist in the source",
                    client.owner
                ),
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
struct StoredJob {
    id: u64,
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
    job_id: u64,
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
    async fn connect(database_url: &str) -> Result<Self> {
        Ok(Self {
            repository: SqlxJobRepository::connect(database_url).await?,
        })
    }

    async fn snapshot(&self) -> Result<StoreSnapshot> {
        let mut transaction = self.repository.pool().begin().await?;
        if self.repository.dialect() == SqlDialect::Postgres {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
                .execute(&mut *transaction)
                .await?;
        }
        ensure_sql_schema_supported(&mut transaction, self.repository.dialect()).await?;

        let rows = sqlx::query(
            "SELECT j.*, q.job_id AS outbox_job_id, q.delivered_at AS notification_delivered_at FROM jobs j LEFT JOIN queue_outbox q ON q.job_id = j.id ORDER BY j.id",
        )
        .fetch_all(&mut *transaction)
        .await?;
        let mut jobs = Vec::with_capacity(rows.len());
        for row in rows {
            let id = from_i64(row.try_get("id")?, "job ID")?;
            if row.try_get::<Option<i64>, _>("outbox_job_id")?.is_none() {
                bail!("SQLx job {id} has no queue_outbox record");
            }
            jobs.push(StoredJob {
                id,
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

        let orphan_outbox = sqlx::query(
            "SELECT COUNT(*) AS count FROM queue_outbox q LEFT JOIN jobs j ON j.id = q.job_id WHERE j.id IS NULL",
        )
        .fetch_one(&mut *transaction)
        .await?
        .try_get::<i64, _>("count")?;
        if orphan_outbox != 0 {
            bail!("SQLx store contains {orphan_outbox} orphaned queue_outbox records");
        }

        let donations = sqlx::query(
            "SELECT job_id, object_key, comment, solve_is_invalid, license_version, donated_at FROM validation_donations ORDER BY job_id",
        )
        .fetch_all(&mut *transaction)
        .await?
        .into_iter()
        .map(|row| {
            Ok(StoredDonation {
                job_id: from_i64(row.try_get("job_id")?, "validation donation job ID")?,
                object_key: row.try_get("object_key")?,
                comment: row.try_get("comment")?,
                solve_is_invalid: row.try_get::<i64, _>("solve_is_invalid")? != 0,
                license_version: row.try_get("license_version")?,
                donated_at: row.try_get("donated_at")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

        let clients =
            sqlx::query("SELECT owner, last_served_at FROM client_service ORDER BY owner")
                .fetch_all(&mut *transaction)
                .await?
                .into_iter()
                .map(|row| {
                    Ok(StoredClient {
                        owner: row.try_get("owner")?,
                        last_served_at: row.try_get("last_served_at")?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
        let counter = from_i64(
            sqlx::query("SELECT value FROM queue_counters WHERE name = 'jobs'")
                .fetch_one(&mut *transaction)
                .await?
                .try_get("value")?,
            "job counter",
        )?;
        transaction.commit().await?;

        Ok(StoreSnapshot::new(counter, jobs, donations, clients))
    }

    async fn import(&self, snapshot: &StoreSnapshot) -> Result<()> {
        let mut transaction = self.repository.pool().begin().await?;
        for job in &snapshot.jobs {
            sqlx::query(
                "INSERT INTO jobs (id, owner, queue_weight, object_key, original_filename, content_type, options_json, status, created_at, started_at, completed_at, solution_json, error, lease_token, lease_expires_at, attempts) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) ON CONFLICT(id) DO UPDATE SET owner = EXCLUDED.owner, queue_weight = EXCLUDED.queue_weight, object_key = EXCLUDED.object_key, original_filename = EXCLUDED.original_filename, content_type = EXCLUDED.content_type, options_json = EXCLUDED.options_json, status = EXCLUDED.status, created_at = EXCLUDED.created_at, started_at = EXCLUDED.started_at, completed_at = EXCLUDED.completed_at, solution_json = EXCLUDED.solution_json, error = EXCLUDED.error, lease_token = EXCLUDED.lease_token, lease_expires_at = EXCLUDED.lease_expires_at, attempts = EXCLUDED.attempts",
            )
            .bind(to_i64(job.id, "job ID")?)
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
            .await
            .with_context(|| format!("importing SQLx job {}", job.id))?;
            sqlx::query(
                "INSERT INTO queue_outbox (job_id, delivered_at) VALUES ($1, $2) ON CONFLICT(job_id) DO UPDATE SET delivered_at = EXCLUDED.delivered_at",
            )
            .bind(to_i64(job.id, "job ID")?)
            .bind(&job.notification_delivered_at)
            .execute(&mut *transaction)
            .await?;
        }
        for donation in &snapshot.donations {
            sqlx::query(
                "INSERT INTO validation_donations (job_id, object_key, comment, solve_is_invalid, license_version, donated_at) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT(job_id) DO UPDATE SET object_key = EXCLUDED.object_key, comment = EXCLUDED.comment, solve_is_invalid = EXCLUDED.solve_is_invalid, license_version = EXCLUDED.license_version, donated_at = EXCLUDED.donated_at",
            )
            .bind(to_i64(donation.job_id, "validation donation job ID")?)
            .bind(&donation.object_key)
            .bind(&donation.comment)
            .bind(i64::from(donation.solve_is_invalid))
            .bind(&donation.license_version)
            .bind(&donation.donated_at)
            .execute(&mut *transaction)
            .await
            .with_context(|| {
                format!(
                    "importing SQLx validation donation for job {}",
                    donation.job_id
                )
            })?;
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
        sqlx::query("UPDATE queue_counters SET value = $1 WHERE name = 'jobs'")
            .bind(to_i64(snapshot.counter, "job counter")?)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }
}

struct DynamoStore {
    client: Client,
    table: String,
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
        let items = self.scan_all().await?;
        let mut counter = None;
        let mut jobs = Vec::new();
        let mut donations = Vec::new();
        let mut clients = Vec::new();
        let mut object_indices = HashMap::new();

        for item in items {
            let pk = required_string(&item, "pk")?;
            if pk == "COUNTER#jobs" {
                ensure_only_attributes(&item, &pk, &["pk", "value"])?;
                if counter.replace(required_u64(&item, "value")?).is_some() {
                    bail!("DynamoDB store contains duplicate job counters");
                }
                continue;
            }
            match optional_string(&item, "entity").as_deref() {
                Some("job") => {
                    ensure_only_attributes(
                        &item,
                        &pk,
                        &[
                            "pk",
                            "entity",
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
                    let object_key = pk
                        .strip_prefix("OBJECT#")
                        .context("DynamoDB object index has an invalid partition key")?;
                    object_indices.insert(object_key.to_owned(), required_u64(&item, "job_id")?);
                }
                entity => bail!(
                    "DynamoDB table contains unsupported item `{pk}` with entity {:?}",
                    entity
                ),
            }
        }

        if counter.is_none() && !jobs.is_empty() {
            bail!("DynamoDB store contains jobs but no COUNTER#jobs item");
        }
        let jobs_by_id: HashMap<_, _> = jobs.iter().map(|job| (job.id, job)).collect();
        for (object_key, job_id) in object_indices {
            let job = jobs_by_id.get(&job_id).with_context(|| {
                format!("DynamoDB object index `{object_key}` references missing job {job_id}")
            })?;
            if job.object_key != object_key {
                bail!(
                    "DynamoDB object index `{object_key}` does not match job {job_id} object key `{}`",
                    job.object_key
                );
            }
        }

        Ok(StoreSnapshot::new(
            counter.unwrap_or(0),
            jobs,
            donations,
            clients,
        ))
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
                .with_context(|| format!("scanning DynamoDB table `{}`", self.table))?;
            items.extend(output.items().iter().cloned());
            start_key = output.last_evaluated_key().cloned();
            if start_key.is_none() {
                return Ok(items);
            }
        }
    }

    async fn import(&self, snapshot: &StoreSnapshot, resume: bool) -> Result<()> {
        self.put_item(
            HashMap::from([
                ("pk".into(), string("COUNTER#jobs")),
                ("value".into(), number(snapshot.counter)),
            ]),
            false,
        )
        .await
        .context("importing DynamoDB job counter")?;

        let donations: HashMap<_, _> = snapshot
            .donations
            .iter()
            .map(|donation| (donation.job_id, donation))
            .collect();
        for job in &snapshot.jobs {
            let mut job_put = Put::builder()
                .table_name(&self.table)
                .set_item(Some(job_to_item(job, donations.get(&job.id).copied())));
            let mut index_put =
                Put::builder()
                    .table_name(&self.table)
                    .set_item(Some(HashMap::from([
                        ("pk".into(), string(object_index_key(&job.object_key))),
                        ("entity".into(), string("object_index")),
                        ("job_id".into(), number(job.id)),
                    ])));
            if !resume {
                job_put = job_put.condition_expression("attribute_not_exists(pk)");
                index_put = index_put.condition_expression("attribute_not_exists(pk)");
            }
            self.client
                .transact_write_items()
                .transact_items(TransactWriteItem::builder().put(job_put.build()?).build())
                .transact_items(TransactWriteItem::builder().put(index_put.build()?).build())
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
                format!(
                    "importing DynamoDB client fairness record `{}`",
                    client.owner
                )
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

fn job_from_item(item: &DynamoItem) -> Result<StoredJob> {
    let id = required_u64(item, "id")?;
    let pk = required_string(item, "pk")?;
    if pk != job_key(id) {
        bail!("DynamoDB job {id} has invalid partition key `{pk}`");
    }
    Ok(StoredJob {
        id,
        owner: required_string(item, "owner")?,
        queue_weight: required_number(item, "queue_weight")?.parse()?,
        object_key: required_string(item, "object_key")?,
        original_filename: required_string(item, "original_filename")?,
        content_type: optional_string(item, "content_type"),
        options_json: required_string(item, "options_json")?,
        status: required_string(item, "status")?,
        created_at: required_string(item, "created_at")?,
        started_at: optional_string(item, "started_at"),
        completed_at: optional_string(item, "completed_at"),
        solution_json: optional_string(item, "solution_json"),
        error: optional_string(item, "error"),
        lease_token: optional_string(item, "lease_token"),
        lease_expires_at: optional_string(item, "lease_expires_at"),
        attempts: required_u64(item, "attempts")?,
        notification_delivered_at: optional_string(item, "notification_delivered_at"),
    })
}

fn donation_from_item(item: &DynamoItem, job_id: u64) -> Result<Option<StoredDonation>> {
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
            bail!("DynamoDB job {job_id} has incomplete validation donation metadata");
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
        ("id".into(), number(job.id)),
        ("owner".into(), string(&job.owner)),
        ("queue_weight".into(), number(job.queue_weight)),
        ("object_key".into(), string(&job.object_key)),
        ("original_filename".into(), string(&job.original_filename)),
        ("options_json".into(), string(&job.options_json)),
        ("status".into(), string(&job.status)),
        ("created_at".into(), string(&job.created_at)),
        ("attempts".into(), number(job.attempts)),
    ]);
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

fn job_key(job_id: u64) -> String {
    format!("JOB#{job_id}")
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

fn ensure_only_attributes(item: &DynamoItem, pk: &str, supported: &[&str]) -> Result<()> {
    let mut unknown = item
        .keys()
        .filter(|name| !supported.contains(&name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    unknown.sort();
    if !unknown.is_empty() {
        bail!(
            "DynamoDB item `{pk}` has unsupported attributes: {}",
            unknown.join(", ")
        );
    }
    Ok(())
}

fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

async fn ensure_sql_schema_supported(
    connection: &mut AnyConnection,
    dialect: SqlDialect,
) -> Result<()> {
    for (table, supported) in [
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
            ][..],
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
    ] {
        let rows = match dialect {
            // `table` only comes from the fixed internal schema list above.
            SqlDialect::Sqlite => sqlx::query(AssertSqlSafe(format!(
                "PRAGMA table_info({table})"
            )))
                .fetch_all(&mut *connection)
                .await?,
            SqlDialect::Postgres => sqlx::query(
                "SELECT column_name AS name FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = $1",
            )
            .bind(table)
            .fetch_all(&mut *connection)
            .await?,
        };
        let actual = rows
            .into_iter()
            .map(|row| row.try_get::<String, _>("name"))
            .collect::<std::result::Result<HashSet<_>, _>>()?;
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
                "SQLx table `{table}` has an unsupported schema (unknown columns: {}; missing columns: {})",
                unsupported.join(", "),
                missing.join(", ")
            );
        }
    }
    Ok(())
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
    use uuid::Uuid;

    fn sample_snapshot() -> StoreSnapshot {
        StoreSnapshot::new(
            7,
            vec![StoredJob {
                id: 7,
                owner: "client-a".into(),
                queue_weight: 1.5,
                object_key: "uploads/example.fits".into(),
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
                job_id: 7,
                object_key: "validation/example.fits".into(),
                comment: Some("use for regression coverage".into()),
                solve_is_invalid: true,
                license_version: "CC0-1.0".into(),
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
        let store = SqlxStore::connect(&format!("sqlite://{}?mode=rwc", path.display()))
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
        let store = SqlxStore::connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .unwrap();
        sqlx::query("ALTER TABLE jobs ADD COLUMN future_state TEXT")
            .execute(store.repository.pool())
            .await
            .unwrap();
        assert!(store.snapshot().await.is_err());
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
