//! Account authentication and management endpoints: email and passkey
//! sign-in, sessions, API keys, cookies, and the sign-in rate-limiting
//! identity. Split from the parent module, which keeps the solve, upload,
//! catalog, and worker surface.

use super::*;

/// Auth and account requests are small JSON bodies; the largest are WebAuthn
/// ceremony payloads at a few kilobytes.
pub(super) const AUTH_BODY_LIMIT_BYTES: usize = 64 * 1024;
const ACCOUNT_SOLVE_HISTORY_LIMIT: usize = 100;

#[derive(Deserialize)]
pub(super) struct EmailStartRequest {
    pub(super) email: String,
}

pub(super) async fn start_email_sign_in(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<EmailStartRequest>,
) -> Result<Response, ApiError> {
    let auth = auth_service(&state)?;
    let source = request_source(&state.config, &headers, peer);
    let started = auth
        .start_email(&request.email, &source)
        .await
        .map_err(auth_api_error)?;
    let mut response = (StatusCode::ACCEPTED, Json(started)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(Deserialize)]
pub(super) struct EmailCompleteRequest {
    #[serde(alias = "token")]
    pub(super) link_token: Option<String>,
    pub(super) email: Option<String>,
    pub(super) challenge_id: Option<Uuid>,
    pub(super) code: Option<String>,
}

pub(super) async fn complete_email_sign_in(
    State(state): State<AppState>,
    Json(request): Json<EmailCompleteRequest>,
) -> Result<Response, ApiError> {
    let auth = auth_service(&state)?;
    let credential = match (
        request.link_token,
        request.email,
        request.challenge_id,
        request.code,
    ) {
        (Some(token), None, None, None) => EmailCredential::LinkToken(token),
        (None, Some(email), Some(challenge_id), Some(code)) => EmailCredential::Code {
            email,
            challenge_id,
            code,
        },
        _ => {
            return Err(ApiError::bad_request(
                "provide either link_token or email, challenge_id, and code",
            ));
        }
    };
    let signed_in = auth
        .complete_email(credential)
        .await
        .map_err(auth_api_error)?;
    let passkey_count = state
        .identity
        .as_ref()
        .expect("auth service requires identity repository")
        .list_passkeys(signed_in.completion.account.id)
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .filter(|passkey| passkey.revoked_at.is_none())
        .count();
    let mut response = Json(json!({
        "status": "success",
        "account": account_json(&signed_in.completion.account),
        "account_created": signed_in.completion.account_created,
        "passkey_setup_required": passkey_count == 0,
        "csrf_token": signed_in.csrf_token,
    }))
    .into_response();
    append_auth_cookies(
        &mut response,
        &state,
        &signed_in.session_token,
        &signed_in.csrf_token,
    )?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    Ok(response)
}

pub(super) async fn get_account(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let authenticated = authenticated_browser(&state, &headers).await?;
    let identity = state
        .identity
        .as_ref()
        .expect("auth service requires identity repository");
    let sessions = identity
        .list_sessions(authenticated.account.id)
        .await
        .map_err(ApiError::internal)?;
    let passkeys = identity
        .list_passkeys(authenticated.account.id)
        .await
        .map_err(ApiError::internal)?;
    let api_keys = identity
        .list_api_keys(authenticated.account.id)
        .await
        .map_err(ApiError::internal)?;
    let mut response = Json(json!({
        "account": account_json(&authenticated.account),
        "csrf_token": authenticated.csrf_token,
        "passkey_setup_required": !passkeys.iter().any(|passkey| passkey.revoked_at.is_none()),
        "passkeys": passkeys.iter().filter(|passkey| passkey.revoked_at.is_none()).map(passkey_json).collect::<Vec<_>>(),
        "api_keys": api_keys.iter().filter(|key| key.revoked_at.is_none()).map(api_key_json).collect::<Vec<_>>(),
        "sessions": sessions.into_iter().filter(|session| {
            session.revoked_at.is_none()
                && session.expires_at > Utc::now()
                && session.absolute_expires_at > Utc::now()
        }).map(|session| json!({
            "id": session.id,
            "kind": session.kind,
            "api_key_id": session.api_key_id,
            "created_at": session.created_at,
            "last_seen_at": session.last_seen_at,
            "expires_at": session.expires_at,
            "revoked_at": session.revoked_at,
            "current": session.id == authenticated.session.id,
        })).collect::<Vec<_>>(),
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(super) async fn list_account_solves(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let authenticated = authenticated_browser(&state, &headers).await?;
    let owner = format!("account:{}", authenticated.account.id);
    let solves = state
        .repository
        .list_by_owner(&owner, ACCOUNT_SOLVE_HISTORY_LIMIT)
        .await
        .map_err(ApiError::internal)?;
    no_store_json(json!({
        "solves": solves.into_iter().map(|job| json!({
            "id": job.id,
            "status": job.status,
            "original_filename": job.original_filename,
            "created_at": job.created_at,
            "started_at": job.started_at,
            "completed_at": job.completed_at,
            "solve_time_ms": solve_time_ms(job.started_at, job.completed_at),
        })).collect::<Vec<_>>(),
    }))
}

pub(super) async fn start_passkey_sign_in(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let source = request_source(&state.config, &headers, peer);
    let started = auth_service(&state)?
        .start_passkey_authentication(&source)
        .await
        .map_err(auth_api_error)?;
    no_store_json(started)
}

#[derive(Deserialize)]
pub(super) struct PasskeyAuthenticationCompleteRequest {
    challenge_id: Uuid,
    credential: webauthn_rs::prelude::PublicKeyCredential,
}

pub(super) async fn complete_passkey_sign_in(
    State(state): State<AppState>,
    Json(request): Json<PasskeyAuthenticationCompleteRequest>,
) -> Result<Response, ApiError> {
    let signed_in = auth_service(&state)?
        .complete_passkey_authentication(request.challenge_id, request.credential)
        .await
        .map_err(auth_api_error)?;
    let mut response = Json(json!({
        "status": "success",
        "account": account_json(&signed_in.account),
        "csrf_token": signed_in.csrf_token,
        "passkey": passkey_json(&signed_in.passkey),
    }))
    .into_response();
    append_auth_cookies(
        &mut response,
        &state,
        &signed_in.session_token,
        &signed_in.csrf_token,
    )?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(super) async fn list_passkeys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let authenticated = authenticated_browser(&state, &headers).await?;
    let passkeys = state
        .identity
        .as_ref()
        .expect("auth service requires identity repository")
        .list_passkeys(authenticated.account.id)
        .await
        .map_err(ApiError::internal)?;
    no_store_json(json!({
        "passkeys": passkeys.iter().filter(|passkey| passkey.revoked_at.is_none()).map(passkey_json).collect::<Vec<_>>(),
    }))
}

pub(super) async fn start_passkey_registration(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let authenticated = authenticated_browser_for_mutation(&state, &headers).await?;
    let started = auth_service(&state)?
        .start_passkey_registration(&authenticated)
        .await
        .map_err(auth_api_error)?;
    no_store_json(started)
}

#[derive(Deserialize)]
pub(super) struct PasskeyRegistrationCompleteRequest {
    challenge_id: Uuid,
    label: String,
    credential: webauthn_rs::prelude::RegisterPublicKeyCredential,
}

pub(super) async fn complete_passkey_registration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PasskeyRegistrationCompleteRequest>,
) -> Result<Response, ApiError> {
    let authenticated = authenticated_browser_for_mutation(&state, &headers).await?;
    let passkey = auth_service(&state)?
        .complete_passkey_registration(
            &authenticated,
            request.challenge_id,
            &request.label,
            request.credential,
        )
        .await
        .map_err(auth_api_error)?;
    no_store_json(json!({ "passkey": passkey_json(&passkey) }))
}

pub(super) async fn revoke_passkey(
    State(state): State<AppState>,
    Path(passkey_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let authenticated = authenticated_browser_for_mutation(&state, &headers).await?;
    auth_service(&state)?
        .require_recent_auth(&authenticated)
        .map_err(auth_api_error)?;
    let revoked = state
        .identity
        .as_ref()
        .expect("auth service requires identity repository")
        .revoke_passkey(authenticated.account.id, passkey_id, Utc::now())
        .await
        .map_err(ApiError::internal)?;
    if !revoked {
        return Err(ApiError::not_found_message("passkey not found"));
    }
    no_store_json(json!({ "status": "success" }))
}

pub(super) fn passkey_json(passkey: &crate::identity::PasskeyCredential) -> Value {
    json!({
        "id": passkey.id,
        "label": passkey.label,
        "created_at": passkey.created_at,
        "last_used_at": passkey.last_used_at,
    })
}

pub(super) async fn list_api_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let authenticated = authenticated_browser(&state, &headers).await?;
    let keys = state
        .identity
        .as_ref()
        .expect("auth service requires identity repository")
        .list_api_keys(authenticated.account.id)
        .await
        .map_err(ApiError::internal)?;
    no_store_json(json!({
        "api_keys": keys.iter().filter(|key| key.revoked_at.is_none()).map(api_key_json).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
pub(super) struct CreateApiKeyRequest {
    name: String,
    #[serde(default)]
    scopes: Vec<String>,
}

pub(super) async fn create_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateApiKeyRequest>,
) -> Result<Response, ApiError> {
    let authenticated = authenticated_browser_for_mutation(&state, &headers).await?;
    let created = auth_service(&state)?
        .create_api_key(&authenticated, &request.name, &request.scopes)
        .await
        .map_err(auth_api_error)?;
    no_store_json(json!({
        "api_key": api_key_json(&created.api_key),
        "token": created.token,
    }))
}

pub(super) async fn revoke_api_key(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let authenticated = authenticated_browser_for_mutation(&state, &headers).await?;
    auth_service(&state)?
        .require_recent_auth(&authenticated)
        .map_err(auth_api_error)?;
    let revoked = state
        .identity
        .as_ref()
        .expect("auth service requires identity repository")
        .revoke_api_key(authenticated.account.id, key_id, Utc::now())
        .await
        .map_err(ApiError::internal)?;
    if !revoked {
        return Err(ApiError::not_found_message("API key not found"));
    }
    no_store_json(json!({ "status": "success" }))
}

pub(super) fn api_key_json(key: &crate::identity::ApiKey) -> Value {
    json!({
        "id": key.id,
        "name": key.name,
        "display_prefix": key.display_prefix,
        "scopes": key.scopes,
        "queue_weight": key.queue_weight,
        "created_at": key.created_at,
        "expires_at": key.expires_at,
        "last_used_at": key.last_used_at,
    })
}

pub(super) async fn revoke_account_session(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let authenticated = authenticated_browser_for_mutation(&state, &headers).await?;
    auth_service(&state)?
        .require_recent_auth(&authenticated)
        .map_err(auth_api_error)?;
    if session_id == authenticated.session.id {
        auth_service(&state)?
            .logout(&authenticated)
            .await
            .map_err(auth_api_error)?;
        return cleared_auth_response(&state, json!({ "status": "success" }));
    }
    let revoked = state
        .identity
        .as_ref()
        .expect("auth service requires identity repository")
        .revoke_session(authenticated.account.id, session_id, Utc::now())
        .await
        .map_err(ApiError::internal)?;
    if !revoked {
        return Err(ApiError::not_found_message("session not found"));
    }
    no_store_json(json!({ "status": "success" }))
}

pub(super) fn no_store_json(value: impl Serialize) -> Result<Response, ApiError> {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(super) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let authenticated = authenticated_browser_for_mutation(&state, &headers).await?;
    auth_service(&state)?
        .logout(&authenticated)
        .await
        .map_err(auth_api_error)?;
    cleared_auth_response(&state, json!({ "status": "success" }))
}

pub(super) async fn logout_all(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let authenticated = authenticated_browser_for_mutation(&state, &headers).await?;
    let revoked = auth_service(&state)?
        .logout_all(&authenticated)
        .await
        .map_err(auth_api_error)?;
    cleared_auth_response(
        &state,
        json!({ "status": "success", "revoked_sessions": revoked }),
    )
}

pub(super) fn auth_service(state: &AppState) -> Result<&AuthService, ApiError> {
    state
        .auth
        .as_deref()
        .ok_or_else(|| ApiError::not_found_message("account authentication is disabled"))
}

pub(super) async fn authenticated_browser(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthenticatedBrowserSession, ApiError> {
    let auth = auth_service(state)?;
    let token = request_cookie(headers, session_cookie_name(state))
        .ok_or_else(|| ApiError::unauthorized("sign in to continue"))?;
    let csrf = request_cookie(headers, csrf_cookie_name(state));
    auth.authenticate_browser_session(&token, csrf.as_deref())
        .await
        .map_err(auth_api_error)
}

pub(super) async fn authenticated_browser_for_mutation(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthenticatedBrowserSession, ApiError> {
    let auth = auth_service(state)?;
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("request origin is required"))?;
    if origin != auth.public_origin() {
        return Err(ApiError::unauthorized("request origin is not allowed"));
    }
    let authenticated = authenticated_browser(state, headers).await?;
    let csrf = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok());
    auth.require_csrf(&authenticated, csrf)
        .map_err(auth_api_error)?;
    Ok(authenticated)
}

pub(super) fn account_json(account: &crate::identity::Account) -> Value {
    json!({
        "id": account.id,
        "email": account.email,
        "email_verified_at": account.email_verified_at,
        "created_at": account.created_at,
        "last_authenticated_at": account.last_authenticated_at,
    })
}

pub(super) fn session_cookie_name(state: &AppState) -> &'static str {
    if state.config.secure_auth_cookies() {
        "__Host-seiza_session"
    } else {
        "seiza_session"
    }
}

pub(super) fn csrf_cookie_name(state: &AppState) -> &'static str {
    if state.config.secure_auth_cookies() {
        "__Host-seiza_csrf"
    } else {
        "seiza_csrf"
    }
}

pub(super) fn request_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(Cookie::split_parse)
        .filter_map(Result::ok)
        .find(|cookie| cookie.name() == name)
        .map(|cookie| cookie.value().to_owned())
}

pub(super) fn append_auth_cookies(
    response: &mut Response,
    state: &AppState,
    session_token: &str,
    csrf_token: &str,
) -> Result<(), ApiError> {
    let secure = state.config.secure_auth_cookies();
    let max_age = CookieDuration::days(90);
    let session = Cookie::build((session_cookie_name(state), session_token.to_owned()))
        .path("/")
        .secure(secure)
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(max_age)
        .build();
    let csrf = Cookie::build((csrf_cookie_name(state), csrf_token.to_owned()))
        .path("/")
        .secure(secure)
        .http_only(false)
        .same_site(SameSite::Lax)
        .max_age(max_age)
        .build();
    append_set_cookie(response, session)?;
    append_set_cookie(response, csrf)
}

pub(super) fn cleared_auth_response(state: &AppState, body: Value) -> Result<Response, ApiError> {
    let secure = state.config.secure_auth_cookies();
    let mut response = Json(body).into_response();
    for (name, http_only) in [
        (session_cookie_name(state), true),
        (csrf_cookie_name(state), false),
    ] {
        let cookie = Cookie::build((name, ""))
            .path("/")
            .secure(secure)
            .http_only(http_only)
            .same_site(SameSite::Lax)
            .max_age(CookieDuration::ZERO)
            .build();
        append_set_cookie(&mut response, cookie)?;
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(super) fn append_set_cookie(
    response: &mut Response,
    cookie: Cookie<'_>,
) -> Result<(), ApiError> {
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie.to_string()).map_err(ApiError::internal)?,
    );
    Ok(())
}

/// Derives the rate-limiting identity of a request. Forwarded headers are
/// client-controlled, so they are honored only when the operator declares how
/// many trusted proxies stand in front of the server; the entry that the
/// nearest trusted proxy recorded is the client. Otherwise the connected peer
/// address is authoritative.
pub(super) fn request_source(config: &Config, headers: &HeaderMap, peer: SocketAddr) -> String {
    if config.trusted_proxy_hops > 0 {
        let forwarded: Vec<&str> = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect();
        let claimed = if forwarded.len() >= config.trusted_proxy_hops {
            Some(forwarded[forwarded.len() - config.trusted_proxy_hops])
        } else {
            headers
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
        };
        if let Some(address) = claimed.and_then(|value| value.parse::<IpAddr>().ok()) {
            return rate_limit_source(address);
        }
    }
    rate_limit_source(peer.ip())
}

/// IPv6 clients rotate within their /64 trivially, so buckets cover the
/// prefix, matching `public_client_id`.
pub(super) fn rate_limit_source(address: IpAddr) -> String {
    match address {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => {
            let prefix = u128::from(address) & (u128::MAX << 64);
            Ipv6Addr::from(prefix).to_string()
        }
    }
}

pub(super) fn auth_api_error(error: AuthError) -> ApiError {
    match error {
        AuthError::InvalidEmail => ApiError::bad_request("enter a valid email address"),
        AuthError::InvalidCredential => {
            ApiError::unauthorized("the sign-in credential is invalid or expired")
        }
        AuthError::RateLimited(retry_after) => ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "auth_rate_limited",
            message: "too many sign-in requests".into(),
            retry_after: Some(retry_after),
        },
        AuthError::Delivery(source) => {
            tracing::error!(error = %source, "sign-in email delivery failed");
            ApiError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "email_unavailable",
                message: "email delivery is temporarily unavailable".into(),
                retry_after: Some(30),
            }
        }
        AuthError::RecentAuthenticationRequired => {
            ApiError::forbidden("sign in again before changing account security")
        }
        AuthError::InvalidPasskeyLabel => {
            ApiError::bad_request("passkey label must be between 1 and 80 characters")
        }
        AuthError::InvalidApiKeyRequest => ApiError::bad_request(
            "API key name must be 1-80 characters and scopes must be supported",
        ),
        AuthError::Internal(source) => ApiError::internal(source),
    }
}
