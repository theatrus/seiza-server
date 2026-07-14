use crate::{
    models::{JobId, SolveOptions},
    storage::ObjectStore,
};
use anyhow::Context;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

pub const TUS_VERSION: &str = "1.0.0";
pub const TUS_EXTENSIONS: &str = "creation,termination";

#[derive(Debug, Error)]
pub enum ResumableUploadError {
    #[error("upload session not found")]
    NotFound,
    #[error("upload offset mismatch: expected {expected}, received {actual}")]
    OffsetMismatch { expected: u64, actual: u64 },
    #[error("upload chunk would exceed the declared file length")]
    ExceedsLength,
    #[error("upload is incomplete: received {offset} of {total} bytes")]
    Incomplete { offset: u64, total: u64 },
    #[error("upload has already completed")]
    Completed,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UploadChunk {
    key: String,
    size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumableUpload {
    pub id: String,
    pub original_filename: String,
    pub content_type: Option<String>,
    pub total_size: u64,
    pub offset: u64,
    pub object_key: String,
    pub options: SolveOptions,
    pub owner: String,
    pub queue_weight: f64,
    pub created_at: DateTime<Utc>,
    pub job_id: Option<JobId>,
    chunks: Vec<UploadChunk>,
}

impl ResumableUpload {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        original_filename: String,
        content_type: Option<String>,
        total_size: u64,
        object_key: String,
        options: SolveOptions,
        owner: String,
        queue_weight: f64,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            original_filename,
            content_type,
            total_size,
            offset: 0,
            object_key,
            options,
            owner,
            queue_weight,
            created_at: Utc::now(),
            job_id: None,
            chunks: Vec::new(),
        }
    }

    pub async fn load(
        store: &Arc<dyn ObjectStore>,
        storage_prefix: &str,
        id: &str,
    ) -> Result<Self, ResumableUploadError> {
        if Uuid::parse_str(id).is_err() {
            return Err(ResumableUploadError::NotFound);
        }
        let key = state_key(storage_prefix, id);
        if !store.exists(&key).await? {
            return Err(ResumableUploadError::NotFound);
        }
        let bytes = store.get(&key).await?;
        serde_json::from_slice(&bytes)
            .context("parsing resumable upload state")
            .map_err(Into::into)
    }

    pub async fn save(
        &self,
        store: &Arc<dyn ObjectStore>,
        storage_prefix: &str,
    ) -> Result<(), ResumableUploadError> {
        let bytes = serde_json::to_vec(self).context("serializing resumable upload state")?;
        store
            .put(
                &state_key(storage_prefix, &self.id),
                Bytes::from(bytes),
                Some("application/json"),
            )
            .await?;
        Ok(())
    }

    pub async fn append(
        &mut self,
        store: &Arc<dyn ObjectStore>,
        storage_prefix: &str,
        offset: u64,
        data: Bytes,
    ) -> Result<u64, ResumableUploadError> {
        if self.job_id.is_some() {
            return Err(ResumableUploadError::Completed);
        }
        if offset != self.offset {
            return Err(ResumableUploadError::OffsetMismatch {
                expected: self.offset,
                actual: offset,
            });
        }
        let size = u64::try_from(data.len()).map_err(anyhow::Error::from)?;
        let new_offset = offset
            .checked_add(size)
            .filter(|new_offset| *new_offset <= self.total_size)
            .ok_or(ResumableUploadError::ExceedsLength)?;
        if size == 0 && new_offset < self.total_size {
            return Err(ResumableUploadError::Incomplete {
                offset: new_offset,
                total: self.total_size,
            });
        }
        let key = chunk_key(storage_prefix, &self.id, offset);
        store
            .put(&key, data, Some("application/offset+octet-stream"))
            .await?;
        self.chunks.push(UploadChunk { key, size });
        self.offset = new_offset;
        self.save(store, storage_prefix).await?;
        Ok(new_offset)
    }

    pub async fn assemble(
        &self,
        store: &Arc<dyn ObjectStore>,
    ) -> Result<Bytes, ResumableUploadError> {
        if self.offset != self.total_size {
            return Err(ResumableUploadError::Incomplete {
                offset: self.offset,
                total: self.total_size,
            });
        }
        let capacity = usize::try_from(self.total_size).map_err(anyhow::Error::from)?;
        let mut output = Vec::with_capacity(capacity);
        for chunk in &self.chunks {
            let bytes = store.get(&chunk.key).await?;
            if u64::try_from(bytes.len()).map_err(anyhow::Error::from)? != chunk.size {
                return Err(anyhow::anyhow!("stored upload chunk has the wrong length").into());
            }
            output.extend_from_slice(&bytes);
        }
        if output.len() != capacity {
            return Err(anyhow::anyhow!("assembled upload has the wrong length").into());
        }
        Ok(Bytes::from(output))
    }

    pub async fn cleanup_chunks(&mut self, store: &Arc<dyn ObjectStore>) {
        let chunks = std::mem::take(&mut self.chunks);
        for chunk in chunks {
            if let Err(error) = store.delete(&chunk.key).await {
                tracing::warn!(key = %chunk.key, %error, "could not remove completed upload chunk");
            }
        }
    }

    pub async fn terminate(
        &self,
        store: &Arc<dyn ObjectStore>,
        storage_prefix: &str,
    ) -> Result<(), ResumableUploadError> {
        if self.job_id.is_some() {
            return Err(ResumableUploadError::Completed);
        }
        for chunk in &self.chunks {
            store.delete(&chunk.key).await?;
        }
        store.delete(&state_key(storage_prefix, &self.id)).await?;
        Ok(())
    }
}

fn upload_namespace(storage_prefix: &str, id: &str) -> String {
    let prefix = storage_prefix.trim_matches('/');
    if prefix.is_empty() {
        format!(".resumable/{id}")
    } else {
        format!("{prefix}/.resumable/{id}")
    }
}

fn state_key(storage_prefix: &str, id: &str) -> String {
    format!("{}/state.json", upload_namespace(storage_prefix, id))
}

fn chunk_key(storage_prefix: &str, id: &str, offset: u64) -> String {
    format!(
        "{}/chunks/{offset:020}",
        upload_namespace(storage_prefix, id)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::LocalObjectStore;

    #[tokio::test]
    async fn persists_and_resumes_chunks_by_offset() {
        let root = std::env::temp_dir().join(format!("seiza-upload-{}", Uuid::now_v7()));
        let store: Arc<dyn ObjectStore> =
            Arc::new(LocalObjectStore::new(root.clone()).await.unwrap());
        let mut upload = ResumableUpload::new(
            "field.fits".into(),
            Some("application/fits".into()),
            8,
            "uploads/field.fits".into(),
            SolveOptions::default(),
            "public:test".into(),
            1.0,
        );
        upload.save(&store, "uploads").await.unwrap();
        upload
            .append(&store, "uploads", 0, Bytes::from_static(b"abcd"))
            .await
            .unwrap();

        let mut resumed = ResumableUpload::load(&store, "uploads", &upload.id)
            .await
            .unwrap();
        assert_eq!(resumed.offset, 4);
        assert!(matches!(
            resumed
                .append(&store, "uploads", 0, Bytes::from_static(b"bad"))
                .await,
            Err(ResumableUploadError::OffsetMismatch {
                expected: 4,
                actual: 0
            })
        ));
        resumed
            .append(&store, "uploads", 4, Bytes::from_static(b"efgh"))
            .await
            .unwrap();
        assert_eq!(resumed.assemble(&store).await.unwrap(), "abcdefgh");
        resumed.cleanup_chunks(&store).await;
        resumed.save(&store, "uploads").await.unwrap();

        std::fs::remove_dir_all(root).unwrap();
    }
}
