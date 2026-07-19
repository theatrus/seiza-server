use crate::identity::{
    Account, AccountId, AccountStatus, ApiKey, ApiKeyId, AuthChallenge, AuthSession, ChallengeId,
    ChallengePurpose, CompletedEmailSignIn, CompletedPasskeySignIn, IdentityRepository,
    PasskeyCredential, SessionId, SessionKind,
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{AnyPool, Row, any::AnyPoolOptions};
use uuid::Uuid;

const IDENTITY_MIGRATIONS: &[(&str, &str)] = &[(
    "0001_identity",
    include_str!("../migrations/identity/0001_identity.sql"),
)];

#[derive(Clone)]
pub struct SqlxIdentityRepository {
    pool: AnyPool,
}

impl SqlxIdentityRepository {
    pub async fn connect(database_url: &str) -> Result<Self> {
        sqlx::any::install_default_drivers();
        let sqlite = database_url.starts_with("sqlite:");
        if !sqlite
            && !database_url.starts_with("postgres:")
            && !database_url.starts_with("postgresql:")
        {
            bail!("SEIZA_IDENTITY_SQL_DATABASE_URL must use a sqlite:// or postgres:// URL");
        }
        let pool = AnyPoolOptions::new()
            .max_connections(if sqlite { 1 } else { 12 })
            .connect(database_url)
            .await
            .with_context(|| format!("connecting SQLx identity repository at {database_url}"))?;
        if sqlite {
            sqlx::query("PRAGMA journal_mode = WAL")
                .execute(&pool)
                .await
                .context("enabling SQLite WAL mode for the identity repository")?;
            sqlx::query("PRAGMA foreign_keys = ON")
                .execute(&pool)
                .await
                .context("enabling SQLite foreign keys for the identity repository")?;
        }
        let repository = Self { pool };
        repository.migrate().await?;
        Ok(repository)
    }

    async fn migrate(&self) -> Result<()> {
        sqlx::query("CREATE TABLE IF NOT EXISTS identity_schema_migrations (version TEXT PRIMARY KEY, applied_at TEXT NOT NULL)")
            .execute(&self.pool)
            .await?;
        for (version, migration) in IDENTITY_MIGRATIONS {
            let already_applied =
                sqlx::query("SELECT version FROM identity_schema_migrations WHERE version = $1")
                    .bind(version)
                    .fetch_optional(&self.pool)
                    .await?
                    .is_some();
            if already_applied {
                continue;
            }
            let mut transaction = self.pool.begin().await?;
            sqlx::raw_sql(*migration)
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("applying identity migration {version}"))?;
            sqlx::query(
                "INSERT INTO identity_schema_migrations (version, applied_at) VALUES ($1, $2)",
            )
            .bind(version)
            .bind(encode_time(Utc::now()))
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
        }
        Ok(())
    }
}

#[async_trait]
impl IdentityRepository for SqlxIdentityRepository {
    async fn create_account(&self, account: Account) -> Result<()> {
        sqlx::query("INSERT INTO accounts (id, email, email_lookup, email_verified_at, webauthn_user_handle, status, created_at, updated_at, last_authenticated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)")
            .bind(account.id.to_string())
            .bind(account.email)
            .bind(account.email_lookup)
            .bind(encode_time(account.email_verified_at))
            .bind(account.webauthn_user_handle)
            .bind(account.status.as_str())
            .bind(encode_time(account.created_at))
            .bind(encode_time(account.updated_at))
            .bind(encode_time(account.last_authenticated_at))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn account_by_id(&self, account_id: AccountId) -> Result<Option<Account>> {
        account_query(&self.pool, "id", account_id.to_string()).await
    }

    async fn account_by_email_lookup(&self, email_lookup: &str) -> Result<Option<Account>> {
        account_query(&self.pool, "email_lookup", email_lookup).await
    }

    async fn account_by_user_handle(&self, user_handle: &str) -> Result<Option<Account>> {
        account_query(&self.pool, "webauthn_user_handle", user_handle).await
    }

    async fn create_challenge(&self, challenge: AuthChallenge) -> Result<()> {
        sqlx::query("INSERT INTO auth_challenges (id, purpose, account_id, email_lookup, link_token_digest, code_digest, webauthn_state_json, attempts, created_at, expires_at, consumed_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)")
            .bind(challenge.id.to_string())
            .bind(challenge.purpose.as_str())
            .bind(challenge.account_id.map(|id| id.to_string()))
            .bind(challenge.email_lookup)
            .bind(challenge.link_token_digest)
            .bind(challenge.code_digest)
            .bind(challenge.webauthn_state_json)
            .bind(i64::from(challenge.attempts))
            .bind(encode_time(challenge.created_at))
            .bind(encode_time(challenge.expires_at))
            .bind(challenge.consumed_at.map(encode_time))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn challenge_by_id(&self, challenge_id: ChallengeId) -> Result<Option<AuthChallenge>> {
        sqlx::query("SELECT * FROM auth_challenges WHERE id = $1")
            .bind(challenge_id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .map(challenge_from_row)
            .transpose()
    }

    async fn create_email_challenge(
        &self,
        challenge: AuthChallenge,
        max_live: usize,
    ) -> Result<()> {
        if challenge.purpose != ChallengePurpose::EmailLogin {
            bail!("email challenge storage requires purpose=email-login");
        }
        if max_live == 0 {
            bail!("email challenge live limit must be at least one");
        }
        let email_lookup = challenge
            .email_lookup
            .clone()
            .context("email challenge is missing email_lookup")?;
        let created_at = challenge.created_at;
        let mut transaction = self.pool.begin().await?;
        sqlx::query("INSERT INTO auth_challenges (id, purpose, account_id, email_lookup, link_token_digest, code_digest, webauthn_state_json, attempts, created_at, expires_at, consumed_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)")
            .bind(challenge.id.to_string())
            .bind(challenge.purpose.as_str())
            .bind(challenge.account_id.map(|id| id.to_string()))
            .bind(challenge.email_lookup)
            .bind(challenge.link_token_digest)
            .bind(challenge.code_digest)
            .bind(challenge.webauthn_state_json)
            .bind(i64::from(challenge.attempts))
            .bind(encode_time(challenge.created_at))
            .bind(encode_time(challenge.expires_at))
            .bind(challenge.consumed_at.map(encode_time))
            .execute(&mut *transaction)
            .await?;
        let live = sqlx::query("SELECT id FROM auth_challenges WHERE email_lookup = $1 AND purpose = 'email-login' AND consumed_at IS NULL AND expires_at > $2 ORDER BY created_at DESC")
            .bind(email_lookup)
            .bind(encode_time(created_at))
            .fetch_all(&mut *transaction)
            .await?;
        for row in live.into_iter().skip(max_live) {
            sqlx::query(
                "UPDATE auth_challenges SET consumed_at = $1 WHERE id = $2 AND consumed_at IS NULL",
            )
            .bind(encode_time(created_at))
            .bind(row.try_get::<String, _>("id")?)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn record_challenge_failure(
        &self,
        challenge_id: ChallengeId,
        now: DateTime<Utc>,
        max_attempts: u32,
    ) -> Result<Option<AuthChallenge>> {
        let result = sqlx::query("UPDATE auth_challenges SET attempts = attempts + 1 WHERE id = $1 AND consumed_at IS NULL AND expires_at > $2 AND attempts < $3")
            .bind(challenge_id.to_string())
            .bind(encode_time(now))
            .bind(i64::from(max_attempts))
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.challenge_by_id(challenge_id).await
    }

    async fn complete_email_challenge(
        &self,
        challenge_id: ChallengeId,
        now: DateTime<Utc>,
        max_attempts: u32,
        new_account: Account,
        mut new_session: AuthSession,
    ) -> Result<Option<CompletedEmailSignIn>> {
        let mut transaction = self.pool.begin().await?;
        let challenge = sqlx::query("SELECT * FROM auth_challenges WHERE id = $1")
            .bind(challenge_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?
            .map(challenge_from_row)
            .transpose()?;
        let Some(challenge) = challenge else {
            transaction.rollback().await?;
            return Ok(None);
        };
        let Some(email_lookup) = challenge.email_lookup else {
            transaction.rollback().await?;
            return Ok(None);
        };
        if challenge.purpose != ChallengePurpose::EmailLogin {
            transaction.rollback().await?;
            return Ok(None);
        }
        let consumed = sqlx::query("UPDATE auth_challenges SET consumed_at = $1 WHERE id = $2 AND purpose = 'email-login' AND consumed_at IS NULL AND expires_at > $1 AND attempts < $3")
            .bind(encode_time(now))
            .bind(challenge_id.to_string())
            .bind(i64::from(max_attempts))
            .execute(&mut *transaction)
            .await?;
        if consumed.rows_affected() == 0 {
            transaction.rollback().await?;
            return Ok(None);
        }

        let existing = sqlx::query("SELECT * FROM accounts WHERE email_lookup = $1")
            .bind(&email_lookup)
            .fetch_optional(&mut *transaction)
            .await?
            .map(account_from_row)
            .transpose()?;
        let (mut account, account_created) = if let Some(account) = existing {
            (account, false)
        } else {
            let inserted = sqlx::query("INSERT INTO accounts (id, email, email_lookup, email_verified_at, webauthn_user_handle, status, created_at, updated_at, last_authenticated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT(email_lookup) DO NOTHING")
                .bind(new_account.id.to_string())
                .bind(&new_account.email)
                .bind(&new_account.email_lookup)
                .bind(encode_time(new_account.email_verified_at))
                .bind(&new_account.webauthn_user_handle)
                .bind(new_account.status.as_str())
                .bind(encode_time(new_account.created_at))
                .bind(encode_time(new_account.updated_at))
                .bind(encode_time(new_account.last_authenticated_at))
                .execute(&mut *transaction)
                .await?;
            let account = sqlx::query("SELECT * FROM accounts WHERE email_lookup = $1")
                .bind(&email_lookup)
                .fetch_one(&mut *transaction)
                .await
                .map(account_from_row)??;
            (account, inserted.rows_affected() == 1)
        };

        new_session.account_id = account.id;
        sqlx::query("INSERT INTO auth_sessions (id, token_digest, account_id, kind, csrf_digest, created_at, last_seen_at, expires_at, absolute_expires_at, revoked_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)")
            .bind(new_session.id.to_string())
            .bind(&new_session.token_digest)
            .bind(new_session.account_id.to_string())
            .bind(new_session.kind.as_str())
            .bind(&new_session.csrf_digest)
            .bind(encode_time(new_session.created_at))
            .bind(encode_time(new_session.last_seen_at))
            .bind(encode_time(new_session.expires_at))
            .bind(encode_time(new_session.absolute_expires_at))
            .bind(new_session.revoked_at.map(encode_time))
            .execute(&mut *transaction)
            .await?;
        sqlx::query("UPDATE auth_challenges SET consumed_at = $1 WHERE email_lookup = $2 AND purpose = 'email-login' AND consumed_at IS NULL")
            .bind(encode_time(now))
            .bind(&email_lookup)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "UPDATE accounts SET updated_at = $1, last_authenticated_at = $1 WHERE id = $2",
        )
        .bind(encode_time(now))
        .bind(account.id.to_string())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        account.updated_at = now;
        account.last_authenticated_at = now;
        Ok(Some(CompletedEmailSignIn {
            account,
            session: new_session,
            account_created,
        }))
    }

    async fn create_session(&self, session: AuthSession) -> Result<()> {
        sqlx::query("INSERT INTO auth_sessions (id, token_digest, account_id, kind, csrf_digest, created_at, last_seen_at, expires_at, absolute_expires_at, revoked_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)")
            .bind(session.id.to_string())
            .bind(session.token_digest)
            .bind(session.account_id.to_string())
            .bind(session.kind.as_str())
            .bind(session.csrf_digest)
            .bind(encode_time(session.created_at))
            .bind(encode_time(session.last_seen_at))
            .bind(encode_time(session.expires_at))
            .bind(encode_time(session.absolute_expires_at))
            .bind(session.revoked_at.map(encode_time))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn session(
        &self,
        account_id: AccountId,
        session_id: SessionId,
    ) -> Result<Option<AuthSession>> {
        sqlx::query("SELECT * FROM auth_sessions WHERE id = $1 AND account_id = $2")
            .bind(session_id.to_string())
            .bind(account_id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .map(session_from_row)
            .transpose()
    }

    async fn list_sessions(&self, account_id: AccountId) -> Result<Vec<AuthSession>> {
        sqlx::query("SELECT * FROM auth_sessions WHERE account_id = $1 ORDER BY created_at DESC")
            .bind(account_id.to_string())
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(session_from_row)
            .collect()
    }

    async fn touch_session(
        &self,
        account_id: AccountId,
        session_id: SessionId,
        last_seen_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<bool> {
        let result = sqlx::query("UPDATE auth_sessions SET last_seen_at = $1, expires_at = $2 WHERE id = $3 AND account_id = $4 AND revoked_at IS NULL AND absolute_expires_at > $1")
            .bind(encode_time(last_seen_at))
            .bind(encode_time(expires_at))
            .bind(session_id.to_string())
            .bind(account_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn revoke_session(
        &self,
        account_id: AccountId,
        session_id: SessionId,
        revoked_at: DateTime<Utc>,
    ) -> Result<bool> {
        let result = sqlx::query("UPDATE auth_sessions SET revoked_at = $1 WHERE id = $2 AND account_id = $3 AND revoked_at IS NULL")
            .bind(encode_time(revoked_at))
            .bind(session_id.to_string())
            .bind(account_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn revoke_all_sessions(
        &self,
        account_id: AccountId,
        revoked_at: DateTime<Utc>,
    ) -> Result<u64> {
        Ok(sqlx::query(
            "UPDATE auth_sessions SET revoked_at = $1 WHERE account_id = $2 AND revoked_at IS NULL",
        )
        .bind(encode_time(revoked_at))
        .bind(account_id.to_string())
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    async fn create_passkey(&self, passkey: PasskeyCredential) -> Result<()> {
        sqlx::query("INSERT INTO passkey_credentials (id, credential_id, account_id, credential_json, label, created_at, last_used_at, revoked_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
            .bind(passkey.id.to_string())
            .bind(passkey.credential_id)
            .bind(passkey.account_id.to_string())
            .bind(passkey.credential_json)
            .bind(passkey.label)
            .bind(encode_time(passkey.created_at))
            .bind(passkey.last_used_at.map(encode_time))
            .bind(passkey.revoked_at.map(encode_time))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn complete_passkey_registration(
        &self,
        challenge_id: ChallengeId,
        passkey: PasskeyCredential,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let mut transaction = self.pool.begin().await?;
        let consumed = sqlx::query("UPDATE auth_challenges SET consumed_at = $1 WHERE id = $2 AND purpose = 'passkey-registration' AND account_id = $3 AND consumed_at IS NULL AND expires_at > $1 AND EXISTS (SELECT 1 FROM accounts WHERE id = $3 AND status = 'active')")
            .bind(encode_time(now))
            .bind(challenge_id.to_string())
            .bind(passkey.account_id.to_string())
            .execute(&mut *transaction)
            .await?;
        if consumed.rows_affected() == 0 {
            transaction.rollback().await?;
            return Ok(false);
        }
        sqlx::query("INSERT INTO passkey_credentials (id, credential_id, account_id, credential_json, label, created_at, last_used_at, revoked_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
            .bind(passkey.id.to_string())
            .bind(passkey.credential_id)
            .bind(passkey.account_id.to_string())
            .bind(passkey.credential_json)
            .bind(passkey.label)
            .bind(encode_time(passkey.created_at))
            .bind(passkey.last_used_at.map(encode_time))
            .bind(passkey.revoked_at.map(encode_time))
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(true)
    }

    async fn complete_passkey_sign_in(
        &self,
        challenge_id: ChallengeId,
        passkey: PasskeyCredential,
        session: AuthSession,
        now: DateTime<Utc>,
    ) -> Result<Option<CompletedPasskeySignIn>> {
        if passkey.account_id != session.account_id {
            bail!("passkey and session account IDs differ");
        }
        let mut transaction = self.pool.begin().await?;
        let account = sqlx::query("SELECT * FROM accounts WHERE id = $1 AND status = 'active'")
            .bind(passkey.account_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?
            .map(account_from_row)
            .transpose()?;
        let Some(mut account) = account else {
            transaction.rollback().await?;
            return Ok(None);
        };
        let consumed = sqlx::query("UPDATE auth_challenges SET consumed_at = $1 WHERE id = $2 AND purpose = 'passkey-authentication' AND consumed_at IS NULL AND expires_at > $1")
            .bind(encode_time(now))
            .bind(challenge_id.to_string())
            .execute(&mut *transaction)
            .await?;
        if consumed.rows_affected() == 0 {
            transaction.rollback().await?;
            return Ok(None);
        }
        let updated = sqlx::query("UPDATE passkey_credentials SET credential_json = $1, last_used_at = $2 WHERE id = $3 AND credential_id = $4 AND account_id = $5 AND revoked_at IS NULL")
            .bind(&passkey.credential_json)
            .bind(encode_time(now))
            .bind(passkey.id.to_string())
            .bind(&passkey.credential_id)
            .bind(passkey.account_id.to_string())
            .execute(&mut *transaction)
            .await?;
        if updated.rows_affected() == 0 {
            transaction.rollback().await?;
            return Ok(None);
        }
        sqlx::query("INSERT INTO auth_sessions (id, token_digest, account_id, kind, csrf_digest, created_at, last_seen_at, expires_at, absolute_expires_at, revoked_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)")
            .bind(session.id.to_string())
            .bind(&session.token_digest)
            .bind(session.account_id.to_string())
            .bind(session.kind.as_str())
            .bind(&session.csrf_digest)
            .bind(encode_time(session.created_at))
            .bind(encode_time(session.last_seen_at))
            .bind(encode_time(session.expires_at))
            .bind(encode_time(session.absolute_expires_at))
            .bind(session.revoked_at.map(encode_time))
            .execute(&mut *transaction)
            .await?;
        sqlx::query("UPDATE accounts SET updated_at = $1, last_authenticated_at = $1 WHERE id = $2 AND status = 'active'")
            .bind(encode_time(now))
            .bind(passkey.account_id.to_string())
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        account.updated_at = now;
        account.last_authenticated_at = now;
        let mut passkey = passkey;
        passkey.last_used_at = Some(now);
        Ok(Some(CompletedPasskeySignIn {
            account,
            session,
            passkey,
        }))
    }

    async fn passkey_by_credential_id(
        &self,
        credential_id: &str,
    ) -> Result<Option<PasskeyCredential>> {
        sqlx::query("SELECT * FROM passkey_credentials WHERE credential_id = $1")
            .bind(credential_id)
            .fetch_optional(&self.pool)
            .await?
            .map(passkey_from_row)
            .transpose()
    }

    async fn list_passkeys(&self, account_id: AccountId) -> Result<Vec<PasskeyCredential>> {
        sqlx::query(
            "SELECT * FROM passkey_credentials WHERE account_id = $1 ORDER BY created_at ASC",
        )
        .bind(account_id.to_string())
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(passkey_from_row)
        .collect()
    }

    async fn revoke_passkey(
        &self,
        account_id: AccountId,
        passkey_id: crate::identity::PasskeyId,
        revoked_at: DateTime<Utc>,
    ) -> Result<bool> {
        Ok(sqlx::query("UPDATE passkey_credentials SET revoked_at = $1 WHERE id = $2 AND account_id = $3 AND revoked_at IS NULL")
            .bind(encode_time(revoked_at))
            .bind(passkey_id.to_string())
            .bind(account_id.to_string())
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    async fn create_api_key(&self, api_key: ApiKey) -> Result<()> {
        sqlx::query("INSERT INTO api_keys (id, account_id, secret_digest, display_prefix, name, scopes_json, queue_weight, created_at, expires_at, last_used_at, revoked_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)")
            .bind(api_key.id.to_string())
            .bind(api_key.account_id.to_string())
            .bind(api_key.secret_digest)
            .bind(api_key.display_prefix)
            .bind(api_key.name)
            .bind(serde_json::to_string(&api_key.scopes)?)
            .bind(api_key.queue_weight)
            .bind(encode_time(api_key.created_at))
            .bind(api_key.expires_at.map(encode_time))
            .bind(api_key.last_used_at.map(encode_time))
            .bind(api_key.revoked_at.map(encode_time))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn api_key(&self, account_id: AccountId, key_id: ApiKeyId) -> Result<Option<ApiKey>> {
        sqlx::query("SELECT * FROM api_keys WHERE id = $1 AND account_id = $2")
            .bind(key_id.to_string())
            .bind(account_id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .map(api_key_from_row)
            .transpose()
    }

    async fn list_api_keys(&self, account_id: AccountId) -> Result<Vec<ApiKey>> {
        sqlx::query("SELECT * FROM api_keys WHERE account_id = $1 ORDER BY created_at ASC")
            .bind(account_id.to_string())
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(api_key_from_row)
            .collect()
    }

    async fn touch_api_key(
        &self,
        account_id: AccountId,
        key_id: ApiKeyId,
        last_used_at: DateTime<Utc>,
    ) -> Result<bool> {
        Ok(sqlx::query("UPDATE api_keys SET last_used_at = $1 WHERE id = $2 AND account_id = $3 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > $1)")
            .bind(encode_time(last_used_at))
            .bind(key_id.to_string())
            .bind(account_id.to_string())
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    async fn revoke_api_key(
        &self,
        account_id: AccountId,
        key_id: ApiKeyId,
        revoked_at: DateTime<Utc>,
    ) -> Result<bool> {
        Ok(sqlx::query("UPDATE api_keys SET revoked_at = $1 WHERE id = $2 AND account_id = $3 AND revoked_at IS NULL")
            .bind(encode_time(revoked_at))
            .bind(key_id.to_string())
            .bind(account_id.to_string())
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }
}

async fn account_query(
    pool: &AnyPool,
    column: &str,
    value: impl Into<String>,
) -> Result<Option<Account>> {
    let sql = match column {
        "id" => "SELECT * FROM accounts WHERE id = $1",
        "email_lookup" => "SELECT * FROM accounts WHERE email_lookup = $1",
        "webauthn_user_handle" => "SELECT * FROM accounts WHERE webauthn_user_handle = $1",
        _ => unreachable!("account lookup columns are fixed"),
    };
    sqlx::query(sql)
        .bind(value.into())
        .fetch_optional(pool)
        .await?
        .map(account_from_row)
        .transpose()
}

fn account_from_row(row: sqlx::any::AnyRow) -> Result<Account> {
    Ok(Account {
        id: decode_uuid(&row.try_get::<String, _>("id")?, "account ID")?,
        email: row.try_get("email")?,
        email_lookup: row.try_get("email_lookup")?,
        email_verified_at: decode_time(&row.try_get::<String, _>("email_verified_at")?)?,
        webauthn_user_handle: row.try_get("webauthn_user_handle")?,
        status: AccountStatus::parse(&row.try_get::<String, _>("status")?)?,
        created_at: decode_time(&row.try_get::<String, _>("created_at")?)?,
        updated_at: decode_time(&row.try_get::<String, _>("updated_at")?)?,
        last_authenticated_at: decode_time(&row.try_get::<String, _>("last_authenticated_at")?)?,
    })
}

fn challenge_from_row(row: sqlx::any::AnyRow) -> Result<AuthChallenge> {
    Ok(AuthChallenge {
        id: decode_uuid(&row.try_get::<String, _>("id")?, "challenge ID")?,
        purpose: ChallengePurpose::parse(&row.try_get::<String, _>("purpose")?)?,
        account_id: optional_uuid(row.try_get("account_id")?, "challenge account ID")?,
        email_lookup: row.try_get("email_lookup")?,
        link_token_digest: row.try_get("link_token_digest")?,
        code_digest: row.try_get("code_digest")?,
        webauthn_state_json: row.try_get("webauthn_state_json")?,
        attempts: u32::try_from(row.try_get::<i64, _>("attempts")?)
            .context("challenge attempts are outside u32")?,
        created_at: decode_time(&row.try_get::<String, _>("created_at")?)?,
        expires_at: decode_time(&row.try_get::<String, _>("expires_at")?)?,
        consumed_at: optional_time(row.try_get("consumed_at")?)?,
    })
}

fn session_from_row(row: sqlx::any::AnyRow) -> Result<AuthSession> {
    Ok(AuthSession {
        id: decode_uuid(&row.try_get::<String, _>("id")?, "session ID")?,
        token_digest: row.try_get("token_digest")?,
        account_id: decode_uuid(
            &row.try_get::<String, _>("account_id")?,
            "session account ID",
        )?,
        kind: SessionKind::parse(&row.try_get::<String, _>("kind")?)?,
        csrf_digest: row.try_get("csrf_digest")?,
        created_at: decode_time(&row.try_get::<String, _>("created_at")?)?,
        last_seen_at: decode_time(&row.try_get::<String, _>("last_seen_at")?)?,
        expires_at: decode_time(&row.try_get::<String, _>("expires_at")?)?,
        absolute_expires_at: decode_time(&row.try_get::<String, _>("absolute_expires_at")?)?,
        revoked_at: optional_time(row.try_get("revoked_at")?)?,
    })
}

fn passkey_from_row(row: sqlx::any::AnyRow) -> Result<PasskeyCredential> {
    Ok(PasskeyCredential {
        id: decode_uuid(&row.try_get::<String, _>("id")?, "passkey ID")?,
        credential_id: row.try_get("credential_id")?,
        account_id: decode_uuid(
            &row.try_get::<String, _>("account_id")?,
            "passkey account ID",
        )?,
        credential_json: row.try_get("credential_json")?,
        label: row.try_get("label")?,
        created_at: decode_time(&row.try_get::<String, _>("created_at")?)?,
        last_used_at: optional_time(row.try_get("last_used_at")?)?,
        revoked_at: optional_time(row.try_get("revoked_at")?)?,
    })
}

fn api_key_from_row(row: sqlx::any::AnyRow) -> Result<ApiKey> {
    Ok(ApiKey {
        id: decode_uuid(&row.try_get::<String, _>("id")?, "API key ID")?,
        account_id: decode_uuid(
            &row.try_get::<String, _>("account_id")?,
            "API key account ID",
        )?,
        secret_digest: row.try_get("secret_digest")?,
        display_prefix: row.try_get("display_prefix")?,
        name: row.try_get("name")?,
        scopes: serde_json::from_str(&row.try_get::<String, _>("scopes_json")?)?,
        queue_weight: row.try_get("queue_weight")?,
        created_at: decode_time(&row.try_get::<String, _>("created_at")?)?,
        expires_at: optional_time(row.try_get("expires_at")?)?,
        last_used_at: optional_time(row.try_get("last_used_at")?)?,
        revoked_at: optional_time(row.try_get("revoked_at")?)?,
    })
}

fn encode_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

fn decode_time(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn optional_time(value: Option<String>) -> Result<Option<DateTime<Utc>>> {
    value.as_deref().map(decode_time).transpose()
}

fn decode_uuid(value: &str, field: &str) -> Result<Uuid> {
    Uuid::parse_str(value).with_context(|| format!("{field} is not a UUID"))
}

fn optional_uuid(value: Option<String>, field: &str) -> Result<Option<Uuid>> {
    value
        .as_deref()
        .map(|value| decode_uuid(value, field))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{AccountStatus, ChallengePurpose, SessionKind};
    use chrono::Duration;

    fn account(now: DateTime<Utc>) -> Account {
        Account {
            id: Uuid::now_v7(),
            email: "astronomer@example.com".into(),
            email_lookup: "astronomer@example.com".into(),
            email_verified_at: now,
            webauthn_user_handle: "random-user-handle".into(),
            status: AccountStatus::Active,
            created_at: now,
            updated_at: now,
            last_authenticated_at: now,
        }
    }

    #[tokio::test]
    async fn sqlite_contract_uses_direct_account_scoped_credentials() {
        let repository = SqlxIdentityRepository::connect("sqlite::memory:")
            .await
            .unwrap();
        let now = Utc::now();
        let account = account(now);
        repository.create_account(account.clone()).await.unwrap();

        assert_eq!(
            repository.account_by_id(account.id).await.unwrap(),
            Some(account.clone())
        );
        assert_eq!(
            repository
                .account_by_email_lookup(&account.email_lookup)
                .await
                .unwrap(),
            Some(account.clone())
        );
        assert_eq!(
            repository
                .account_by_user_handle(&account.webauthn_user_handle)
                .await
                .unwrap(),
            Some(account.clone())
        );

        let challenge = AuthChallenge {
            id: Uuid::now_v7(),
            purpose: ChallengePurpose::EmailLogin,
            account_id: None,
            email_lookup: Some(account.email_lookup.clone()),
            link_token_digest: Some("link-digest".into()),
            code_digest: Some("code-digest".into()),
            webauthn_state_json: None,
            attempts: 0,
            created_at: now,
            expires_at: now + Duration::minutes(10),
            consumed_at: None,
        };
        repository
            .create_challenge(challenge.clone())
            .await
            .unwrap();
        assert_eq!(
            repository.challenge_by_id(challenge.id).await.unwrap(),
            Some(challenge)
        );

        let session = AuthSession {
            id: Uuid::now_v7(),
            token_digest: "session-digest".into(),
            account_id: account.id,
            kind: SessionKind::Browser,
            csrf_digest: Some("csrf-digest".into()),
            created_at: now,
            last_seen_at: now,
            expires_at: now + Duration::days(30),
            absolute_expires_at: now + Duration::days(90),
            revoked_at: None,
        };
        repository.create_session(session.clone()).await.unwrap();
        assert_eq!(
            repository.session(account.id, session.id).await.unwrap(),
            Some(session.clone())
        );
        assert_eq!(
            repository.list_sessions(account.id).await.unwrap(),
            vec![session]
        );

        let passkey = PasskeyCredential {
            id: Uuid::now_v7(),
            credential_id: "credential-id".into(),
            account_id: account.id,
            credential_json: "{}".into(),
            label: "Laptop".into(),
            created_at: now,
            last_used_at: None,
            revoked_at: None,
        };
        repository.create_passkey(passkey.clone()).await.unwrap();
        assert_eq!(
            repository
                .passkey_by_credential_id(&passkey.credential_id)
                .await
                .unwrap(),
            Some(passkey.clone())
        );
        assert_eq!(
            repository.list_passkeys(account.id).await.unwrap(),
            vec![passkey]
        );

        let api_key = ApiKey {
            id: Uuid::now_v7(),
            account_id: account.id,
            secret_digest: "key-digest".into(),
            display_prefix: "seiza_key_abc".into(),
            name: "Observatory".into(),
            scopes: vec!["solve:submit".into()],
            queue_weight: 1.0,
            created_at: now,
            expires_at: None,
            last_used_at: None,
            revoked_at: None,
        };
        repository.create_api_key(api_key.clone()).await.unwrap();
        assert_eq!(
            repository.api_key(account.id, api_key.id).await.unwrap(),
            Some(api_key.clone())
        );
        assert_eq!(
            repository.list_api_keys(account.id).await.unwrap(),
            vec![api_key]
        );
    }

    #[tokio::test]
    async fn sqlite_constraints_reject_duplicate_email_and_credential_ids() {
        let repository = SqlxIdentityRepository::connect("sqlite::memory:")
            .await
            .unwrap();
        let now = Utc::now();
        let first = account(now);
        repository.create_account(first.clone()).await.unwrap();
        let mut duplicate = account(now);
        duplicate.email_lookup = first.email_lookup.clone();
        assert!(repository.create_account(duplicate).await.is_err());

        let passkey = PasskeyCredential {
            id: Uuid::now_v7(),
            credential_id: "duplicate-credential".into(),
            account_id: first.id,
            credential_json: "{}".into(),
            label: "First".into(),
            created_at: now,
            last_used_at: None,
            revoked_at: None,
        };
        repository.create_passkey(passkey.clone()).await.unwrap();
        let mut duplicate = passkey;
        duplicate.id = Uuid::now_v7();
        assert!(repository.create_passkey(duplicate).await.is_err());
    }

    #[tokio::test]
    async fn sqlite_passkey_ceremonies_consume_challenges_atomically() {
        let repository = SqlxIdentityRepository::connect("sqlite::memory:")
            .await
            .unwrap();
        let now = Utc::now();
        let account = account(now);
        repository.create_account(account.clone()).await.unwrap();
        let registration = AuthChallenge {
            id: Uuid::now_v7(),
            purpose: ChallengePurpose::PasskeyRegistration,
            account_id: Some(account.id),
            email_lookup: None,
            link_token_digest: None,
            code_digest: None,
            webauthn_state_json: Some("registration-state".into()),
            attempts: 0,
            created_at: now,
            expires_at: now + Duration::minutes(10),
            consumed_at: None,
        };
        repository
            .create_challenge(registration.clone())
            .await
            .unwrap();
        let passkey = PasskeyCredential {
            id: Uuid::now_v7(),
            credential_id: "atomic-credential".into(),
            account_id: account.id,
            credential_json: "credential-v1".into(),
            label: "Laptop".into(),
            created_at: now,
            last_used_at: None,
            revoked_at: None,
        };
        assert!(
            repository
                .complete_passkey_registration(registration.id, passkey.clone(), now)
                .await
                .unwrap()
        );
        assert!(
            !repository
                .complete_passkey_registration(registration.id, passkey.clone(), now)
                .await
                .unwrap()
        );

        let authentication = AuthChallenge {
            id: Uuid::now_v7(),
            purpose: ChallengePurpose::PasskeyAuthentication,
            account_id: None,
            email_lookup: None,
            link_token_digest: None,
            code_digest: None,
            webauthn_state_json: Some("authentication-state".into()),
            attempts: 0,
            created_at: now,
            expires_at: now + Duration::minutes(10),
            consumed_at: None,
        };
        repository
            .create_challenge(authentication.clone())
            .await
            .unwrap();
        let mut updated_passkey = passkey.clone();
        updated_passkey.credential_json = "credential-v2".into();
        let session = AuthSession {
            id: Uuid::now_v7(),
            token_digest: "passkey-session".into(),
            account_id: account.id,
            kind: SessionKind::Browser,
            csrf_digest: Some("passkey-csrf".into()),
            created_at: now,
            last_seen_at: now,
            expires_at: now + Duration::days(30),
            absolute_expires_at: now + Duration::days(90),
            revoked_at: None,
        };
        let completed = repository
            .complete_passkey_sign_in(authentication.id, updated_passkey, session.clone(), now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.session, session);
        assert_eq!(completed.passkey.last_used_at, Some(now));
        assert!(
            repository
                .complete_passkey_sign_in(
                    authentication.id,
                    passkey.clone(),
                    completed.session,
                    now,
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repository
                .revoke_passkey(account.id, passkey.id, now)
                .await
                .unwrap()
        );
        assert!(
            repository
                .passkey_by_credential_id(&passkey.credential_id)
                .await
                .unwrap()
                .unwrap()
                .revoked_at
                .is_some()
        );
    }
}
