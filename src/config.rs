use anyhow::{Context, Result, bail};
use seiza::data_paths::{self, DataPathError};
use std::{
    env, fmt,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
};
use url::Url;

#[derive(Clone, Default)]
pub(crate) struct PriorityApiKeys(Vec<String>);

impl PriorityApiKeys {
    pub(crate) fn parse(value: Option<String>) -> Self {
        Self(
            value
                .into_iter()
                .flat_map(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|key| !key.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .collect(),
        )
    }

    fn contains(&self, key: &str) -> bool {
        self.0.iter().any(|candidate| candidate == key)
    }

    fn queue_weight(&self, api_key: Option<&str>, priority_weight: usize) -> f64 {
        if api_key.is_some_and(|key| self.contains(key)) {
            priority_weight as f64
        } else {
            1.0
        }
    }
}

impl fmt::Debug for PriorityApiKeys {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PriorityApiKeys")
            .field("count", &self.0.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Public,
    StubApiKey,
    Accounts,
}

impl FromStr for AuthMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "public" => Ok(Self::Public),
            "stub-api-key" | "stub_api_key" => Ok(Self::StubApiKey),
            "accounts" => Ok(Self::Accounts),
            _ => bail!("SEIZA_AUTH_MODE must be `public`, `stub-api-key`, or `accounts`"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailProvider {
    Ses,
    Smtp,
}

impl FromStr for EmailProvider {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ses" => Ok(Self::Ses),
            "smtp" => Ok(Self::Smtp),
            _ => bail!("SEIZA_EMAIL_PROVIDER must be `ses` or `smtp`"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpTls {
    StartTls,
    Implicit,
}

impl FromStr for SmtpTls {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "starttls" => Ok(Self::StartTls),
            "implicit" => Ok(Self::Implicit),
            _ => bail!("SEIZA_SMTP_TLS must be `starttls` or `implicit`"),
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
    pub blind_index_path: Option<PathBuf>,
    pub object_catalog_path: Option<PathBuf>,
    pub star_identifier_catalog_path: Option<PathBuf>,
    pub transient_catalog_path: Option<PathBuf>,
    pub minor_body_catalog_path: Option<PathBuf>,
    pub job_backend: JobBackend,
    pub sql_database_url: String,
    pub dynamodb_table: Option<String>,
    pub identity_backend: JobBackend,
    pub identity_sql_database_url: String,
    pub identity_dynamodb_table: Option<String>,
    pub queue_transport: QueueDelivery,
    pub sqs_queue_url: Option<String>,
    pub sqs_priority_queue_url: Option<String>,
    pub sqs_priority_weight: usize,
    pub(crate) priority_api_keys: PriorityApiKeys,
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
    pub public_base_url: Option<Url>,
    pub auth_code_pepper_file: Option<PathBuf>,
    pub email_provider: Option<EmailProvider>,
    pub email_from: Option<String>,
    pub ses_from_identity_arn: Option<String>,
    pub ses_role_arn: Option<String>,
    pub ses_role_external_id_file: Option<PathBuf>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_username: Option<String>,
    pub smtp_password_file: Option<PathBuf>,
    pub smtp_tls: SmtpTls,
    pub smtp_timeout_seconds: u64,
    pub storage_backend: StorageBackend,
    pub s3_bucket: Option<String>,
    pub s3_prefix: String,
    pub validation_prefix: String,
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
        let sqs_priority_weight = parse_env("SEIZA_SQS_PRIORITY_WEIGHT", 2usize)?;
        if rate_limit_per_minute <= 0.0 || rate_limit_burst < 1.0 {
            bail!("rate limit values must be positive and burst must be at least 1");
        }
        if lease_seconds == 0 {
            bail!("SEIZA_LEASE_SECONDS must be at least 1");
        }
        validate_sqs_priority_weight(sqs_priority_weight)?;
        if upload_retention_seconds == 0 || upload_retention_seconds > i64::MAX as u64 {
            bail!("SEIZA_UPLOAD_RETENTION_SECONDS must be between 1 and i64::MAX");
        }
        if upload_cleanup_interval_seconds == 0 {
            bail!("SEIZA_UPLOAD_CLEANUP_INTERVAL_SECONDS must be at least 1");
        }
        let data_dir = PathBuf::from(env_or("SEIZA_DATA_DIR", "data"));
        let catalog_path = optional_data_path(data_paths::star_data(None))
            .context("resolving Seiza star catalog")?;
        let blind_index_path =
            data_paths::blind_index(None).context("resolving Seiza blind index")?;
        let object_catalog_path =
            optional_catalog_from_env("SEIZA_OBJECT_DATA", data_paths::objects)?;
        let star_identifier_catalog_path =
            optional_catalog_from_env("SEIZA_STAR_IDENTIFIER_DATA", data_paths::star_identifiers)?;
        let transient_catalog_path =
            optional_catalog_from_env("SEIZA_TRANSIENT_DATA", data_paths::transients)?;
        let minor_body_catalog_path =
            optional_catalog_from_env("SEIZA_MINOR_BODY_DATA", data_paths::minor_bodies)?;
        let queue_database = env::var_os("SEIZA_QUEUE_DATABASE")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("jobs.sqlite3"));
        let sql_database_url = env::var("SEIZA_SQL_DATABASE_URL")
            .unwrap_or_else(|_| format!("sqlite://{}?mode=rwc", queue_database.display()));
        let job_backend: JobBackend = env_or("SEIZA_JOB_BACKEND", "sqlx").parse()?;
        let identity_backend = env::var("SEIZA_IDENTITY_BACKEND")
            .map(|value| value.parse())
            .unwrap_or(Ok(job_backend))?;
        let identity_sql_database_url = env::var("SEIZA_IDENTITY_SQL_DATABASE_URL")
            .unwrap_or_else(|_| sql_database_url.clone());
        let identity_dynamodb_table = env::var("SEIZA_IDENTITY_DYNAMODB_TABLE")
            .ok()
            .filter(|value| !value.is_empty());
        let auth_mode: AuthMode = env_or("SEIZA_AUTH_MODE", "public").parse()?;
        if auth_mode == AuthMode::Accounts
            && identity_backend == JobBackend::DynamoDb
            && identity_dynamodb_table.is_none()
        {
            bail!(
                "SEIZA_IDENTITY_DYNAMODB_TABLE is required when accounts use the DynamoDB identity backend"
            );
        }
        let public_base_url = env::var("SEIZA_PUBLIC_BASE_URL")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| validate_public_base_url(&value))
            .transpose()?;
        let auth_code_pepper_file = env::var_os("SEIZA_AUTH_CODE_PEPPER_FILE")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let email_provider = env::var("SEIZA_EMAIL_PROVIDER")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| value.parse())
            .transpose()?;
        let email_from = env::var("SEIZA_EMAIL_FROM")
            .ok()
            .filter(|value| !value.is_empty());
        let ses_from_identity_arn = env::var("SEIZA_SES_FROM_IDENTITY_ARN")
            .ok()
            .filter(|value| !value.is_empty());
        let ses_role_arn = env::var("SEIZA_SES_ROLE_ARN")
            .ok()
            .filter(|value| !value.is_empty());
        let ses_role_external_id_file = env::var_os("SEIZA_SES_ROLE_EXTERNAL_ID_FILE")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let smtp_host = env::var("SEIZA_SMTP_HOST")
            .ok()
            .filter(|value| !value.is_empty());
        let smtp_port = env::var("SEIZA_SMTP_PORT")
            .ok()
            .map(|value| value.parse().context("invalid SEIZA_SMTP_PORT"))
            .transpose()?;
        let smtp_username = env::var("SEIZA_SMTP_USERNAME")
            .ok()
            .filter(|value| !value.is_empty());
        let smtp_password_file = env::var_os("SEIZA_SMTP_PASSWORD_FILE")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let smtp_tls: SmtpTls = env_or("SEIZA_SMTP_TLS", "starttls").parse()?;
        let smtp_timeout_seconds = parse_env("SEIZA_SMTP_TIMEOUT_SECONDS", 30u64)?;
        if smtp_timeout_seconds == 0 {
            bail!("SEIZA_SMTP_TIMEOUT_SECONDS must be at least 1");
        }
        if auth_mode == AuthMode::Accounts {
            public_base_url
                .as_ref()
                .context("SEIZA_PUBLIC_BASE_URL is required when SEIZA_AUTH_MODE=accounts")?;
            if auth_code_pepper_file.is_none() {
                bail!("SEIZA_AUTH_CODE_PEPPER_FILE is required when SEIZA_AUTH_MODE=accounts");
            }
            let provider = email_provider
                .context("SEIZA_EMAIL_PROVIDER is required when SEIZA_AUTH_MODE=accounts")?;
            if email_from.is_none() {
                bail!("SEIZA_EMAIL_FROM is required when SEIZA_AUTH_MODE=accounts");
            }
            match provider {
                EmailProvider::Ses => {}
                EmailProvider::Smtp => {
                    if smtp_host.is_none()
                        || smtp_username.is_none()
                        || smtp_password_file.is_none()
                    {
                        bail!(
                            "SMTP email requires SEIZA_SMTP_HOST, SEIZA_SMTP_USERNAME, and SEIZA_SMTP_PASSWORD_FILE"
                        );
                    }
                }
            }
        }
        let s3_prefix = env_or("SEIZA_S3_PREFIX", "uploads")
            .trim_matches('/')
            .to_owned();
        let validation_prefix = env_or("SEIZA_VALIDATION_PREFIX", "validation")
            .trim_matches('/')
            .to_owned();
        if validation_prefix.is_empty()
            || validation_prefix
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
        {
            bail!("SEIZA_VALIDATION_PREFIX must be a non-empty safe object-key prefix");
        }
        if s3_prefix == validation_prefix
            || (!s3_prefix.is_empty() && s3_prefix.starts_with(&format!("{validation_prefix}/")))
            || (!s3_prefix.is_empty() && validation_prefix.starts_with(&format!("{s3_prefix}/")))
        {
            bail!("SEIZA_VALIDATION_PREFIX and SEIZA_S3_PREFIX must not overlap");
        }

        Ok(Self {
            bind_addr,
            frontend_dir: PathBuf::from(env_or("SEIZA_FRONTEND_DIR", "frontend/dist")),
            data_dir,
            catalog_path,
            blind_index_path,
            object_catalog_path,
            star_identifier_catalog_path,
            transient_catalog_path,
            minor_body_catalog_path,
            job_backend,
            sql_database_url,
            dynamodb_table: env::var("SEIZA_DYNAMODB_TABLE")
                .ok()
                .filter(|value| !value.is_empty()),
            identity_backend,
            identity_sql_database_url,
            identity_dynamodb_table,
            // SEIZA_QUEUE_BACKEND was the original name. It remains an alias
            // so existing local/SQS deployments do not need a flag day.
            queue_transport: env::var("SEIZA_QUEUE_TRANSPORT")
                .or_else(|_| env::var("SEIZA_QUEUE_BACKEND"))
                .unwrap_or_else(|_| "local".to_owned())
                .parse()?,
            sqs_queue_url: env::var("SEIZA_SQS_QUEUE_URL")
                .ok()
                .filter(|value| !value.is_empty()),
            sqs_priority_queue_url: env::var("SEIZA_SQS_PRIORITY_QUEUE_URL")
                .ok()
                .filter(|value| !value.is_empty()),
            sqs_priority_weight,
            priority_api_keys: PriorityApiKeys::parse(env::var("SEIZA_PRIORITY_API_KEYS").ok()),
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
            auth_mode,
            public_base_url,
            auth_code_pepper_file,
            email_provider,
            email_from,
            ses_from_identity_arn,
            ses_role_arn,
            ses_role_external_id_file,
            smtp_host,
            smtp_port,
            smtp_username,
            smtp_password_file,
            smtp_tls,
            smtp_timeout_seconds,
            storage_backend: env_or("SEIZA_STORAGE_BACKEND", "local").parse()?,
            s3_bucket: env::var("SEIZA_S3_BUCKET")
                .ok()
                .filter(|value| !value.is_empty()),
            s3_prefix,
            validation_prefix,
        })
    }

    pub(crate) fn queue_weight_for_api_key(&self, api_key: Option<&str>) -> f64 {
        self.priority_api_keys
            .queue_weight(api_key, self.sqs_priority_weight)
    }

    pub(crate) fn secure_auth_cookies(&self) -> bool {
        self.public_base_url
            .as_ref()
            .is_some_and(|url| url.scheme() == "https")
    }
}

fn validate_public_base_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("invalid SEIZA_PUBLIC_BASE_URL")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        bail!(
            "SEIZA_PUBLIC_BASE_URL must be an origin without credentials, path, query, or fragment"
        );
    }
    if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback_url(&url)) {
        bail!("SEIZA_PUBLIC_BASE_URL must use HTTPS except for localhost development");
    }
    Ok(url)
}

fn is_loopback_url(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("localhost"))
}

fn optional_catalog_from_env(
    variable: &str,
    resolver: fn(Option<&Path>) -> std::result::Result<PathBuf, DataPathError>,
) -> Result<Option<PathBuf>> {
    let explicit = env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    optional_data_path(resolver(explicit.as_deref()))
        .with_context(|| format!("resolving {variable}"))
}

fn optional_data_path(
    result: std::result::Result<PathBuf, DataPathError>,
) -> Result<Option<PathBuf>> {
    match result {
        Ok(path) => Ok(Some(path)),
        Err(DataPathError::NoDefault { .. }) => Ok(None),
        Err(error) => Err(error.into()),
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

fn validate_sqs_priority_weight(weight: usize) -> Result<()> {
    if !(2..=100).contains(&weight) {
        bail!("SEIZA_SQS_PRIORITY_WEIGHT must be between 2 and 100");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seiza_library_discovers_canonical_catalog_names() {
        let directory = std::env::temp_dir().join(format!(
            "seiza-server-catalog-discovery-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let catalog = directory.join("objects.bin");
        let identifiers = directory.join("stars-lite-tycho2.ids.bin");
        std::fs::write(&catalog, b"catalog").unwrap();
        std::fs::write(&identifiers, b"identifiers").unwrap();

        assert_eq!(
            optional_data_path(data_paths::objects(Some(&directory))).unwrap(),
            Some(catalog),
        );
        assert_eq!(
            optional_data_path(data_paths::star_identifiers(Some(&directory))).unwrap(),
            Some(identifiers),
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn seiza_library_prefers_deep_catalog_and_discovers_blind_index() {
        let directory = std::env::temp_dir().join(format!(
            "seiza-server-solver-data-discovery-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let regular = directory.join("stars-gaia.bin");
        let deep = directory.join("stars-deep-gaia17.bin");
        let index = directory.join("blind-gaia16.idx");
        std::fs::write(&regular, b"regular").unwrap();
        std::fs::write(&deep, b"deep").unwrap();
        std::fs::write(&index, b"index").unwrap();

        assert_eq!(
            optional_data_path(data_paths::star_data(Some(&directory))).unwrap(),
            Some(deep)
        );
        assert_eq!(
            data_paths::blind_index(Some(&directory)).unwrap(),
            Some(index)
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn absent_default_catalog_remains_optional() {
        assert_eq!(
            optional_data_path(Err(DataPathError::NoDefault {
                kind: "object catalog"
            }))
            .unwrap(),
            None,
        );
    }

    #[test]
    fn explicit_missing_catalog_is_rejected() {
        let missing = std::env::temp_dir().join(format!(
            "seiza-server-missing-object-catalog-{}",
            uuid::Uuid::now_v7()
        ));
        assert!(data_paths::objects(Some(&missing)).is_err());
    }

    #[test]
    fn priority_api_keys_are_trimmed_and_redacted() {
        let keys = PriorityApiKeys::parse(Some(" first-key,second-key ,, ".into()));
        assert!(keys.contains("first-key"));
        assert!(keys.contains("second-key"));
        assert!(!keys.contains("missing"));
        assert_eq!(keys.queue_weight(Some("first-key"), 2), 2.0);
        assert_eq!(keys.queue_weight(Some("missing"), 2), 1.0);
        assert_eq!(keys.queue_weight(None, 2), 1.0);
        assert_eq!(format!("{keys:?}"), "PriorityApiKeys { count: 2 }");
    }

    #[test]
    fn priority_weight_requires_an_actual_preference() {
        assert!(validate_sqs_priority_weight(1).is_err());
        assert!(validate_sqs_priority_weight(2).is_ok());
        assert!(validate_sqs_priority_weight(100).is_ok());
        assert!(validate_sqs_priority_weight(101).is_err());
    }

    #[test]
    fn accounts_auth_mode_is_explicit() {
        assert_eq!("accounts".parse::<AuthMode>().unwrap(), AuthMode::Accounts);
        assert!("account".parse::<AuthMode>().is_err());
    }

    #[test]
    fn account_base_urls_require_https_except_for_loopback() {
        assert!(validate_public_base_url("https://solve.example.com").is_ok());
        assert!(validate_public_base_url("http://localhost:8080").is_ok());
        assert!(validate_public_base_url("http://127.0.0.1:8080").is_err());
        assert!(validate_public_base_url("http://solve.example.com").is_err());
        assert!(validate_public_base_url("https://solve.example.com/path").is_err());
        assert!(validate_public_base_url("https://user@solve.example.com").is_err());
    }
}
