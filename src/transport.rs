use crate::config::{Config, QueueDelivery};
#[cfg(not(feature = "aws"))]
use anyhow::bail;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;

/// Notification transport for workers that poll an external durable queue.
/// The selected job repository remains the authoritative scheduler and lease
/// store; this transport only carries a job ID, making duplicate delivery safe.
#[async_trait]
pub trait QueueTransport: Send + Sync {
    fn uses_external_queue(&self) -> bool;
    async fn publish(&self, job_id: u64) -> Result<()>;
}

pub struct LocalTransport;

#[async_trait]
impl QueueTransport for LocalTransport {
    fn uses_external_queue(&self) -> bool {
        false
    }

    async fn publish(&self, _job_id: u64) -> Result<()> {
        Ok(())
    }
}

#[cfg(feature = "aws")]
pub struct SqsTransport {
    client: aws_sdk_sqs::Client,
    queue_url: String,
}

#[cfg(feature = "aws")]
impl SqsTransport {
    async fn new(queue_url: String) -> Self {
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        Self {
            client: aws_sdk_sqs::Client::new(&sdk_config),
            queue_url,
        }
    }
}

#[cfg(feature = "aws")]
#[async_trait]
impl QueueTransport for SqsTransport {
    fn uses_external_queue(&self) -> bool {
        true
    }

    async fn publish(&self, job_id: u64) -> Result<()> {
        self.client
            .send_message()
            .queue_url(&self.queue_url)
            .message_body(job_id.to_string())
            .send()
            .await
            .context("publishing job ID to SQS")?;
        Ok(())
    }
}

pub async fn queue_transport(config: &Config) -> Result<Arc<dyn QueueTransport>> {
    match config.queue_transport {
        QueueDelivery::Local => Ok(Arc::new(LocalTransport)),
        QueueDelivery::Sqs => {
            let queue_url = config
                .sqs_queue_url
                .clone()
                .context("SEIZA_SQS_QUEUE_URL is required for SQS queue transport")?;
            #[cfg(feature = "aws")]
            {
                Ok(Arc::new(SqsTransport::new(queue_url).await))
            }
            #[cfg(not(feature = "aws"))]
            {
                let _ = queue_url;
                bail!("SQS queue transport requires `cargo run --features aws`")
            }
        }
    }
}
