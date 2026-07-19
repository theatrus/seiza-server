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
    /// The API key that minted this session, for Astrometry-compat sessions.
    /// Authentication re-validates the key, so revoking a key immediately
    /// invalidates every session created from it.
    pub api_key_id: Option<ApiKeyId>,
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
    /// Removes a session outright. Used when recycling capped Astrometry
    /// sessions, where a lingering revoked row would have no audit value.
    async fn delete_session(&self, account_id: AccountId, session_id: SessionId) -> Result<()>;

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
    /// Resolves a live (non-revoked) credential. Revocation frees the
    /// credential ID so the same authenticator can be registered again.
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
    /// Revokes the key and every session whose `api_key_id` references it.
    async fn revoke_api_key(
        &self,
        account_id: AccountId,
        key_id: ApiKeyId,
        revoked_at: DateTime<Utc>,
    ) -> Result<bool>;

    /// Removes challenges and sessions that can no longer authenticate and
    /// whose audit grace period has passed, returning how many records were
    /// deleted. DynamoDB deployments rely on per-item TTL and return zero.
    async fn purge_expired(&self, now: DateTime<Utc>) -> Result<u64>;
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

/// Behavioral contract shared by every `IdentityRepository` implementation.
///
/// The SQLx and DynamoDB backends are independent thousand-line
/// implementations of the same trait; this suite pins the invariants that the
/// authentication service relies on so the two cannot drift apart. It runs
/// against SQLite in `sqlx_identity` tests and against a live table in the
/// env-gated `dynamodb_identity` test.
#[cfg(test)]
pub(crate) mod contract {
    use super::*;
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn test_account(email: &str, status: AccountStatus) -> Account {
        let id = Uuid::now_v7();
        let at = now();
        Account {
            id,
            email: email.to_owned(),
            email_lookup: email.to_owned(),
            email_verified_at: at,
            webauthn_user_handle: id.to_string(),
            status,
            created_at: at,
            updated_at: at,
            last_authenticated_at: at,
        }
    }

    fn email_challenge(email: &str) -> AuthChallenge {
        let at = now();
        AuthChallenge {
            id: Uuid::now_v7(),
            purpose: ChallengePurpose::EmailLogin,
            account_id: None,
            email_lookup: Some(email.to_owned()),
            link_token_digest: Some("link-digest".into()),
            code_digest: Some("code-digest".into()),
            webauthn_state_json: None,
            attempts: 0,
            created_at: at,
            expires_at: at + Duration::minutes(10),
            consumed_at: None,
        }
    }

    fn browser_session(account_id: AccountId) -> AuthSession {
        let at = now();
        AuthSession {
            id: Uuid::now_v7(),
            token_digest: format!("digest-{}", Uuid::now_v7()),
            account_id,
            kind: SessionKind::Browser,
            csrf_digest: Some("csrf".into()),
            api_key_id: None,
            created_at: at,
            last_seen_at: at,
            expires_at: at + Duration::days(30),
            absolute_expires_at: at + Duration::days(90),
            revoked_at: None,
        }
    }

    fn astrometry_session(account_id: AccountId, api_key_id: ApiKeyId) -> AuthSession {
        AuthSession {
            kind: SessionKind::Astrometry,
            csrf_digest: None,
            api_key_id: Some(api_key_id),
            ..browser_session(account_id)
        }
    }

    fn test_api_key(account_id: AccountId) -> ApiKey {
        let at = now();
        ApiKey {
            id: Uuid::now_v7(),
            account_id,
            secret_digest: format!("key-digest-{}", Uuid::now_v7()),
            display_prefix: "seiza_key_test…".into(),
            name: "contract".into(),
            scopes: vec!["solve:submit".into()],
            queue_weight: 1.0,
            created_at: at,
            expires_at: None,
            last_used_at: None,
            revoked_at: None,
        }
    }

    fn test_passkey(account_id: AccountId) -> PasskeyCredential {
        PasskeyCredential {
            id: Uuid::now_v7(),
            credential_id: format!("credential-{}", Uuid::now_v7()),
            account_id,
            credential_json: "{}".into(),
            label: "contract key".into(),
            created_at: now(),
            last_used_at: None,
            revoked_at: None,
        }
    }

    fn unique_email(tag: &str) -> String {
        format!("{tag}-{}@contract.example", Uuid::now_v7().simple())
    }

    pub(crate) async fn assert_contract(repository: &dyn IdentityRepository) {
        email_challenges_are_single_use(repository).await;
        completing_stale_challenges_fails_after_eviction(repository).await;
        disabled_accounts_cannot_complete_email_sign_in(repository).await;
        challenge_attempt_limits_are_enforced(repository).await;
        session_lifecycle_is_enforced(repository).await;
        api_key_revocation_cascades_to_its_sessions(repository).await;
        passkey_revocation_frees_the_credential(repository).await;
    }

    async fn email_challenges_are_single_use(repository: &dyn IdentityRepository) {
        let email = unique_email("single-use");
        let challenge = email_challenge(&email);
        repository
            .create_email_challenge(challenge.clone(), 3)
            .await
            .unwrap();
        let account = test_account(&email, AccountStatus::Active);
        let completed = repository
            .complete_email_challenge(
                challenge.id,
                now(),
                5,
                account.clone(),
                browser_session(account.id),
            )
            .await
            .unwrap()
            .expect("first completion succeeds");
        assert!(completed.account_created);
        assert_eq!(completed.account.email_lookup, email);
        // A second completion of the same challenge must fail.
        assert!(
            repository
                .complete_email_challenge(
                    challenge.id,
                    now(),
                    5,
                    test_account(&email, AccountStatus::Active),
                    browser_session(account.id),
                )
                .await
                .unwrap()
                .is_none()
        );
        // Signing in again reuses the account instead of creating another.
        let second = email_challenge(&email);
        repository
            .create_email_challenge(second.clone(), 3)
            .await
            .unwrap();
        let again = repository
            .complete_email_challenge(
                second.id,
                now(),
                5,
                test_account(&email, AccountStatus::Active),
                browser_session(completed.account.id),
            )
            .await
            .unwrap()
            .expect("repeat sign-in succeeds");
        assert!(!again.account_created);
        assert_eq!(again.account.id, completed.account.id);
    }

    async fn completing_stale_challenges_fails_after_eviction(repository: &dyn IdentityRepository) {
        let email = unique_email("eviction");
        let first = email_challenge(&email);
        repository
            .create_email_challenge(first.clone(), 2)
            .await
            .unwrap();
        let second = email_challenge(&email);
        repository
            .create_email_challenge(second.clone(), 2)
            .await
            .unwrap();
        let third = email_challenge(&email);
        repository
            .create_email_challenge(third.clone(), 2)
            .await
            .unwrap();
        // The live limit is two, so the oldest challenge is no longer usable.
        let account = test_account(&email, AccountStatus::Active);
        assert!(
            repository
                .complete_email_challenge(
                    first.id,
                    now(),
                    5,
                    account.clone(),
                    browser_session(account.id),
                )
                .await
                .unwrap()
                .is_none(),
            "evicted challenge must not complete"
        );
        let completed = repository
            .complete_email_challenge(
                third.id,
                now(),
                5,
                account.clone(),
                browser_session(account.id),
            )
            .await
            .unwrap()
            .expect("newest challenge completes");
        // Completion invalidates the other outstanding challenge too.
        assert!(
            repository
                .complete_email_challenge(
                    second.id,
                    now(),
                    5,
                    test_account(&email, AccountStatus::Active),
                    browser_session(completed.account.id),
                )
                .await
                .unwrap()
                .is_none(),
            "sibling challenges are consumed by a successful sign-in"
        );
    }

    async fn disabled_accounts_cannot_complete_email_sign_in(repository: &dyn IdentityRepository) {
        let email = unique_email("disabled");
        let disabled = test_account(&email, AccountStatus::Disabled);
        repository.create_account(disabled.clone()).await.unwrap();
        let challenge = email_challenge(&email);
        repository
            .create_email_challenge(challenge.clone(), 3)
            .await
            .unwrap();
        assert!(
            repository
                .complete_email_challenge(
                    challenge.id,
                    now(),
                    5,
                    test_account(&email, AccountStatus::Active),
                    browser_session(disabled.id),
                )
                .await
                .unwrap()
                .is_none(),
            "disabled accounts must not receive sessions"
        );
    }

    async fn challenge_attempt_limits_are_enforced(repository: &dyn IdentityRepository) {
        let email = unique_email("attempts");
        let challenge = email_challenge(&email);
        repository
            .create_email_challenge(challenge.clone(), 3)
            .await
            .unwrap();
        for _ in 0..2 {
            repository
                .record_challenge_failure(challenge.id, now(), 2)
                .await
                .unwrap();
        }
        // The failure counter is saturated, so recording and completion fail.
        assert!(
            repository
                .record_challenge_failure(challenge.id, now(), 2)
                .await
                .unwrap()
                .is_none()
        );
        let account = test_account(&email, AccountStatus::Active);
        assert!(
            repository
                .complete_email_challenge(
                    challenge.id,
                    now(),
                    2,
                    account.clone(),
                    browser_session(account.id),
                )
                .await
                .unwrap()
                .is_none()
        );
    }

    async fn session_lifecycle_is_enforced(repository: &dyn IdentityRepository) {
        let email = unique_email("sessions");
        let account = test_account(&email, AccountStatus::Active);
        repository.create_account(account.clone()).await.unwrap();
        let session = browser_session(account.id);
        repository.create_session(session.clone()).await.unwrap();
        let touch_at = now();
        assert!(
            repository
                .touch_session(
                    account.id,
                    session.id,
                    touch_at,
                    touch_at + Duration::days(30),
                )
                .await
                .unwrap()
        );
        assert!(
            repository
                .revoke_session(account.id, session.id, now())
                .await
                .unwrap()
        );
        // Revocation is idempotent-false and blocks further touches.
        assert!(
            !repository
                .revoke_session(account.id, session.id, now())
                .await
                .unwrap()
        );
        assert!(
            !repository
                .touch_session(account.id, session.id, now(), now() + Duration::days(30))
                .await
                .unwrap()
        );
        // Revoking a session that never existed must not create one.
        assert!(
            !repository
                .revoke_session(account.id, Uuid::now_v7(), now())
                .await
                .unwrap()
        );
        let other = browser_session(account.id);
        repository.create_session(other.clone()).await.unwrap();
        assert_eq!(
            repository
                .revoke_all_sessions(account.id, now())
                .await
                .unwrap(),
            1
        );
        // Deletion removes the record entirely and tolerates repeats.
        repository
            .delete_session(account.id, other.id)
            .await
            .unwrap();
        assert!(
            repository
                .session(account.id, other.id)
                .await
                .unwrap()
                .is_none()
        );
        repository
            .delete_session(account.id, other.id)
            .await
            .unwrap();
    }

    async fn api_key_revocation_cascades_to_its_sessions(repository: &dyn IdentityRepository) {
        let email = unique_email("api-keys");
        let account = test_account(&email, AccountStatus::Active);
        repository.create_account(account.clone()).await.unwrap();
        let api_key = test_api_key(account.id);
        repository.create_api_key(api_key.clone()).await.unwrap();
        let minted = astrometry_session(account.id, api_key.id);
        repository.create_session(minted.clone()).await.unwrap();
        let browser = browser_session(account.id);
        repository.create_session(browser.clone()).await.unwrap();
        assert!(
            repository
                .revoke_api_key(account.id, api_key.id, now())
                .await
                .unwrap()
        );
        assert!(
            repository
                .api_key(account.id, api_key.id)
                .await
                .unwrap()
                .unwrap()
                .revoked_at
                .is_some()
        );
        let sessions = repository.list_sessions(account.id).await.unwrap();
        let minted_after = sessions.iter().find(|s| s.id == minted.id).unwrap();
        assert!(
            minted_after.revoked_at.is_some(),
            "sessions minted from a revoked key are revoked with it"
        );
        let browser_after = sessions.iter().find(|s| s.id == browser.id).unwrap();
        assert!(browser_after.revoked_at.is_none());
        // Revoking again reports false and revoking an unknown key is a no-op.
        assert!(
            !repository
                .revoke_api_key(account.id, api_key.id, now())
                .await
                .unwrap()
        );
        assert!(
            !repository
                .revoke_api_key(account.id, Uuid::now_v7(), now())
                .await
                .unwrap()
        );
    }

    async fn passkey_revocation_frees_the_credential(repository: &dyn IdentityRepository) {
        let email = unique_email("passkeys");
        let account = test_account(&email, AccountStatus::Active);
        repository.create_account(account.clone()).await.unwrap();
        let passkey = test_passkey(account.id);
        repository.create_passkey(passkey.clone()).await.unwrap();
        assert_eq!(
            repository
                .passkey_by_credential_id(&passkey.credential_id)
                .await
                .unwrap()
                .map(|found| found.id),
            Some(passkey.id)
        );
        assert!(
            repository
                .revoke_passkey(account.id, passkey.id, now())
                .await
                .unwrap()
        );
        assert!(
            !repository
                .revoke_passkey(account.id, passkey.id, now())
                .await
                .unwrap()
        );
        assert!(
            repository
                .passkey_by_credential_id(&passkey.credential_id)
                .await
                .unwrap()
                .is_none(),
            "revocation frees the credential ID"
        );
        // The same authenticator can be registered again.
        let replacement = PasskeyCredential {
            id: Uuid::now_v7(),
            ..test_passkey(account.id)
        };
        let replacement = PasskeyCredential {
            credential_id: passkey.credential_id.clone(),
            ..replacement
        };
        repository
            .create_passkey(replacement.clone())
            .await
            .unwrap();
        assert_eq!(
            repository
                .passkey_by_credential_id(&passkey.credential_id)
                .await
                .unwrap()
                .map(|found| found.id),
            Some(replacement.id)
        );
    }
}
