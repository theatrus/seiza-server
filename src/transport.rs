use crate::{
    config::{Config, QueueDelivery},
    models::JobRecord,
};
#[cfg(not(feature = "aws"))]
use anyhow::bail;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;

/// Notification transport for workers that poll an external durable queue.
/// The selected job repository remains the authoritative scheduler and lease
/// store; this transport carries the job ID as its body plus non-authoritative
/// scheduling metadata, making duplicate delivery safe.
#[async_trait]
pub trait QueueTransport: Send + Sync {
    fn uses_external_queue(&self) -> bool;
    async fn publish(&self, job: &JobRecord) -> Result<()>;
}

pub struct LocalTransport;

#[async_trait]
impl QueueTransport for LocalTransport {
    fn uses_external_queue(&self) -> bool {
        false
    }

    async fn publish(&self, _job: &JobRecord) -> Result<()> {
        Ok(())
    }
}

#[cfg(feature = "aws")]
pub struct SqsTransport {
    client: aws_sdk_sqs::Client,
    queue_url: String,
    priority_queue_url: Option<String>,
}

#[cfg(feature = "aws")]
impl SqsTransport {
    async fn new(queue_url: String, priority_queue_url: Option<String>) -> Self {
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        Self {
            client: aws_sdk_sqs::Client::new(&sdk_config),
            queue_url,
            priority_queue_url,
        }
    }

    fn queue_url(&self, job: &JobRecord) -> &str {
        if is_priority(job.queue_weight)
            && let Some(priority_queue_url) = &self.priority_queue_url
        {
            priority_queue_url
        } else {
            &self.queue_url
        }
    }
}

#[cfg(feature = "aws")]
#[async_trait]
impl QueueTransport for SqsTransport {
    fn uses_external_queue(&self) -> bool {
        true
    }

    async fn publish(&self, job: &JobRecord) -> Result<()> {
        let message_group_id = fair_queue_group_id(&job.owner)?;
        self.client
            .send_message()
            .queue_url(self.queue_url(job))
            .message_body(job.id.to_string())
            .message_group_id(message_group_id)
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
                Ok(Arc::new(
                    SqsTransport::new(queue_url, config.sqs_priority_queue_url.clone()).await,
                ))
            }
            #[cfg(not(feature = "aws"))]
            {
                let _ = queue_url;
                bail!("SQS queue transport requires `cargo run --features aws`")
            }
        }
    }
}

fn is_priority(queue_weight: f64) -> bool {
    queue_weight > 1.0
}

fn fair_queue_group_id(owner: &str) -> Result<String> {
    let group_id = format!("seiza:v1:{owner}");
    if group_id.len() > 128
        || !group_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character.is_ascii_punctuation())
    {
        anyhow::bail!("job owner cannot be represented as an SQS MessageGroupId");
    }
    Ok(group_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fair_queue_group_is_stable_and_namespaced() {
        assert_eq!(
            fair_queue_group_id("key:0123456789abcdef").unwrap(),
            "seiza:v1:key:0123456789abcdef"
        );
        assert_eq!(
            fair_queue_group_id("public:2001:db8::1").unwrap(),
            "seiza:v1:public:2001:db8::1"
        );
    }

    #[test]
    fn fair_queue_group_rejects_unbounded_or_whitespace_owners() {
        assert!(fair_queue_group_id("public:not an ip").is_err());
        assert!(fair_queue_group_id(&"x".repeat(120)).is_err());
    }

    #[test]
    fn priority_requires_a_weight_above_one() {
        assert!(!is_priority(1.0));
        assert!(is_priority(1.01));
    }
}
