use crate::config::{Config, StorageBackend};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use bytes::Bytes;
#[cfg(feature = "aws")]
use std::time::UNIX_EPOCH;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

/// Uploaded originals are deliberately separated from job state. Local
/// development writes to disk; production can use the same interface backed
/// by S3, so workers never depend on a shared filesystem.
#[async_trait]
pub trait ObjectStore: Send + Sync {
    async fn put(&self, key: &str, content: Bytes, content_type: Option<&str>) -> Result<()>;
    async fn get(&self, key: &str) -> Result<Bytes>;
    async fn delete_older_than(&self, cutoff: SystemTime) -> Result<usize>;
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

    async fn delete_older_than(&self, cutoff: SystemTime) -> Result<usize> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || sweep_directory(&root, cutoff))
            .await
            .context("local object-store cleanup worker panicked")?
    }
}

fn sweep_directory(root: &Path, cutoff: SystemTime) -> Result<usize> {
    let mut removed = 0;
    let mut directories = vec![root.to_path_buf()];
    let mut visited = Vec::new();
    while let Some(directory) = directories.pop() {
        visited.push(directory.clone());
        for entry in std::fs::read_dir(&directory)
            .with_context(|| format!("reading object-store directory {}", directory.display()))?
        {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                directories.push(entry.path());
            } else if file_type.is_file()
                && entry
                    .metadata()?
                    .modified()
                    .is_ok_and(|modified| modified <= cutoff)
            {
                std::fs::remove_file(entry.path())?;
                removed += 1;
            }
        }
    }
    for directory in visited.into_iter().rev() {
        if directory != root {
            let _ = std::fs::remove_dir(directory);
        }
    }
    Ok(removed)
}

#[cfg(feature = "aws")]
pub struct S3ObjectStore {
    client: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
}

#[cfg(feature = "aws")]
impl S3ObjectStore {
    async fn new(bucket: String, prefix: String) -> Result<Self> {
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        let prefix = prefix.trim_matches('/');
        let prefix = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}/")
        };
        Ok(Self {
            client: aws_sdk_s3::Client::new(&sdk_config),
            bucket,
            prefix,
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
        Ok(response
            .body
            .collect()
            .await
            .context("reading S3 object body")?
            .into_bytes())
    }

    async fn delete_older_than(&self, cutoff: SystemTime) -> Result<usize> {
        let cutoff = cutoff.duration_since(UNIX_EPOCH)?.as_secs() as i64;
        let mut continuation_token = None;
        let mut removed = 0;
        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&self.prefix);
            if let Some(token) = continuation_token.as_deref() {
                request = request.continuation_token(token);
            }
            let response = request.send().await.context("listing uploads in S3")?;
            for object in response.contents() {
                let Some(key) = object.key() else { continue };
                let Some(modified) = object.last_modified() else {
                    continue;
                };
                if modified.secs() <= cutoff {
                    self.client
                        .delete_object()
                        .bucket(&self.bucket)
                        .key(key)
                        .send()
                        .await
                        .with_context(|| format!("deleting expired S3 upload {key}"))?;
                    removed += 1;
                }
            }
            continuation_token = response.next_continuation_token().map(str::to_owned);
            if continuation_token.is_none() {
                break;
            }
        }
        Ok(removed)
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
                Ok(Arc::new(
                    S3ObjectStore::new(bucket, config.s3_prefix.clone()).await?,
                ))
            }
            #[cfg(not(feature = "aws"))]
            {
                let _ = bucket;
                bail!("S3 storage requires `cargo run --features aws`")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_store_sweeps_expired_objects() {
        let root = std::env::temp_dir().join(format!("seiza-store-{}", uuid::Uuid::now_v7()));
        let store = LocalObjectStore::new(root.clone()).await.unwrap();
        store
            .put("nested/image.fits", Bytes::from_static(b"image"), None)
            .await
            .unwrap();

        let removed = store
            .delete_older_than(SystemTime::now() + std::time::Duration::from_secs(1))
            .await
            .unwrap();

        assert_eq!(removed, 1);
        assert!(store.get("nested/image.fits").await.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
