use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

pub type AccountId = Uuid;
pub type ChallengeId = Uuid;
pub type SessionId = Uuid;
pub type PasskeyId = Uuid;
pub type ApiKeyId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountStatus {
    Active,
    Disabled,
}

impl AccountStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "disabled" => Ok(Self::Disabled),
            _ => anyhow::bail!("unknown account status `{value}`"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub email: String,
    pub email_lookup: String,
    pub email_verified_at: DateTime<Utc>,
    /// Stable random WebAuthn UUID. It is never derived from the email address.
    pub webauthn_user_handle: String,
    pub status: AccountStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_authenticated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChallengePurpose {
    EmailLogin,
    PasskeyRegistration,
    PasskeyAuthentication,
}

impl ChallengePurpose {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::EmailLogin => "email-login",
            Self::PasskeyRegistration => "passkey-registration",
            Self::PasskeyAuthentication => "passkey-authentication",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "email-login" => Ok(Self::EmailLogin),
            "passkey-registration" => Ok(Self::PasskeyRegistration),
            "passkey-authentication" => Ok(Self::PasskeyAuthentication),
            _ => anyhow::bail!("unknown authentication challenge purpose `{value}`"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthChallenge {
    pub id: ChallengeId,
    pub purpose: ChallengePurpose,
    pub account_id: Option<AccountId>,
    pub email_lookup: Option<String>,
    pub link_token_digest: Option<String>,
    pub code_digest: Option<String>,
    pub webauthn_state_json: Option<String>,
    pub attempts: u32,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionKind {
    Browser,
    Astrometry,
}

impl SessionKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Astrometry => "astrometry",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "browser" => Ok(Self::Browser),
            "astrometry" => Ok(Self::Astrometry),
            _ => anyhow::bail!("unknown authentication session kind `{value}`"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthSession {
    pub id: SessionId,
    pub token_digest: String,
    pub account_id: AccountId,
    pub kind: SessionKind,
    pub csrf_digest: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub absolute_expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasskeyCredential {
    pub id: PasskeyId,
    pub credential_id: String,
    pub account_id: AccountId,
    pub credential_json: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: ApiKeyId,
    pub account_id: AccountId,
    pub secret_digest: String,
    pub display_prefix: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub queue_weight: f64,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletedEmailSignIn {
    pub account: Account,
    pub session: AuthSession,
    pub account_created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedPasskeySignIn {
    pub account: Account,
    pub session: AuthSession,
    pub passkey: PasskeyCredential,
}

/// Persistent identity boundary shared by SQLx and DynamoDB deployments.
///
/// Session and API-key authentication always supplies both the account ID and
/// record ID, allowing implementations to use a primary-key lookup rather than
/// a scan. Mutation methods used by authentication ceremonies are added in the
/// phase that introduces those ceremonies.
#[async_trait]
pub trait IdentityRepository: Send + Sync {
    async fn create_account(&self, account: Account) -> Result<()>;
    async fn account_by_id(&self, account_id: AccountId) -> Result<Option<Account>>;
    async fn account_by_email_lookup(&self, email_lookup: &str) -> Result<Option<Account>>;
    async fn account_by_user_handle(&self, user_handle: &str) -> Result<Option<Account>>;

    async fn create_challenge(&self, challenge: AuthChallenge) -> Result<()>;
    async fn challenge_by_id(&self, challenge_id: ChallengeId) -> Result<Option<AuthChallenge>>;
    async fn create_email_challenge(&self, challenge: AuthChallenge, max_live: usize)
    -> Result<()>;
    async fn record_challenge_failure(
        &self,
        challenge_id: ChallengeId,
        now: DateTime<Utc>,
        max_attempts: u32,
    ) -> Result<Option<AuthChallenge>>;
    async fn complete_email_challenge(
        &self,
        challenge_id: ChallengeId,
        now: DateTime<Utc>,
        max_attempts: u32,
        new_account: Account,
        new_session: AuthSession,
    ) -> Result<Option<CompletedEmailSignIn>>;

    async fn create_session(&self, session: AuthSession) -> Result<()>;
    async fn session(
        &self,
        account_id: AccountId,
        session_id: SessionId,
    ) -> Result<Option<AuthSession>>;
    async fn list_sessions(&self, account_id: AccountId) -> Result<Vec<AuthSession>>;
    async fn touch_session(
        &self,
        account_id: AccountId,
        session_id: SessionId,
        last_seen_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<bool>;
    async fn revoke_session(
        &self,
        account_id: AccountId,
        session_id: SessionId,
        revoked_at: DateTime<Utc>,
    ) -> Result<bool>;
    async fn revoke_all_sessions(
        &self,
        account_id: AccountId,
        revoked_at: DateTime<Utc>,
    ) -> Result<u64>;

    async fn create_passkey(&self, passkey: PasskeyCredential) -> Result<()>;
    async fn complete_passkey_registration(
        &self,
        challenge_id: ChallengeId,
        passkey: PasskeyCredential,
        now: DateTime<Utc>,
    ) -> Result<bool>;
    async fn complete_passkey_sign_in(
        &self,
        challenge_id: ChallengeId,
        passkey: PasskeyCredential,
        session: AuthSession,
        now: DateTime<Utc>,
    ) -> Result<Option<CompletedPasskeySignIn>>;
    async fn passkey_by_credential_id(
        &self,
        credential_id: &str,
    ) -> Result<Option<PasskeyCredential>>;
    async fn list_passkeys(&self, account_id: AccountId) -> Result<Vec<PasskeyCredential>>;
    async fn revoke_passkey(
        &self,
        account_id: AccountId,
        passkey_id: PasskeyId,
        revoked_at: DateTime<Utc>,
    ) -> Result<bool>;

    async fn create_api_key(&self, api_key: ApiKey) -> Result<()>;
    async fn api_key(&self, account_id: AccountId, key_id: ApiKeyId) -> Result<Option<ApiKey>>;
    async fn list_api_keys(&self, account_id: AccountId) -> Result<Vec<ApiKey>>;
    async fn touch_api_key(
        &self,
        account_id: AccountId,
        key_id: ApiKeyId,
        last_used_at: DateTime<Utc>,
    ) -> Result<bool>;
    async fn revoke_api_key(
        &self,
        account_id: AccountId,
        key_id: ApiKeyId,
        revoked_at: DateTime<Utc>,
    ) -> Result<bool>;
}

pub async fn identity_repository(
    config: &crate::config::Config,
) -> Result<Option<Arc<dyn IdentityRepository>>> {
    use crate::config::{AuthMode, JobBackend};

    if config.auth_mode != AuthMode::Accounts {
        return Ok(None);
    }
    match config.identity_backend {
        JobBackend::Sqlx => Ok(Some(Arc::new(
            crate::sqlx_identity::SqlxIdentityRepository::connect(
                &config.identity_sql_database_url,
            )
            .await?,
        ))),
        JobBackend::DynamoDb => {
            #[cfg(feature = "aws")]
            {
                Ok(Some(Arc::new(
                    crate::dynamodb_identity::DynamoDbIdentityRepository::connect(config).await?,
                )))
            }
            #[cfg(not(feature = "aws"))]
            {
                anyhow::bail!("DynamoDB identity backend requires an AWS-enabled build")
            }
        }
    }
}
