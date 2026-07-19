//! TUS resumable-upload endpoints and helpers. Split from the parent
//! module, which keeps the solve, catalog, and worker surface.

use super::*;

pub(super) async fn resumable_upload_options(State(state): State<AppState>) -> Response {
    (
        StatusCode::NO_CONTENT,
        tus_headers(state.config.max_upload_bytes),
    )
        .into_response()
}

pub(super) async fn create_resumable_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    verify_tus_version(&headers)?;
    let client = client_from_headers(&state, &headers, None, true).await?;
    let upload_concat = headers
        .get("upload-concat")
        .and_then(|value| value.to_str().ok());
    let mut upload = match upload_concat {
        Some("partial") => {
            let total_size = checked_upload_length(&state, &headers)?;
            ResumableUpload::new_partial(total_size, client.id, client.queue_weight)
        }
        Some(value) if value.starts_with("final;") => {
            state
                .limiter
                .check(&client.id)
                .await
                .map_err(ApiError::rate_limited)?;
            let part_ids = parse_concat_part_ids(value)?;
            let mut parts = Vec::with_capacity(part_ids.len());
            for part_id in &part_ids {
                let part = ResumableUpload::load(&state.store, &state.config.s3_prefix, part_id)
                    .await
                    .map_err(resumable_api_error)?;
                ensure_upload_owner(&part, &client)?;
                if !part.partial || part.offset != part.total_size || part.job_id.is_some() {
                    return Err(ApiError::upload_conflict(
                        "Upload-Concat references an incomplete or non-partial upload",
                    ));
                }
                parts.push(part);
            }
            let total_size = parts.iter().try_fold(0_u64, |total, part| {
                total
                    .checked_add(part.total_size)
                    .ok_or_else(ApiError::payload_too_large)
            })?;
            if total_size == 0 {
                return Err(ApiError::bad_request("uploaded image must not be empty"));
            }
            if total_size > state.config.max_upload_bytes as u64 {
                return Err(ApiError::payload_too_large());
            }
            let (original_filename, content_type, options) = upload_metadata(&headers)?;
            let object_key = state.new_object_key(&original_filename);
            ResumableUpload::concatenate(
                original_filename,
                content_type,
                object_key,
                options,
                client.id,
                client.queue_weight,
                &parts,
            )
            .map_err(resumable_api_error)?
        }
        Some(_) => {
            return Err(ApiError::bad_request(
                "Upload-Concat must be `partial` or `final;<upload URLs>`",
            ));
        }
        None => {
            state
                .limiter
                .check(&client.id)
                .await
                .map_err(ApiError::rate_limited)?;
            let total_size = checked_upload_length(&state, &headers)?;
            let (original_filename, content_type, options) = upload_metadata(&headers)?;
            let object_key = state.new_object_key(&original_filename);
            ResumableUpload::new(
                original_filename,
                content_type,
                total_size,
                object_key,
                options,
                client.id,
                client.queue_weight,
            )
        }
    };
    upload
        .save(&state.store, &state.config.s3_prefix)
        .await
        .map_err(resumable_api_error)?;

    if !upload.partial && !upload.concat_parts.is_empty() {
        state.finalize_resumable(&mut upload).await?;
        for part_id in &upload.concat_parts {
            let part = ResumableUpload::load(&state.store, &state.config.s3_prefix, part_id)
                .await
                .map_err(resumable_api_error)?;
            if let Err(error) = part
                .delete_state(&state.store, &state.config.s3_prefix)
                .await
            {
                tracing::warn!(upload_id = %part.id, %error, "could not remove concatenated upload state");
            }
        }
    }

    let mut response_headers = tus_headers(state.config.max_upload_bytes);
    response_headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/api/v1/uploads/{}", upload.id))
            .map_err(ApiError::internal)?,
    );
    response_headers.insert(
        http::HeaderName::from_static("upload-offset"),
        HeaderValue::from_str(&upload.offset.to_string()).map_err(ApiError::internal)?,
    );
    Ok((StatusCode::CREATED, response_headers).into_response())
}

pub(super) fn checked_upload_length(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<u64, ApiError> {
    let total_size = required_u64_header(headers, "upload-length")?;
    if total_size == 0 {
        return Err(ApiError::bad_request("uploaded image must not be empty"));
    }
    if total_size > state.config.max_upload_bytes as u64 {
        return Err(ApiError::payload_too_large());
    }
    Ok(total_size)
}

pub(super) fn upload_metadata(
    headers: &HeaderMap,
) -> Result<(String, Option<String>, SolveOptions), ApiError> {
    let metadata = parse_upload_metadata(headers)?;
    let original_filename = metadata
        .get("filename")
        .map(|filename| safe_filename(filename))
        .filter(|filename| !filename.is_empty())
        .ok_or_else(|| ApiError::bad_request("Upload-Metadata must include filename"))?;
    let content_type = metadata
        .get("filetype")
        .filter(|value| !value.is_empty() && value.len() <= 255)
        .cloned();
    let options = metadata
        .get("options")
        .map(|raw| {
            serde_json::from_str::<SolveOptions>(raw)
                .map_err(|error| ApiError::bad_request(format!("invalid options JSON: {error}")))
        })
        .transpose()?
        .unwrap_or_default();
    options.validate().map_err(ApiError::bad_request)?;
    Ok((original_filename, content_type, options))
}

pub(super) fn parse_concat_part_ids(value: &str) -> Result<Vec<String>, ApiError> {
    let raw_parts = value
        .strip_prefix("final;")
        .ok_or_else(|| ApiError::bad_request("invalid Upload-Concat header"))?;
    let mut ids = Vec::new();
    for raw in raw_parts.split_ascii_whitespace() {
        let path = raw
            .parse::<http::Uri>()
            .map_err(|_| ApiError::bad_request("Upload-Concat contains an invalid upload URL"))?
            .path()
            .to_owned();
        let id = path
            .strip_prefix("/api/v1/uploads/")
            .filter(|id| !id.is_empty() && !id.contains('/'))
            .ok_or_else(|| {
                ApiError::bad_request("Upload-Concat may only reference this server's uploads")
            })?;
        if Uuid::parse_str(id).is_err() || ids.iter().any(|existing| existing == id) {
            return Err(ApiError::bad_request(
                "Upload-Concat contains an invalid or duplicate upload URL",
            ));
        }
        ids.push(id.to_owned());
    }
    if ids.is_empty() {
        return Err(ApiError::bad_request(
            "Upload-Concat must reference at least one partial upload",
        ));
    }
    Ok(ids)
}

pub(super) async fn head_resumable_upload(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    verify_tus_version(&headers)?;
    let client = client_from_headers(&state, &headers, None, false).await?;
    let upload = ResumableUpload::load(&state.store, &state.config.s3_prefix, &upload_id)
        .await
        .map_err(resumable_api_error)?;
    ensure_upload_owner(&upload, &client)?;
    let mut response_headers = tus_headers(state.config.max_upload_bytes);
    insert_u64_header(&mut response_headers, "upload-offset", upload.offset)?;
    insert_u64_header(&mut response_headers, "upload-length", upload.total_size)?;
    if upload.partial {
        response_headers.insert(
            http::HeaderName::from_static("upload-concat"),
            HeaderValue::from_static("partial"),
        );
    } else if !upload.concat_parts.is_empty() {
        let value = format!(
            "final;{}",
            upload
                .concat_parts
                .iter()
                .map(|id| format!("/api/v1/uploads/{id}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        response_headers.insert(
            http::HeaderName::from_static("upload-concat"),
            HeaderValue::from_str(&value).map_err(ApiError::internal)?,
        );
    }
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((StatusCode::OK, response_headers).into_response())
}

pub(super) async fn patch_resumable_upload(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    verify_tus_version(&headers)?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if content_type != "application/offset+octet-stream" {
        return Err(ApiError::bad_request(
            "chunk Content-Type must be application/offset+octet-stream",
        ));
    }
    let client = client_from_headers(&state, &headers, None, true).await?;
    let offset = required_u64_header(&headers, "upload-offset")?;
    let lock = state.upload_lock(&upload_id).await;
    let _guard = lock.lock().await;
    let mut upload = ResumableUpload::load(&state.store, &state.config.s3_prefix, &upload_id)
        .await
        .map_err(resumable_api_error)?;
    ensure_upload_owner(&upload, &client)?;
    let new_offset = upload
        .append(&state.store, &state.config.s3_prefix, offset, body)
        .await
        .map_err(resumable_api_error)?;
    if new_offset == upload.total_size && !upload.partial {
        state.finalize_resumable(&mut upload).await?;
    }
    let mut response_headers = tus_headers(state.config.max_upload_bytes);
    insert_u64_header(&mut response_headers, "upload-offset", new_offset)?;
    Ok((StatusCode::NO_CONTENT, response_headers).into_response())
}

pub(super) async fn delete_resumable_upload(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    verify_tus_version(&headers)?;
    let client = client_from_headers(&state, &headers, None, true).await?;
    let lock = state.upload_lock(&upload_id).await;
    let _guard = lock.lock().await;
    let upload = ResumableUpload::load(&state.store, &state.config.s3_prefix, &upload_id)
        .await
        .map_err(resumable_api_error)?;
    ensure_upload_owner(&upload, &client)?;
    upload
        .terminate(&state.store, &state.config.s3_prefix)
        .await
        .map_err(resumable_api_error)?;
    Ok((
        StatusCode::NO_CONTENT,
        tus_headers(state.config.max_upload_bytes),
    )
        .into_response())
}

pub(super) async fn get_resumable_upload_result(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<JobResponse>, ApiError> {
    let client = client_from_headers(&state, &headers, None, false).await?;
    let lock = state.upload_lock(&upload_id).await;
    let _guard = lock.lock().await;
    let mut upload = ResumableUpload::load(&state.store, &state.config.s3_prefix, &upload_id)
        .await
        .map_err(resumable_api_error)?;
    ensure_upload_owner(&upload, &client)?;
    if upload.offset != upload.total_size {
        return Err(ApiError::artifact_not_ready(format!(
            "upload has received {} of {} bytes",
            upload.offset, upload.total_size
        )));
    }
    // Finalizing an uncommitted upload enqueues a solve. An API key needs the
    // submit scope for that; browser and Astrometry sessions already carry
    // submission authority.
    if upload.job_id.is_none() && request_api_key(&headers).is_some() {
        client_from_headers(&state, &headers, None, true).await?;
    }
    let job = state.finalize_resumable(&mut upload).await?;
    Ok(Json(state.job_response(&job)?))
}

pub(super) fn tus_headers(max_upload_bytes: usize) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::HeaderName::from_static("tus-resumable"),
        HeaderValue::from_static(TUS_VERSION),
    );
    headers.insert(
        http::HeaderName::from_static("tus-version"),
        HeaderValue::from_static(TUS_VERSION),
    );
    headers.insert(
        http::HeaderName::from_static("tus-extension"),
        HeaderValue::from_static(TUS_EXTENSIONS),
    );
    if let Ok(value) = HeaderValue::from_str(&max_upload_bytes.to_string()) {
        headers.insert(http::HeaderName::from_static("tus-max-size"), value);
    }
    headers
}

pub(super) fn verify_tus_version(headers: &HeaderMap) -> Result<(), ApiError> {
    match headers
        .get("tus-resumable")
        .and_then(|value| value.to_str().ok())
    {
        Some(TUS_VERSION) => Ok(()),
        _ => Err(ApiError::bad_request(format!(
            "Tus-Resumable must be {TUS_VERSION}"
        ))),
    }
}

pub(super) fn required_u64_header(
    headers: &HeaderMap,
    name: &'static str,
) -> Result<u64, ApiError> {
    headers
        .get(name)
        .ok_or_else(|| ApiError::bad_request(format!("missing {name} header")))?
        .to_str()
        .map_err(ApiError::bad_request)?
        .parse()
        .map_err(|_| ApiError::bad_request(format!("invalid {name} header")))
}

pub(super) fn insert_u64_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: u64,
) -> Result<(), ApiError> {
    headers.insert(
        http::HeaderName::from_static(name),
        HeaderValue::from_str(&value.to_string()).map_err(ApiError::internal)?,
    );
    Ok(())
}

pub(super) fn parse_upload_metadata(
    headers: &HeaderMap,
) -> Result<HashMap<String, String>, ApiError> {
    let Some(raw) = headers
        .get("upload-metadata")
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(HashMap::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, encoded) = pair.split_once(' ').unwrap_or((pair, ""));
            let value = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| ApiError::bad_request("invalid base64 Upload-Metadata value"))?;
            let value = String::from_utf8(value)
                .map_err(|_| ApiError::bad_request("Upload-Metadata value is not UTF-8"))?;
            Ok((key.to_owned(), value))
        })
        .collect()
}

pub(super) fn ensure_upload_owner(
    upload: &ResumableUpload,
    client: &Client,
) -> Result<(), ApiError> {
    if upload.owner == client.id {
        Ok(())
    } else {
        Err(ApiError::not_found_message("upload session not found"))
    }
}

pub(super) fn resumable_api_error(error: ResumableUploadError) -> ApiError {
    match error {
        ResumableUploadError::NotFound => ApiError::not_found_message("upload session not found"),
        ResumableUploadError::OffsetMismatch { expected, actual } => ApiError::upload_conflict(
            format!("upload offset mismatch: expected {expected}, received {actual}"),
        ),
        ResumableUploadError::ExceedsLength => {
            ApiError::bad_request("upload chunk exceeds declared file length")
        }
        ResumableUploadError::Incomplete { offset, total } => {
            ApiError::artifact_not_ready(format!("upload has received {offset} of {total} bytes"))
        }
        ResumableUploadError::Completed => {
            ApiError::upload_conflict("upload has already completed")
        }
        ResumableUploadError::Internal(error) => ApiError::internal(error),
    }
}
