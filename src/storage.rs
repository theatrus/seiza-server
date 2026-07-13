use crate::config::{Config, StorageBackend};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use bytes::Bytes;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Uploaded originals are deliberately separated from job state. Local
/// development writes to disk; production can use the same interface backed
/// by S3, so workers never depend on a shared filesystem.
#[async_trait]
pub trait ObjectStore: Send + Sync {
    async fn put(&self, key: &str, content: Bytes, content_type: Option<&str>) -> Result<()>;
    async fn get(&self, key: &str) -> Result<Bytes>;
}

pub struct LocalObjectStore {
    root: PathBuf,
}

impl LocalObjectStore {
    pub async fn new(root: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&root)
            .await
            .with_context(|| format!("creating local object store {}", root.display()))?;
        Ok(Self { root })
    }

    fn path_for(&self, key: &str) -> Result<PathBuf> {
        let path = Path::new(key);
        if path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            bail!("unsafe object key");
        }
        Ok(self.root.join(path))
    }
}

#[async_trait]
impl ObjectStore for LocalObjectStore {
    async fn put(&self, key: &str, content: Bytes, _content_type: Option<&str>) -> Result<()> {
        let path = self.path_for(key)?;
        let parent = path.parent().context("object key has no parent")?;
        tokio::fs::create_dir_all(parent).await?;
        tokio::fs::write(&path, content)
            .await
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Bytes> {
        let path = self.path_for(key)?;
        tokio::fs::read(&path)
            .await
            .map(Bytes::from)
            .with_context(|| format!("reading {}", path.display()))
    }
}

#[cfg(feature = "aws")]
pub struct S3ObjectStore {
    client: aws_sdk_s3::Client,
    bucket: String,
}

#[cfg(feature = "aws")]
impl S3ObjectStore {
    async fn new(bucket: String) -> Result<Self> {
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        Ok(Self {
            client: aws_sdk_s3::Client::new(&sdk_config),
            bucket,
        })
    }
}

#[cfg(feature = "aws")]
#[async_trait]
impl ObjectStore for S3ObjectStore {
    async fn put(&self, key: &str, content: Bytes, content_type: Option<&str>) -> Result<()> {
        let mut request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(aws_sdk_s3::primitives::ByteStream::from(content));
        if let Some(content_type) = content_type {
            request = request.content_type(content_type);
        }
        request.send().await.context("putting upload in S3")?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Bytes> {
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .context("getting upload from S3")?;
        Ok(Bytes::from(
            response
                .body
                .collect()
                .await
                .context("reading S3 object body")?
                .into_bytes(),
        ))
    }
}

pub async fn object_store(config: &Config) -> Result<Arc<dyn ObjectStore>> {
    match config.storage_backend {
        StorageBackend::Local => Ok(Arc::new(
            LocalObjectStore::new(config.data_dir.join("objects")).await?,
        )),
        StorageBackend::S3 => {
            let bucket = config
                .s3_bucket
                .clone()
                .context("SEIZA_S3_BUCKET is required for S3 storage")?;
            #[cfg(feature = "aws")]
            {
                Ok(Arc::new(S3ObjectStore::new(bucket).await?))
            }
            #[cfg(not(feature = "aws"))]
            {
                let _ = bucket;
                bail!("S3 storage requires `cargo run --features aws`")
            }
        }
    }
}
