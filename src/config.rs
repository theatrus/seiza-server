use anyhow::{Context, Result, bail};
use std::{env, net::SocketAddr, path::PathBuf, str::FromStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Public,
    StubApiKey,
}

impl FromStr for AuthMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "public" => Ok(Self::Public),
            "stub-api-key" | "stub_api_key" => Ok(Self::StubApiKey),
            _ => bail!("SEIZA_AUTH_MODE must be `public` or `stub-api-key`"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    Local,
    S3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueDelivery {
    Local,
    Sqs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobBackend {
    Sqlx,
    DynamoDb,
}

impl FromStr for JobBackend {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "sqlx" | "sqlite" | "postgres" | "postgresql" => Ok(Self::Sqlx),
            "dynamodb" | "dynamo" => Ok(Self::DynamoDb),
            _ => bail!("SEIZA_JOB_BACKEND must be `sqlx` or `dynamodb`"),
        }
    }
}

impl FromStr for QueueDelivery {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "local" | "sqlite" | "disk" => Ok(Self::Local),
            "sqs" => Ok(Self::Sqs),
            _ => bail!("SEIZA_QUEUE_TRANSPORT must be `local` or `sqs`"),
        }
    }
}

impl FromStr for StorageBackend {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "s3" => Ok(Self::S3),
            _ => bail!("SEIZA_STORAGE_BACKEND must be `local` or `s3`"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub frontend_dir: PathBuf,
    pub data_dir: PathBuf,
    pub catalog_path: Option<PathBuf>,
    pub object_catalog_path: Option<PathBuf>,
    pub transient_catalog_path: Option<PathBuf>,
    pub minor_body_catalog_path: Option<PathBuf>,
    pub job_backend: JobBackend,
    pub sql_database_url: String,
    pub dynamodb_table: Option<String>,
    pub queue_transport: QueueDelivery,
    pub sqs_queue_url: Option<String>,
    pub embedded_workers: bool,
    pub worker_token: Option<String>,
    pub lease_seconds: u64,
    pub worker_count: usize,
    pub max_upload_bytes: usize,
    pub upload_retention_seconds: u64,
    pub upload_cleanup_interval_seconds: u64,
    pub rate_limit_per_minute: f64,
    pub rate_limit_burst: f64,
    pub auth_mode: AuthMode,
    pub storage_backend: StorageBackend,
    pub s3_bucket: Option<String>,
    pub s3_prefix: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let bind_addr = env_or("SEIZA_BIND_ADDR", "127.0.0.1:8080")
            .parse()
            .context("invalid SEIZA_BIND_ADDR")?;
        let worker_count = parse_env("SEIZA_WORKER_COUNT", 1usize)?;
        if worker_count == 0 {
            bail!("SEIZA_WORKER_COUNT must be at least 1");
        }
        let max_upload_bytes = parse_env("SEIZA_MAX_UPLOAD_BYTES", 100 * 1024 * 1024usize)?;
        let upload_retention_seconds = parse_env("SEIZA_UPLOAD_RETENTION_SECONDS", 86_400u64)?;
        let upload_cleanup_interval_seconds =
            parse_env("SEIZA_UPLOAD_CLEANUP_INTERVAL_SECONDS", 3_600u64)?;
        let rate_limit_per_minute = parse_env("SEIZA_RATE_LIMIT_PER_MINUTE", 6.0f64)?;
        let rate_limit_burst = parse_env("SEIZA_RATE_LIMIT_BURST", 3.0f64)?;
        let embedded_workers = parse_env("SEIZA_EMBEDDED_WORKERS", true)?;
        let lease_seconds = parse_env("SEIZA_LEASE_SECONDS", 900u64)?;
        if rate_limit_per_minute <= 0.0 || rate_limit_burst < 1.0 {
            bail!("rate limit values must be positive and burst must be at least 1");
        }
        if lease_seconds == 0 {
            bail!("SEIZA_LEASE_SECONDS must be at least 1");
        }
        if upload_retention_seconds == 0 || upload_retention_seconds > i64::MAX as u64 {
            bail!("SEIZA_UPLOAD_RETENTION_SECONDS must be between 1 and i64::MAX");
        }
        if upload_cleanup_interval_seconds == 0 {
            bail!("SEIZA_UPLOAD_CLEANUP_INTERVAL_SECONDS must be at least 1");
        }
        let data_dir = PathBuf::from(env_or("SEIZA_DATA_DIR", "data"));
        let queue_database = env::var_os("SEIZA_QUEUE_DATABASE")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("jobs.sqlite3"));
        let sql_database_url = env::var("SEIZA_SQL_DATABASE_URL")
            .unwrap_or_else(|_| format!("sqlite://{}?mode=rwc", queue_database.display()));

        Ok(Self {
            bind_addr,
            frontend_dir: PathBuf::from(env_or("SEIZA_FRONTEND_DIR", "frontend/dist")),
            data_dir,
            catalog_path: env::var_os("SEIZA_STAR_DATA").map(PathBuf::from),
            object_catalog_path: env::var_os("SEIZA_OBJECT_DATA").map(PathBuf::from),
            transient_catalog_path: env::var_os("SEIZA_TRANSIENT_DATA").map(PathBuf::from),
            minor_body_catalog_path: env::var_os("SEIZA_MINOR_BODY_DATA").map(PathBuf::from),
            job_backend: env_or("SEIZA_JOB_BACKEND", "sqlx").parse()?,
            sql_database_url,
            dynamodb_table: env::var("SEIZA_DYNAMODB_TABLE")
                .ok()
                .filter(|value| !value.is_empty()),
            // SEIZA_QUEUE_BACKEND was the original name. It remains an alias
            // so existing local/SQS deployments do not need a flag day.
            queue_transport: env::var("SEIZA_QUEUE_TRANSPORT")
                .or_else(|_| env::var("SEIZA_QUEUE_BACKEND"))
                .unwrap_or_else(|_| "local".to_owned())
                .parse()?,
            sqs_queue_url: env::var("SEIZA_SQS_QUEUE_URL")
                .ok()
                .filter(|value| !value.is_empty()),
            embedded_workers,
            worker_token: env::var("SEIZA_WORKER_TOKEN")
                .ok()
                .filter(|value| !value.is_empty()),
            lease_seconds,
            worker_count,
            max_upload_bytes,
            upload_retention_seconds,
            upload_cleanup_interval_seconds,
            rate_limit_per_minute,
            rate_limit_burst,
            auth_mode: env_or("SEIZA_AUTH_MODE", "public").parse()?,
            storage_backend: env_or("SEIZA_STORAGE_BACKEND", "local").parse()?,
            s3_bucket: env::var("SEIZA_S3_BUCKET")
                .ok()
                .filter(|value| !value.is_empty()),
            s3_prefix: env_or("SEIZA_S3_PREFIX", "uploads")
                .trim_matches('/')
                .to_owned(),
        })
    }
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn parse_env<T>(key: &str, default: T) -> Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(key) {
        Ok(value) => value
            .parse()
            .map_err(|error| anyhow::anyhow!("invalid {key}: {error}")),
        Err(_) => Ok(default),
    }
}
