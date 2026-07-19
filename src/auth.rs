use crate::{
    config::Config,
    email::{EmailSender, SignInEmail, email_sender},
    identity::{
        Account, AccountStatus, AuthChallenge, AuthSession, ChallengeId, ChallengePurpose,
        CompletedEmailSignIn, IdentityRepository, PasskeyCredential, SessionKind,
    },
    rate_limit::RateLimiter,
};
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use email_address::EmailAddress;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{str::FromStr, sync::Arc};
use subtle::ConstantTimeEq;
use thiserror::Error;
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::{
    CreationChallengeResponse, DiscoverableAuthentication, DiscoverableKey, Passkey,
    PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse, Webauthn, WebauthnBuilder,
};

const CHALLENGE_LIFETIME: Duration = Duration::minutes(10);
const CHALLENGE_RESEND_DELAY: Duration = Duration::minutes(1);
const MAX_LIVE_EMAIL_CHALLENGES: usize = 3;
const MAX_CHALLENGE_ATTEMPTS: u32 = 5;
const SESSION_IDLE_LIFETIME: Duration = Duration::days(30);
const SESSION_ABSOLUTE_LIFETIME: Duration = Duration::days(90);
const SESSION_TOUCH_INTERVAL: Duration = Duration::minutes(15);
const RECENT_AUTH_LIFETIME: Duration = Duration::minutes(10);

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("enter a valid email address")]
    InvalidEmail,
    #[error("the sign-in link or code is invalid or expired")]
    InvalidCredential,
    #[error("too many sign-in requests; retry in {0} seconds")]
    RateLimited(u64),
    #[error("email delivery is temporarily unavailable")]
    Delivery(#[source] anyhow::Error),
    #[error("authentication storage failed")]
    Internal(#[source] anyhow::Error),
    #[error("sign in again before changing account security")]
    RecentAuthenticationRequired,
    #[error("passkey label must be between 1 and 80 characters")]
    InvalidPasskeyLabel,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailStart {
    pub challenge_id: ChallengeId,
    pub resend_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub enum EmailCredential {
    LinkToken(String),
    Code {
        email: String,
        challenge_id: ChallengeId,
        code: String,
    },
}

#[derive(Debug, Clone)]
pub struct CompletedBrowserSignIn {
    pub completion: CompletedEmailSignIn,
    pub session_token: String,
    pub csrf_token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PasskeyRegistrationStart {
    pub challenge_id: ChallengeId,
    pub options: CreationChallengeResponse,
}

#[derive(Debug, Clone, Serialize)]
pub struct PasskeyAuthenticationStart {
    pub challenge_id: ChallengeId,
    pub options: RequestChallengeResponse,
}

#[derive(Debug, Clone)]
pub struct CompletedPasskeyBrowserSignIn {
    pub account: Account,
    pub session: AuthSession,
    pub passkey: PasskeyCredential,
    pub session_token: String,
    pub csrf_token: String,
}

#[derive(Debug, Clone)]
pub struct AuthenticatedBrowserSession {
    pub account: Account,
    pub session: AuthSession,
    pub csrf_token: Option<String>,
}

pub struct AuthService {
    repository: Arc<dyn IdentityRepository>,
    sender: Arc<dyn EmailSender>,
    public_base_url: Url,
    public_origin: String,
    code_pepper: Vec<u8>,
    source_limiter: RateLimiter,
    email_limiter: RateLimiter,
    webauthn: Webauthn,
}

impl AuthService {
    pub async fn from_config(
        config: &Config,
        repository: Arc<dyn IdentityRepository>,
    ) -> Result<Self> {
        let pepper_path = config
            .auth_code_pepper_file
            .as_deref()
            .context("SEIZA_AUTH_CODE_PEPPER_FILE is required")?;
        let pepper = tokio::fs::read(pepper_path)
            .await
            .with_context(|| format!("reading auth code pepper {}", pepper_path.display()))?;
        let pepper = pepper
            .strip_suffix(b"\n")
            .unwrap_or(&pepper)
            .strip_suffix(b"\r")
            .unwrap_or(&pepper)
            .to_vec();
        if pepper.len() < 32 {
            anyhow::bail!("SEIZA_AUTH_CODE_PEPPER_FILE must contain at least 32 bytes");
        }
        let public_base_url = config
            .public_base_url
            .clone()
            .context("SEIZA_PUBLIC_BASE_URL is required")?;
        Self::try_new(
            repository,
            email_sender(config).await?,
            public_base_url,
            pepper,
        )
    }

    fn try_new(
        repository: Arc<dyn IdentityRepository>,
        sender: Arc<dyn EmailSender>,
        public_base_url: Url,
        code_pepper: Vec<u8>,
    ) -> Result<Self> {
        let public_origin = public_base_url.origin().ascii_serialization();
        let rp_id = public_base_url
            .host_str()
            .context("validated public base URL has no host")?;
        let webauthn = WebauthnBuilder::new(rp_id, &public_base_url)
            .and_then(|builder| builder.rp_name("Seiza").build())
            .map_err(anyhow::Error::from)
            .context("SEIZA_PUBLIC_BASE_URL is not a valid WebAuthn relying party")?;
        Ok(Self {
            repository,
            sender,
            public_base_url,
            public_origin,
            code_pepper,
            source_limiter: RateLimiter::new(10.0, 5.0),
            email_limiter: RateLimiter::new(5.0, 3.0),
            webauthn,
        })
    }

    #[cfg(test)]
    pub fn new(
        repository: Arc<dyn IdentityRepository>,
        sender: Arc<dyn EmailSender>,
        public_base_url: Url,
        code_pepper: Vec<u8>,
    ) -> Self {
        Self::try_new(repository, sender, public_base_url, code_pepper).unwrap()
    }

    pub fn public_origin(&self) -> &str {
        &self.public_origin
    }

    pub async fn start_email(&self, email: &str, source: &str) -> Result<EmailStart, AuthError> {
        let email = normalize_email(email).map_err(|_| AuthError::InvalidEmail)?;
        let source_retry = self.source_limiter.check(source).await.err();
        let email_retry = self
            .email_limiter
            .check(&format!("email:{email}"))
            .await
            .err();
        if let Some(retry_after) = source_retry.into_iter().chain(email_retry).max() {
            return Err(AuthError::RateLimited(retry_after));
        }

        let now = Utc::now();
        let challenge_id = Uuid::now_v7();
        let link_secret = random_secret(32).map_err(AuthError::Internal)?;
        let code = random_code().map_err(AuthError::Internal)?;
        let challenge = AuthChallenge {
            id: challenge_id,
            purpose: ChallengePurpose::EmailLogin,
            account_id: None,
            email_lookup: Some(email.clone()),
            link_token_digest: Some(secret_digest(&link_secret)),
            code_digest: Some(code_digest(&self.code_pepper, challenge_id, &code)),
            webauthn_state_json: None,
            attempts: 0,
            created_at: now,
            expires_at: now + CHALLENGE_LIFETIME,
            consumed_at: None,
        };
        self.repository
            .create_email_challenge(challenge, MAX_LIVE_EMAIL_CHALLENGES)
            .await
            .map_err(AuthError::Internal)?;

        let link_token = format!("seiza_login_{challenge_id}_{link_secret}");
        let mut link = self
            .public_base_url
            .join("signin")
            .map_err(|error| AuthError::Internal(error.into()))?;
        link.query_pairs_mut().append_pair("token", &link_token);
        self.sender
            .send_sign_in(SignInEmail {
                to: email,
                link: link.into(),
                code,
                expires_minutes: CHALLENGE_LIFETIME.num_minutes() as u32,
            })
            .await
            .map_err(AuthError::Delivery)?;
        Ok(EmailStart {
            challenge_id,
            resend_at: now + CHALLENGE_RESEND_DELAY,
        })
    }

    pub async fn complete_email(
        &self,
        credential: EmailCredential,
    ) -> Result<CompletedBrowserSignIn, AuthError> {
        let now = Utc::now();
        let (challenge_id, candidate) = match credential {
            EmailCredential::LinkToken(token) => {
                let (challenge_id, secret) =
                    parse_login_token(&token).ok_or(AuthError::InvalidCredential)?;
                (challenge_id, CandidateCredential::Link(secret))
            }
            EmailCredential::Code {
                email,
                challenge_id,
                code,
            } => {
                let email = normalize_email(&email).map_err(|_| AuthError::InvalidCredential)?;
                if code.len() != 8 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(AuthError::InvalidCredential);
                }
                (challenge_id, CandidateCredential::Code { email, code })
            }
        };
        let challenge = self
            .repository
            .challenge_by_id(challenge_id)
            .await
            .map_err(AuthError::Internal)?
            .ok_or(AuthError::InvalidCredential)?;
        if challenge.purpose != ChallengePurpose::EmailLogin
            || challenge.consumed_at.is_some()
            || challenge.expires_at <= now
            || challenge.attempts >= MAX_CHALLENGE_ATTEMPTS
        {
            return Err(AuthError::InvalidCredential);
        }
        let email_lookup = challenge
            .email_lookup
            .as_deref()
            .ok_or(AuthError::InvalidCredential)?;
        let valid = match candidate {
            CandidateCredential::Link(secret) => challenge
                .link_token_digest
                .as_deref()
                .is_some_and(|digest| constant_time_eq(digest, &secret_digest(&secret))),
            CandidateCredential::Code { email, code } => {
                constant_time_eq(email_lookup, &email)
                    && challenge.code_digest.as_deref().is_some_and(|digest| {
                        constant_time_eq(
                            digest,
                            &code_digest(&self.code_pepper, challenge_id, &code),
                        )
                    })
            }
        };
        if !valid {
            self.repository
                .record_challenge_failure(challenge_id, now, MAX_CHALLENGE_ATTEMPTS)
                .await
                .map_err(AuthError::Internal)?;
            return Err(AuthError::InvalidCredential);
        }

        let account_id = Uuid::now_v7();
        let account = Account {
            id: account_id,
            email: email_lookup.to_owned(),
            email_lookup: email_lookup.to_owned(),
            email_verified_at: now,
            webauthn_user_handle: account_id.to_string(),
            status: AccountStatus::Active,
            created_at: now,
            updated_at: now,
            last_authenticated_at: now,
        };
        let session_id = Uuid::now_v7();
        let session_secret = random_secret(32).map_err(AuthError::Internal)?;
        let csrf_token = random_secret(32).map_err(AuthError::Internal)?;
        let session = AuthSession {
            id: session_id,
            token_digest: secret_digest(&session_secret),
            account_id,
            kind: SessionKind::Browser,
            csrf_digest: Some(secret_digest(&csrf_token)),
            created_at: now,
            last_seen_at: now,
            expires_at: now + SESSION_IDLE_LIFETIME,
            absolute_expires_at: now + SESSION_ABSOLUTE_LIFETIME,
            revoked_at: None,
        };
        let completion = self
            .repository
            .complete_email_challenge(challenge_id, now, MAX_CHALLENGE_ATTEMPTS, account, session)
            .await
            .map_err(AuthError::Internal)?
            .ok_or(AuthError::InvalidCredential)?;
        let session_token = format!(
            "seiza_session_{}_{}_{}",
            completion.account.id, completion.session.id, session_secret
        );
        Ok(CompletedBrowserSignIn {
            completion,
            session_token,
            csrf_token,
        })
    }

    pub async fn authenticate_browser_session(
        &self,
        token: &str,
        csrf_cookie: Option<&str>,
    ) -> Result<AuthenticatedBrowserSession, AuthError> {
        let (account_id, session_id, secret) =
            parse_session_token(token).ok_or(AuthError::InvalidCredential)?;
        let now = Utc::now();
        let mut session = self
            .repository
            .session(account_id, session_id)
            .await
            .map_err(AuthError::Internal)?
            .ok_or(AuthError::InvalidCredential)?;
        if session.kind != SessionKind::Browser
            || session.revoked_at.is_some()
            || session.expires_at <= now
            || session.absolute_expires_at <= now
            || !constant_time_eq(&session.token_digest, &secret_digest(&secret))
        {
            return Err(AuthError::InvalidCredential);
        }
        let account = self
            .repository
            .account_by_id(account_id)
            .await
            .map_err(AuthError::Internal)?
            .filter(|account| account.status == AccountStatus::Active)
            .ok_or(AuthError::InvalidCredential)?;
        if now - session.last_seen_at >= SESSION_TOUCH_INTERVAL {
            let expires_at = (now + SESSION_IDLE_LIFETIME).min(session.absolute_expires_at);
            if self
                .repository
                .touch_session(account_id, session_id, now, expires_at)
                .await
                .map_err(AuthError::Internal)?
            {
                session.last_seen_at = now;
                session.expires_at = expires_at;
            }
        }
        let csrf_token = csrf_cookie.filter(|token| {
            session
                .csrf_digest
                .as_deref()
                .is_some_and(|digest| constant_time_eq(digest, &secret_digest(token)))
        });
        Ok(AuthenticatedBrowserSession {
            account,
            session,
            csrf_token: csrf_token.map(str::to_owned),
        })
    }

    pub fn require_csrf(
        &self,
        authenticated: &AuthenticatedBrowserSession,
        header: Option<&str>,
    ) -> Result<(), AuthError> {
        match (authenticated.csrf_token.as_deref(), header) {
            (Some(cookie), Some(header)) if constant_time_eq(cookie, header) => Ok(()),
            _ => Err(AuthError::InvalidCredential),
        }
    }

    pub async fn logout(
        &self,
        authenticated: &AuthenticatedBrowserSession,
    ) -> Result<(), AuthError> {
        self.repository
            .revoke_session(
                authenticated.account.id,
                authenticated.session.id,
                Utc::now(),
            )
            .await
            .map_err(AuthError::Internal)?;
        Ok(())
    }

    pub async fn logout_all(
        &self,
        authenticated: &AuthenticatedBrowserSession,
    ) -> Result<u64, AuthError> {
        self.repository
            .revoke_all_sessions(authenticated.account.id, Utc::now())
            .await
            .map_err(AuthError::Internal)
    }

    pub fn require_recent_auth(
        &self,
        authenticated: &AuthenticatedBrowserSession,
    ) -> Result<(), AuthError> {
        if Utc::now() - authenticated.session.created_at <= RECENT_AUTH_LIFETIME {
            Ok(())
        } else {
            Err(AuthError::RecentAuthenticationRequired)
        }
    }

    pub async fn start_passkey_registration(
        &self,
        authenticated: &AuthenticatedBrowserSession,
    ) -> Result<PasskeyRegistrationStart, AuthError> {
        self.require_recent_auth(authenticated)?;
        let existing = self
            .repository
            .list_passkeys(authenticated.account.id)
            .await
            .map_err(AuthError::Internal)?
            .into_iter()
            .filter(|credential| credential.revoked_at.is_none())
            .map(|credential| {
                serde_json::from_str::<Passkey>(&credential.credential_json)
                    .map_err(anyhow::Error::from)
            })
            .collect::<Result<Vec<_>>>()
            .map_err(AuthError::Internal)?;
        let exclude = (!existing.is_empty()).then(|| {
            existing
                .iter()
                .map(|passkey| passkey.cred_id().clone())
                .collect()
        });
        let (options, state) = self
            .webauthn
            .start_passkey_registration(
                authenticated.account.id,
                &authenticated.account.email,
                &authenticated.account.email,
                exclude,
            )
            .map_err(|error| AuthError::Internal(error.into()))?;
        let now = Utc::now();
        let challenge_id = Uuid::now_v7();
        self.repository
            .create_challenge(AuthChallenge {
                id: challenge_id,
                purpose: ChallengePurpose::PasskeyRegistration,
                account_id: Some(authenticated.account.id),
                email_lookup: None,
                link_token_digest: None,
                code_digest: None,
                webauthn_state_json: Some(
                    serde_json::to_string(&state)
                        .map_err(|error| AuthError::Internal(error.into()))?,
                ),
                attempts: 0,
                created_at: now,
                expires_at: now + CHALLENGE_LIFETIME,
                consumed_at: None,
            })
            .await
            .map_err(AuthError::Internal)?;
        Ok(PasskeyRegistrationStart {
            challenge_id,
            options,
        })
    }

    pub async fn complete_passkey_registration(
        &self,
        authenticated: &AuthenticatedBrowserSession,
        challenge_id: ChallengeId,
        label: &str,
        credential: RegisterPublicKeyCredential,
    ) -> Result<PasskeyCredential, AuthError> {
        self.require_recent_auth(authenticated)?;
        let label = normalize_passkey_label(label)?;
        let challenge = self
            .valid_webauthn_challenge(
                challenge_id,
                ChallengePurpose::PasskeyRegistration,
                Some(authenticated.account.id),
            )
            .await?;
        let state = serde_json::from_str::<PasskeyRegistration>(
            challenge
                .webauthn_state_json
                .as_deref()
                .ok_or(AuthError::InvalidCredential)?,
        )
        .map_err(|error| AuthError::Internal(error.into()))?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(&credential, &state)
            .map_err(|_| AuthError::InvalidCredential)?;
        let now = Utc::now();
        let persisted = PasskeyCredential {
            id: Uuid::now_v7(),
            credential_id: URL_SAFE_NO_PAD.encode(passkey.cred_id()),
            account_id: authenticated.account.id,
            credential_json: serde_json::to_string(&passkey)
                .map_err(|error| AuthError::Internal(error.into()))?,
            label,
            created_at: now,
            last_used_at: None,
            revoked_at: None,
        };
        if !self
            .repository
            .complete_passkey_registration(challenge_id, persisted.clone(), now)
            .await
            .map_err(AuthError::Internal)?
        {
            return Err(AuthError::InvalidCredential);
        }
        Ok(persisted)
    }

    pub async fn start_passkey_authentication(
        &self,
    ) -> Result<PasskeyAuthenticationStart, AuthError> {
        let (options, state) = self
            .webauthn
            .start_discoverable_authentication()
            .map_err(|error| AuthError::Internal(error.into()))?;
        let now = Utc::now();
        let challenge_id = Uuid::now_v7();
        self.repository
            .create_challenge(AuthChallenge {
                id: challenge_id,
                purpose: ChallengePurpose::PasskeyAuthentication,
                account_id: None,
                email_lookup: None,
                link_token_digest: None,
                code_digest: None,
                webauthn_state_json: Some(
                    serde_json::to_string(&state)
                        .map_err(|error| AuthError::Internal(error.into()))?,
                ),
                attempts: 0,
                created_at: now,
                expires_at: now + CHALLENGE_LIFETIME,
                consumed_at: None,
            })
            .await
            .map_err(AuthError::Internal)?;
        Ok(PasskeyAuthenticationStart {
            challenge_id,
            options,
        })
    }

    pub async fn complete_passkey_authentication(
        &self,
        challenge_id: ChallengeId,
        credential: PublicKeyCredential,
    ) -> Result<CompletedPasskeyBrowserSignIn, AuthError> {
        let challenge = self
            .valid_webauthn_challenge(challenge_id, ChallengePurpose::PasskeyAuthentication, None)
            .await?;
        let state = serde_json::from_str::<DiscoverableAuthentication>(
            challenge
                .webauthn_state_json
                .as_deref()
                .ok_or(AuthError::InvalidCredential)?,
        )
        .map_err(|error| AuthError::Internal(error.into()))?;
        let (account_id, credential_id) = self
            .webauthn
            .identify_discoverable_authentication(&credential)
            .map_err(|_| AuthError::InvalidCredential)?;
        let credential_id = URL_SAFE_NO_PAD.encode(credential_id);
        let mut persisted = self
            .repository
            .passkey_by_credential_id(&credential_id)
            .await
            .map_err(AuthError::Internal)?
            .filter(|passkey| passkey.account_id == account_id && passkey.revoked_at.is_none())
            .ok_or(AuthError::InvalidCredential)?;
        let mut passkey = serde_json::from_str::<Passkey>(&persisted.credential_json)
            .map_err(|error| AuthError::Internal(error.into()))?;
        let result = self
            .webauthn
            .finish_discoverable_authentication(
                &credential,
                state,
                &[DiscoverableKey::from(&passkey)],
            )
            .map_err(|_| AuthError::InvalidCredential)?;
        if !result.user_verified() || passkey.update_credential(&result).is_none() {
            return Err(AuthError::InvalidCredential);
        }
        persisted.credential_json =
            serde_json::to_string(&passkey).map_err(|error| AuthError::Internal(error.into()))?;
        let now = Utc::now();
        let (session, session_secret, csrf_token) =
            new_browser_session(account_id, now).map_err(AuthError::Internal)?;
        let completed = self
            .repository
            .complete_passkey_sign_in(challenge_id, persisted, session, now)
            .await
            .map_err(AuthError::Internal)?
            .ok_or(AuthError::InvalidCredential)?;
        Ok(CompletedPasskeyBrowserSignIn {
            session_token: format!(
                "seiza_session_{}_{}_{}",
                completed.account.id, completed.session.id, session_secret
            ),
            csrf_token,
            account: completed.account,
            session: completed.session,
            passkey: completed.passkey,
        })
    }

    async fn valid_webauthn_challenge(
        &self,
        challenge_id: ChallengeId,
        purpose: ChallengePurpose,
        account_id: Option<Uuid>,
    ) -> Result<AuthChallenge, AuthError> {
        let now = Utc::now();
        self.repository
            .challenge_by_id(challenge_id)
            .await
            .map_err(AuthError::Internal)?
            .filter(|challenge| {
                challenge.purpose == purpose
                    && challenge.account_id == account_id
                    && challenge.consumed_at.is_none()
                    && challenge.expires_at > now
            })
            .ok_or(AuthError::InvalidCredential)
    }
}

enum CandidateCredential {
    Link(String),
    Code { email: String, code: String },
}

fn normalize_passkey_label(label: &str) -> Result<String, AuthError> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > 80 {
        return Err(AuthError::InvalidPasskeyLabel);
    }
    Ok(label.to_owned())
}

fn new_browser_session(
    account_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(AuthSession, String, String)> {
    let session_secret = random_secret(32)?;
    let csrf_token = random_secret(32)?;
    Ok((
        AuthSession {
            id: Uuid::now_v7(),
            token_digest: secret_digest(&session_secret),
            account_id,
            kind: SessionKind::Browser,
            csrf_digest: Some(secret_digest(&csrf_token)),
            created_at: now,
            last_seen_at: now,
            expires_at: now + SESSION_IDLE_LIFETIME,
            absolute_expires_at: now + SESSION_ABSOLUTE_LIFETIME,
            revoked_at: None,
        },
        session_secret,
        csrf_token,
    ))
}

pub fn normalize_email(value: &str) -> Result<String> {
    let normalized = value.trim().to_lowercase();
    EmailAddress::from_str(&normalized).context("invalid email address")?;
    Ok(normalized)
}

fn parse_login_token(token: &str) -> Option<(ChallengeId, String)> {
    let mut components = token.splitn(4, '_');
    if components.next()? != "seiza" || components.next()? != "login" {
        return None;
    }
    let challenge_id = Uuid::parse_str(components.next()?).ok()?;
    let secret = components.next()?.to_owned();
    if secret.is_empty() {
        return None;
    }
    Some((challenge_id, secret))
}

pub fn parse_session_token(token: &str) -> Option<(Uuid, Uuid, String)> {
    let mut components = token.splitn(5, '_');
    if components.next()? != "seiza" || components.next()? != "session" {
        return None;
    }
    let account_id = Uuid::parse_str(components.next()?).ok()?;
    let session_id = Uuid::parse_str(components.next()?).ok()?;
    let secret = components.next()?.to_owned();
    if secret.is_empty() {
        return None;
    }
    Some((account_id, session_id, secret))
}

fn random_secret(bytes: usize) -> Result<String> {
    let mut value = vec![0u8; bytes];
    getrandom::fill(&mut value).context("operating-system random source failed")?;
    Ok(URL_SAFE_NO_PAD.encode(value))
}

fn random_code() -> Result<String> {
    const RANGE: u32 = 100_000_000;
    const LIMIT: u32 = u32::MAX - (u32::MAX % RANGE);
    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).context("operating-system random source failed")?;
        let value = u32::from_le_bytes(bytes);
        if value < LIMIT {
            return Ok(format!("{:08}", value % RANGE));
        }
    }
}

fn secret_digest(secret: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()))
}

fn code_digest(pepper: &[u8], challenge_id: ChallengeId, code: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(pepper).expect("HMAC accepts every key length");
    mac.update(challenge_id.as_bytes());
    mac.update(code.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{email::SignInEmail, sqlx_identity::SqlxIdentityRepository};
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct CapturingSender(Mutex<Vec<SignInEmail>>);

    #[async_trait::async_trait]
    impl EmailSender for CapturingSender {
        async fn send_sign_in(&self, email: SignInEmail) -> Result<()> {
            self.0.lock().await.push(email);
            Ok(())
        }
    }

    async fn service() -> (AuthService, Arc<CapturingSender>) {
        let repository = Arc::new(
            SqlxIdentityRepository::connect("sqlite::memory:")
                .await
                .unwrap(),
        );
        let sender = Arc::new(CapturingSender::default());
        (
            AuthService::new(
                repository,
                sender.clone(),
                Url::parse("https://solve.example.com").unwrap(),
                vec![42; 32],
            ),
            sender,
        )
    }

    #[tokio::test]
    async fn email_code_creates_a_persisted_multi_session_account() {
        let (service, sender) = service().await;
        let first = service
            .start_email("Astronomer@Example.com", "192.0.2.1")
            .await
            .unwrap();
        let message = sender.0.lock().await[0].clone();
        let signed_in = service
            .complete_email(EmailCredential::Code {
                email: "astronomer@example.com".into(),
                challenge_id: first.challenge_id,
                code: message.code,
            })
            .await
            .unwrap();
        assert!(signed_in.completion.account_created);
        assert_eq!(signed_in.completion.account.email, "astronomer@example.com");
        let authenticated = service
            .authenticate_browser_session(&signed_in.session_token, Some(&signed_in.csrf_token))
            .await
            .unwrap();
        service
            .require_csrf(&authenticated, Some(&signed_in.csrf_token))
            .unwrap();

        let second = service
            .start_email("astronomer@example.com", "192.0.2.2")
            .await
            .unwrap();
        let message = sender.0.lock().await[1].clone();
        let second_session = service
            .complete_email(EmailCredential::Code {
                email: "astronomer@example.com".into(),
                challenge_id: second.challenge_id,
                code: message.code,
            })
            .await
            .unwrap();
        assert!(!second_session.completion.account_created);
        assert_ne!(
            signed_in.completion.session.id,
            second_session.completion.session.id
        );
        assert_eq!(
            service
                .repository
                .list_sessions(signed_in.completion.account.id)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn login_links_are_single_use_and_get_does_not_consume_them() {
        let (service, sender) = service().await;
        service
            .start_email("user@example.com", "192.0.2.1")
            .await
            .unwrap();
        let message = sender.0.lock().await[0].clone();
        let token = Url::parse(&message.link)
            .unwrap()
            .query_pairs()
            .find_map(|(name, value)| (name == "token").then(|| value.into_owned()))
            .unwrap();
        service
            .complete_email(EmailCredential::LinkToken(token.clone()))
            .await
            .unwrap();
        assert!(matches!(
            service
                .complete_email(EmailCredential::LinkToken(token))
                .await,
            Err(AuthError::InvalidCredential)
        ));
    }

    #[tokio::test]
    async fn wrong_codes_increment_and_lock_the_challenge() {
        let (service, _) = service().await;
        let start = service
            .start_email("user@example.com", "192.0.2.1")
            .await
            .unwrap();
        for _ in 0..MAX_CHALLENGE_ATTEMPTS {
            assert!(
                service
                    .complete_email(EmailCredential::Code {
                        email: "user@example.com".into(),
                        challenge_id: start.challenge_id,
                        code: "00000000".into(),
                    })
                    .await
                    .is_err()
            );
        }
        assert_eq!(
            service
                .repository
                .challenge_by_id(start.challenge_id)
                .await
                .unwrap()
                .unwrap()
                .attempts,
            MAX_CHALLENGE_ATTEMPTS
        );
    }

    #[test]
    fn composite_session_tokens_preserve_base64url_underscores() {
        let account_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        let token = format!("seiza_session_{account_id}_{session_id}_secret_with_underlines");
        assert_eq!(
            parse_session_token(&token),
            Some((account_id, session_id, "secret_with_underlines".into()))
        );
    }
}
