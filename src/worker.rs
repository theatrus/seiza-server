use crate::{
    config::Config,
    models::{JobId, JobLease, WorkerCompletion},
    solver::SolverEngine,
};
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use reqwest::{Client, StatusCode};
use std::{env, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerMode {
    Http,
    Sqs,
}

pub struct WorkerArgs {
    pub server: String,
    pub token: String,
    pub mode: WorkerMode,
}

impl WorkerArgs {
    pub fn from_env_and_args(args: &[String]) -> Result<Self> {
        let mut server =
            env::var("SEIZA_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
        let mut token = env::var("SEIZA_WORKER_TOKEN").unwrap_or_default();
        let mut mode = WorkerMode::Http;
        let mut values = args.iter();
        while let Some(flag) = values.next() {
            match flag.as_str() {
                "--server" => server = values.next().context("--server requires a URL")?.clone(),
                "--token" => token = values.next().context("--token requires a value")?.clone(),
                "--mode" => {
                    mode = match values
                        .next()
                        .context("--mode requires `http` or `sqs`")?
                        .as_str()
                    {
                        "http" => WorkerMode::Http,
                        "sqs" => WorkerMode::Sqs,
                        value => bail!("unknown worker mode `{value}`; use `http` or `sqs`"),
                    }
                }
                "--help" | "-h" => bail!(worker_usage()),
                value => bail!("unknown worker option `{value}`\n{}", worker_usage()),
            }
        }
        if token.trim().is_empty() {
            bail!(
                "SEIZA_WORKER_TOKEN or --token is required\n{}",
                worker_usage()
            );
        }
        Ok(Self {
            server: server.trim_end_matches('/').to_owned(),
            token,
            mode,
        })
    }
}

pub fn worker_usage() -> &'static str {
    "usage: seiza-server worker [--server http://api:8080] [--token TOKEN] [--mode http|sqs]"
}

#[derive(Clone)]
struct WorkerClient {
    client: Client,
    server: String,
    token: String,
}

impl WorkerClient {
    fn new(server: String, token: String) -> Self {
        Self {
            client: Client::new(),
            server,
            token,
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{}", self.server, path))
            .bearer_auth(&self.token)
    }

    async fn claim(&self, requested_job_id: Option<JobId>) -> Result<Option<JobLease>> {
        let path = match requested_job_id {
            Some(job_id) => format!("/api/v1/internal/worker/claim/{job_id}"),
            None => "/api/v1/internal/worker/claim".into(),
        };
        let response = self.request(reqwest::Method::POST, &path).send().await?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        Ok(Some(response.error_for_status()?.json().await?))
    }

    async fn input(&self, lease: &JobLease) -> Result<Bytes> {
        Ok(self
            .request(
                reqwest::Method::GET,
                &format!("/api/v1/internal/worker/jobs/{}/input", lease.job_id),
            )
            .header("x-seiza-lease-token", &lease.lease_token)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?)
    }

    async fn heartbeat(&self, lease: &JobLease) -> Result<bool> {
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("/api/v1/internal/worker/jobs/{}/heartbeat", lease.job_id),
            )
            .json(&serde_json::json!({ "lease_token": lease.lease_token }))
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json::<serde_json::Value>().await?["active"]
            .as_bool()
            .unwrap_or(false))
    }

    async fn complete(&self, lease: &JobLease, completion: WorkerCompletion) -> Result<bool> {
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("/api/v1/internal/worker/jobs/{}/complete", lease.job_id),
            )
            .json(&completion)
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json::<serde_json::Value>().await?["accepted"]
            .as_bool()
            .unwrap_or(false))
    }
}

pub async fn run(args: WorkerArgs) -> Result<()> {
    let config = Config::from_env()?;
    let engine = SolverEngine::from_catalog_path(config.catalog_path.as_deref());
    if !engine.is_ready() {
        bail!("worker requires SEIZA_STAR_DATA to point at a Seiza tile catalog");
    }
    let client = WorkerClient::new(args.server, args.token);
    match args.mode {
        WorkerMode::Http => run_http_worker(client, engine).await,
        WorkerMode::Sqs => run_sqs_worker(client, engine).await,
    }
}

async fn run_http_worker(client: WorkerClient, engine: SolverEngine) -> Result<()> {
    tracing::info!(server = %client.server, "remote HTTP queue worker started");
    loop {
        match client.claim(None).await {
            Ok(Some(lease)) => {
                if let Err(error) = process_lease(&client, &engine, lease).await {
                    // Leave the lease to expire rather than terminating the
                    // worker process. Another worker can safely retry it.
                    tracing::warn!(%error, "worker failed while processing lease; continuing");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            Ok(None) => tokio::time::sleep(Duration::from_secs(1)).await,
            Err(error) => {
                tracing::warn!(%error, "worker claim failed; retrying");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

async fn process_lease(
    client: &WorkerClient,
    engine: &SolverEngine,
    lease: JobLease,
) -> Result<bool> {
    let input = client.input(&lease).await?;
    let solve = engine.solve(
        input,
        lease.original_filename.clone(),
        lease.options.clone(),
    );
    tokio::pin!(solve);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(60));
    heartbeat.tick().await;
    let outcome = loop {
        tokio::select! {
            outcome = &mut solve => break outcome,
            _ = heartbeat.tick() => {
                if !client.heartbeat(&lease).await? {
                    bail!("worker lease for job {} is no longer active", lease.job_id);
                }
            }
        }
    };
    let completion = match outcome {
        Ok(solution) => WorkerCompletion {
            lease_token: lease.lease_token.clone(),
            solution: Some(solution),
            error: None,
        },
        Err(error) => WorkerCompletion {
            lease_token: lease.lease_token.clone(),
            solution: None,
            error: Some(format!("{error:#}")),
        },
    };
    client.complete(&lease, completion).await
}

#[cfg(feature = "aws")]
async fn run_sqs_worker(client: WorkerClient, engine: SolverEngine) -> Result<()> {
    let queue_url = env::var("SEIZA_SQS_QUEUE_URL")
        .context("SEIZA_SQS_QUEUE_URL is required for SQS workers")?;
    let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .load()
        .await;
    let sqs = aws_sdk_sqs::Client::new(&sdk_config);
    tracing::info!(queue_url, "direct SQS worker started");
    loop {
        let response = sqs
            .receive_message()
            .queue_url(&queue_url)
            .max_number_of_messages(1)
            .wait_time_seconds(20)
            .visibility_timeout(1_200)
            .send()
            .await
            .context("receiving SQS message")?;
        for message in response.messages() {
            let Some(body) = message.body() else { continue };
            let Some(receipt_handle) = message.receipt_handle() else {
                continue;
            };
            let job_id: JobId = match body.parse() {
                Ok(id) => id,
                Err(_) => {
                    tracing::warn!(body, "discarding invalid SQS job message");
                    sqs.delete_message()
                        .queue_url(&queue_url)
                        .receipt_handle(receipt_handle)
                        .send()
                        .await?;
                    continue;
                }
            };
            let handled = match client.claim(Some(job_id)).await? {
                Some(lease) => process_lease(&client, &engine, lease).await?,
                // A duplicate or an already-completed message is safe to ack.
                None => true,
            };
            if handled {
                sqs.delete_message()
                    .queue_url(&queue_url)
                    .receipt_handle(receipt_handle)
                    .send()
                    .await?;
            }
        }
    }
}

#[cfg(not(feature = "aws"))]
async fn run_sqs_worker(_client: WorkerClient, _engine: SolverEngine) -> Result<()> {
    bail!("SQS worker mode requires `cargo run --features aws -- worker --mode sqs`")
}
