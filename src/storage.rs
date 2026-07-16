use crate::config::{Config, StorageBackend};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
#[cfg(feature = "aws")]
use std::time::UNIX_EPOCH;
use std::{
    io::SeekFrom,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredObjectPart {
    pub key: String,
    pub size: u64,
}

/// Uploaded originals are deliberately separated from job state. Local
/// development writes to disk; production can use the same interface backed
/// by S3, so workers never depend on a shared filesystem.
#[async_trait]
pub trait ObjectStore: Send + Sync {
    async fn put(&self, key: &str, content: Bytes, content_type: Option<&str>) -> Result<()>;
    async fn get(&self, key: &str) -> Result<Bytes>;
    async fn get_range(&self, key: &str, start: u64, length: usize) -> Result<Bytes> {
        let content = self.get(key).await?;
        let start = usize::try_from(start).context("object range start does not fit in memory")?;
        if start >= content.len() || length == 0 {
            return Ok(Bytes::new());
        }
        Ok(content.slice(start..start.saturating_add(length).min(content.len())))
    }
    async fn compose(
        &self,
        key: &str,
        parts: &[StoredObjectPart],
        content_type: Option<&str>,
    ) -> Result<()> {
        let content = collect_parts(self, parts).await?;
        self.put(key, content, content_type).await
    }
    async fn exists(&self, key: &str) -> Result<bool>;
    async fn copy(
        &self,
        source_key: &str,
        destination_key: &str,
        content_type: Option<&str>,
    ) -> Result<()> {
        let content = self.get(source_key).await?;
        self.put(destination_key, content, content_type).await
    }
    async fn delete(&self, key: &str) -> Result<()>;
    async fn delete_older_than(
        &self,
        cutoff: SystemTime,
        protected_prefixes: &[String],
    ) -> Result<usize>;
}

async fn collect_parts<S: ObjectStore + ?Sized>(
    store: &S,
    parts: &[StoredObjectPart],
) -> Result<Bytes> {
    let total_size = parts.iter().try_fold(0_u64, |total, part| {
        total
            .checked_add(part.size)
            .context("upload length overflow")
    })?;
    let mut content = Vec::with_capacity(
        usize::try_from(total_size).context("upload is too large to assemble in memory")?,
    );
    for part in parts {
        let bytes = store.get(&part.key).await?;
        if bytes.len() as u64 != part.size {
            bail!("stored upload part {} has the wrong length", part.key);
        }
        content.extend_from_slice(&bytes);
    }
    Ok(Bytes::from(content))
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

    async fn get_range(&self, key: &str, start: u64, length: usize) -> Result<Bytes> {
        if length == 0 {
            return Ok(Bytes::new());
        }
        let path = self.path_for(key)?;
        let mut file = tokio::fs::File::open(&path)
            .await
            .with_context(|| format!("opening {}", path.display()))?;
        file.seek(SeekFrom::Start(start)).await?;
        let mut content = vec![0_u8; length];
        file.read_exact(&mut content).await?;
        Ok(Bytes::from(content))
    }

    async fn compose(
        &self,
        key: &str,
        parts: &[StoredObjectPart],
        _content_type: Option<&str>,
    ) -> Result<()> {
        let destination = self.path_for(key)?;
        let parent = destination.parent().context("object key has no parent")?;
        tokio::fs::create_dir_all(parent).await?;
        let filename = destination
            .file_name()
            .and_then(|value| value.to_str())
            .context("object key has no UTF-8 filename")?;
        let temporary = destination.with_file_name(format!(".{filename}.assembling"));
        let result = async {
            let mut output = tokio::fs::File::create(&temporary).await?;
            for part in parts {
                let source = self.path_for(&part.key)?;
                let mut input = tokio::fs::File::open(&source)
                    .await
                    .with_context(|| format!("opening upload part {}", source.display()))?;
                let copied = tokio::io::copy(&mut input, &mut output).await?;
                if copied != part.size {
                    bail!("stored upload part {} has the wrong length", part.key);
                }
            }
            output.flush().await?;
            output.sync_all().await?;
            drop(output);
            tokio::fs::rename(&temporary, &destination).await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
        }
        result.with_context(|| format!("composing {}", destination.display()))
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        Ok(tokio::fs::try_exists(self.path_for(key)?).await?)
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let path = self.path_for(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("deleting {}", path.display())),
        }
    }

    async fn delete_older_than(
        &self,
        cutoff: SystemTime,
        protected_prefixes: &[String],
    ) -> Result<usize> {
        let root = self.root.clone();
        let protected_prefixes = protected_prefixes.to_vec();
        tokio::task::spawn_blocking(move || sweep_directory(&root, cutoff, &protected_prefixes))
            .await
            .context("local object-store cleanup worker panicked")?
    }
}

fn sweep_directory(
    root: &Path,
    cutoff: SystemTime,
    protected_prefixes: &[String],
) -> Result<usize> {
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
                if !path_is_protected(root, &entry.path(), protected_prefixes) {
                    directories.push(entry.path());
                }
            } else if file_type.is_file()
                && !path_is_protected(root, &entry.path(), protected_prefixes)
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

fn path_is_protected(root: &Path, path: &Path, protected_prefixes: &[String]) -> bool {
    path.strip_prefix(root)
        .ok()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .is_some_and(|key| key_is_protected(&key, protected_prefixes))
}

fn key_is_protected(key: &str, protected_prefixes: &[String]) -> bool {
    protected_prefixes.iter().any(|prefix| {
        let prefix = prefix.trim_matches('/');
        !prefix.is_empty() && (key == prefix || key.starts_with(&format!("{prefix}/")))
    })
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

    async fn get_range(&self, key: &str, start: u64, length: usize) -> Result<Bytes> {
        if length == 0 {
            return Ok(Bytes::new());
        }
        let end = start
            .checked_add(length as u64)
            .and_then(|value| value.checked_sub(1))
            .context("S3 object range overflow")?;
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .range(format!("bytes={start}-{end}"))
            .send()
            .await
            .context("getting S3 upload range")?;
        Ok(response
            .body
            .collect()
            .await
            .context("reading S3 object range")?
            .into_bytes())
    }

    async fn compose(
        &self,
        key: &str,
        parts: &[StoredObjectPart],
        content_type: Option<&str>,
    ) -> Result<()> {
        if !s3_multipart_copy_compatible(parts) {
            let content = collect_parts(self, parts).await?;
            return self.put(key, content, content_type).await;
        }

        let mut create = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key);
        if let Some(content_type) = content_type {
            create = create.content_type(content_type);
        }
        let created = create
            .send()
            .await
            .context("starting multipart S3 upload")?;
        let upload_id = created
            .upload_id()
            .context("S3 multipart upload did not return an upload ID")?
            .to_owned();

        let result = async {
            let mut copies = tokio::task::JoinSet::new();
            for (index, part) in parts.iter().enumerate() {
                let part_number = i32::try_from(index + 1).context("too many S3 upload parts")?;
                let client = self.client.clone();
                let bucket = self.bucket.clone();
                let destination = key.to_owned();
                let source_key = part.key.clone();
                let upload_id = upload_id.clone();
                copies.spawn(async move {
                    let copied = client
                        .upload_part_copy()
                        .bucket(&bucket)
                        .key(&destination)
                        .copy_source(format!("{}/{}", bucket, urlencoding::encode(&source_key)))
                        .upload_id(&upload_id)
                        .part_number(part_number)
                        .send()
                        .await
                        .with_context(|| {
                            format!("copying S3 multipart upload part {source_key}")
                        })?;
                    let etag = copied
                        .copy_part_result()
                        .and_then(|result| result.e_tag())
                        .context("S3 multipart copy did not return an ETag")?;
                    Ok::<_, anyhow::Error>((
                        part_number,
                        aws_sdk_s3::types::CompletedPart::builder()
                            .part_number(part_number)
                            .e_tag(etag)
                            .build(),
                    ))
                });
            }
            let mut completed_parts = Vec::with_capacity(parts.len());
            while let Some(copied) = copies.join_next().await {
                completed_parts.push(copied.context("S3 multipart copy task failed")??);
            }
            completed_parts.sort_by_key(|(part_number, _)| *part_number);
            let completed = aws_sdk_s3::types::CompletedMultipartUpload::builder()
                .set_parts(Some(
                    completed_parts.into_iter().map(|(_, part)| part).collect(),
                ))
                .build();
            self.client
                .complete_multipart_upload()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(&upload_id)
                .multipart_upload(completed)
                .send()
                .await
                .context("completing multipart S3 upload")?;
            Ok(())
        }
        .await;

        if result.is_err()
            && let Err(error) = self
                .client
                .abort_multipart_upload()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(&upload_id)
                .send()
                .await
        {
            tracing::warn!(%error, object_key = key, "could not abort failed multipart S3 upload");
        }
        result
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_not_found()) =>
            {
                Ok(false)
            }
            Err(error) => Err(error).context("checking S3 upload object"),
        }
    }

    async fn copy(
        &self,
        source_key: &str,
        destination_key: &str,
        _content_type: Option<&str>,
    ) -> Result<()> {
        self.client
            .copy_object()
            .bucket(&self.bucket)
            .key(destination_key)
            .copy_source(format!(
                "{}/{}",
                self.bucket,
                urlencoding::encode(source_key)
            ))
            .send()
            .await
            .with_context(|| format!("copying S3 upload {source_key} to {destination_key}"))?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .with_context(|| format!("deleting S3 upload object {key}"))?;
        Ok(())
    }

    async fn delete_older_than(
        &self,
        cutoff: SystemTime,
        protected_prefixes: &[String],
    ) -> Result<usize> {
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
                if key_is_protected(key, protected_prefixes) {
                    continue;
                }
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

#[cfg(feature = "aws")]
fn s3_multipart_copy_compatible(parts: &[StoredObjectPart]) -> bool {
    const MIN_PART_SIZE: u64 = 5 * 1_024 * 1_024;
    const MAX_PART_SIZE: u64 = 5 * 1_024 * 1_024 * 1_024;
    const MAX_PARTS: usize = 10_000;
    !parts.is_empty()
        && parts.len() <= MAX_PARTS
        && parts.iter().enumerate().all(|(index, part)| {
            part.size > 0
                && part.size <= MAX_PART_SIZE
                && (index + 1 == parts.len() || part.size >= MIN_PART_SIZE)
        })
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
            .delete_older_than(SystemTime::now() + std::time::Duration::from_secs(1), &[])
            .await
            .unwrap();

        assert_eq!(removed, 1);
        assert!(store.get("nested/image.fits").await.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn local_store_preserves_protected_validation_objects() {
        let root = std::env::temp_dir().join(format!("seiza-store-{}", uuid::Uuid::now_v7()));
        let store = LocalObjectStore::new(root.clone()).await.unwrap();
        store
            .put("uploads/image.fits", Bytes::from_static(b"temporary"), None)
            .await
            .unwrap();
        store
            .copy("uploads/image.fits", "validation/image.fits", None)
            .await
            .unwrap();

        let removed = store
            .delete_older_than(
                SystemTime::now() + std::time::Duration::from_secs(1),
                &["validation".into()],
            )
            .await
            .unwrap();

        assert_eq!(removed, 1);
        assert!(!store.exists("uploads/image.fits").await.unwrap());
        assert_eq!(
            store.get("validation/image.fits").await.unwrap(),
            b"temporary"[..]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn local_store_reports_and_deletes_individual_objects() {
        let root = std::env::temp_dir().join(format!("seiza-store-{}", uuid::Uuid::now_v7()));
        let store = LocalObjectStore::new(root.clone()).await.unwrap();
        store
            .put("uploads/session/chunk", Bytes::from_static(b"chunk"), None)
            .await
            .unwrap();

        assert!(store.exists("uploads/session/chunk").await.unwrap());
        store.delete("uploads/session/chunk").await.unwrap();
        assert!(!store.exists("uploads/session/chunk").await.unwrap());
        store.delete("uploads/session/chunk").await.unwrap();

        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn local_store_reads_ranges_and_streams_composed_parts() {
        let root = std::env::temp_dir().join(format!("seiza-store-{}", uuid::Uuid::now_v7()));
        let store = LocalObjectStore::new(root.clone()).await.unwrap();
        store
            .put("parts/one", Bytes::from_static(b"abcdef"), None)
            .await
            .unwrap();
        store
            .put("parts/two", Bytes::from_static(b"ghijkl"), None)
            .await
            .unwrap();

        assert_eq!(
            store.get_range("parts/one", 2, 3).await.unwrap(),
            b"cde"[..]
        );
        store
            .compose(
                "uploads/final.fits",
                &[
                    StoredObjectPart {
                        key: "parts/one".into(),
                        size: 6,
                    },
                    StoredObjectPart {
                        key: "parts/two".into(),
                        size: 6,
                    },
                ],
                Some("application/fits"),
            )
            .await
            .unwrap();
        assert_eq!(
            store.get("uploads/final.fits").await.unwrap(),
            b"abcdefghijkl"[..]
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "aws")]
    #[test]
    fn s3_multipart_copy_requires_only_the_final_part_to_be_small() {
        let mib = 1_024 * 1_024;
        assert!(s3_multipart_copy_compatible(&[
            StoredObjectPart {
                key: "one".into(),
                size: 5 * mib,
            },
            StoredObjectPart {
                key: "two".into(),
                size: 5 * mib,
            },
            StoredObjectPart {
                key: "three".into(),
                size: 2 * mib,
            },
        ]));
        assert!(!s3_multipart_copy_compatible(&[
            StoredObjectPart {
                key: "one".into(),
                size: 4 * mib,
            },
            StoredObjectPart {
                key: "two".into(),
                size: 6 * mib,
            },
        ]));
    }
}
