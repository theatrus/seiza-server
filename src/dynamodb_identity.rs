use crate::{
    config::Config,
    identity::{
        Account, AccountId, AccountStatus, ApiKey, ApiKeyId, AuthChallenge, AuthSession,
        ChallengeId, ChallengePurpose, IdentityRepository, PasskeyCredential, SessionId,
        SessionKind,
    },
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_dynamodb::{
    Client,
    types::{AttributeValue, ConditionCheck, Put, TransactWriteItem},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use uuid::Uuid;

type Item = HashMap<String, AttributeValue>;

pub struct DynamoDbIdentityRepository {
    client: Client,
    table: String,
}

impl DynamoDbIdentityRepository {
    pub async fn connect(config: &Config) -> Result<Self> {
        let table = config.identity_dynamodb_table.clone().context(
            "SEIZA_IDENTITY_DYNAMODB_TABLE is required for the DynamoDB identity backend",
        )?;
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        Ok(Self {
            client: Client::new(&sdk_config),
            table,
        })
    }

    async fn get_item(&self, pk: String, sk: String) -> Result<Option<Item>> {
        Ok(self
            .client
            .get_item()
            .table_name(&self.table)
            .key("pk", string(pk))
            .key("sk", string(sk))
            .consistent_read(true)
            .send()
            .await?
            .item)
    }

    async fn account_from_alias(&self, pk: String) -> Result<Option<Account>> {
        let alias = self.get_item(pk, "ACCOUNT".into()).await?;
        let Some(account_id) = alias
            .as_ref()
            .map(|item| required_uuid(item, "account_id"))
            .transpose()?
        else {
            return Ok(None);
        };
        self.account_by_id(account_id).await
    }

    async fn account_items(&self, account_id: AccountId, prefix: &str) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        let mut start_key = None;
        loop {
            let mut request = self
                .client
                .query()
                .table_name(&self.table)
                .consistent_read(true)
                .key_condition_expression("pk = :pk AND begins_with(sk, :prefix)")
                .expression_attribute_values(":pk", string(account_key(account_id)))
                .expression_attribute_values(":prefix", string(prefix));
            if let Some(start_key) = start_key {
                request = request.set_exclusive_start_key(Some(start_key));
            }
            let output = request.send().await?;
            items.extend(output.items.unwrap_or_default());
            start_key = output.last_evaluated_key;
            if start_key.is_none() {
                return Ok(items);
            }
        }
    }

    fn active_account_check(&self, account_id: AccountId) -> Result<ConditionCheck> {
        Ok(ConditionCheck::builder()
            .table_name(&self.table)
            .key("pk", string(account_key(account_id)))
            .key("sk", string("PROFILE"))
            .condition_expression("#status = :active")
            .expression_attribute_names("#status", "status")
            .expression_attribute_values(":active", string(AccountStatus::Active.as_str()))
            .build()?)
    }
}

#[async_trait]
impl IdentityRepository for DynamoDbIdentityRepository {
    async fn create_account(&self, account: Account) -> Result<()> {
        let profile = Put::builder()
            .table_name(&self.table)
            .set_item(Some(account_item(&account)))
            .condition_expression("attribute_not_exists(pk)")
            .build()?;
        let email_alias = Put::builder()
            .table_name(&self.table)
            .set_item(Some(alias_item(
                email_key(&account.email_lookup),
                account.id,
            )))
            .condition_expression("attribute_not_exists(pk)")
            .build()?;
        let user_alias = Put::builder()
            .table_name(&self.table)
            .set_item(Some(alias_item(
                user_key(&account.webauthn_user_handle),
                account.id,
            )))
            .condition_expression("attribute_not_exists(pk)")
            .build()?;
        self.client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().put(profile).build())
            .transact_items(TransactWriteItem::builder().put(email_alias).build())
            .transact_items(TransactWriteItem::builder().put(user_alias).build())
            .send()
            .await
            .context("creating DynamoDB account and unique aliases")?;
        Ok(())
    }

    async fn account_by_id(&self, account_id: AccountId) -> Result<Option<Account>> {
        self.get_item(account_key(account_id), "PROFILE".into())
            .await?
            .as_ref()
            .map(account_from_item)
            .transpose()
    }

    async fn account_by_email_lookup(&self, email_lookup: &str) -> Result<Option<Account>> {
        self.account_from_alias(email_key(email_lookup)).await
    }

    async fn account_by_user_handle(&self, user_handle: &str) -> Result<Option<Account>> {
        self.account_from_alias(user_key(user_handle)).await
    }

    async fn create_challenge(&self, challenge: AuthChallenge) -> Result<()> {
        self.client
            .put_item()
            .table_name(&self.table)
            .set_item(Some(challenge_item(&challenge)))
            .condition_expression("attribute_not_exists(pk)")
            .send()
            .await?;
        Ok(())
    }

    async fn challenge_by_id(&self, challenge_id: ChallengeId) -> Result<Option<AuthChallenge>> {
        self.get_item(challenge_key(challenge_id), "CHALLENGE".into())
            .await?
            .as_ref()
            .map(challenge_from_item)
            .transpose()
    }

    async fn create_session(&self, session: AuthSession) -> Result<()> {
        let put = Put::builder()
            .table_name(&self.table)
            .set_item(Some(session_item(&session)))
            .condition_expression("attribute_not_exists(pk) AND attribute_not_exists(sk)")
            .build()?;
        self.client
            .transact_write_items()
            .transact_items(
                TransactWriteItem::builder()
                    .condition_check(self.active_account_check(session.account_id)?)
                    .build(),
            )
            .transact_items(TransactWriteItem::builder().put(put).build())
            .send()
            .await?;
        Ok(())
    }

    async fn session(
        &self,
        account_id: AccountId,
        session_id: SessionId,
    ) -> Result<Option<AuthSession>> {
        self.get_item(account_key(account_id), session_key(session_id))
            .await?
            .as_ref()
            .map(session_from_item)
            .transpose()
    }

    async fn list_sessions(&self, account_id: AccountId) -> Result<Vec<AuthSession>> {
        self.account_items(account_id, "SESSION#")
            .await?
            .iter()
            .map(session_from_item)
            .collect()
    }

    async fn create_passkey(&self, passkey: PasskeyCredential) -> Result<()> {
        let credential_key = credential_key(&passkey.credential_id);
        let passkey_put = Put::builder()
            .table_name(&self.table)
            .set_item(Some(passkey_item(&passkey)))
            .condition_expression("attribute_not_exists(pk) AND attribute_not_exists(sk)")
            .build()?;
        let credential_put = Put::builder()
            .table_name(&self.table)
            .set_item(Some(HashMap::from([
                ("pk".into(), string(credential_key)),
                ("sk".into(), string("ACCOUNT")),
                ("entity".into(), string("passkey_lookup")),
                ("account_id".into(), string(passkey.account_id)),
                ("passkey_id".into(), string(passkey.id)),
            ])))
            .condition_expression("attribute_not_exists(pk)")
            .build()?;
        self.client
            .transact_write_items()
            .transact_items(
                TransactWriteItem::builder()
                    .condition_check(self.active_account_check(passkey.account_id)?)
                    .build(),
            )
            .transact_items(TransactWriteItem::builder().put(passkey_put).build())
            .transact_items(TransactWriteItem::builder().put(credential_put).build())
            .send()
            .await
            .context("creating DynamoDB passkey and credential lookup")?;
        Ok(())
    }

    async fn passkey_by_credential_id(
        &self,
        credential_id: &str,
    ) -> Result<Option<PasskeyCredential>> {
        let lookup = self
            .get_item(credential_key(credential_id), "ACCOUNT".into())
            .await?;
        let Some((account_id, passkey_id)) = lookup
            .as_ref()
            .map(|item| {
                Ok::<_, anyhow::Error>((
                    required_uuid(item, "account_id")?,
                    required_uuid(item, "passkey_id")?,
                ))
            })
            .transpose()?
        else {
            return Ok(None);
        };
        self.get_item(account_key(account_id), passkey_key(passkey_id))
            .await?
            .as_ref()
            .map(passkey_from_item)
            .transpose()
    }

    async fn list_passkeys(&self, account_id: AccountId) -> Result<Vec<PasskeyCredential>> {
        self.account_items(account_id, "PASSKEY#")
            .await?
            .iter()
            .map(passkey_from_item)
            .collect()
    }

    async fn create_api_key(&self, api_key: ApiKey) -> Result<()> {
        let put = Put::builder()
            .table_name(&self.table)
            .set_item(Some(api_key_item(&api_key)?))
            .condition_expression("attribute_not_exists(pk) AND attribute_not_exists(sk)")
            .build()?;
        self.client
            .transact_write_items()
            .transact_items(
                TransactWriteItem::builder()
                    .condition_check(self.active_account_check(api_key.account_id)?)
                    .build(),
            )
            .transact_items(TransactWriteItem::builder().put(put).build())
            .send()
            .await?;
        Ok(())
    }

    async fn api_key(&self, account_id: AccountId, key_id: ApiKeyId) -> Result<Option<ApiKey>> {
        self.get_item(account_key(account_id), api_key_key(key_id))
            .await?
            .as_ref()
            .map(api_key_from_item)
            .transpose()
    }

    async fn list_api_keys(&self, account_id: AccountId) -> Result<Vec<ApiKey>> {
        self.account_items(account_id, "APIKEY#")
            .await?
            .iter()
            .map(api_key_from_item)
            .collect()
    }
}

fn account_item(account: &Account) -> Item {
    HashMap::from([
        ("pk".into(), string(account_key(account.id))),
        ("sk".into(), string("PROFILE")),
        ("entity".into(), string("account")),
        ("account_id".into(), string(account.id)),
        ("email".into(), string(&account.email)),
        ("email_lookup".into(), string(&account.email_lookup)),
        (
            "email_verified_at".into(),
            string(encode_time(account.email_verified_at)),
        ),
        (
            "webauthn_user_handle".into(),
            string(&account.webauthn_user_handle),
        ),
        ("status".into(), string(account.status.as_str())),
        ("created_at".into(), string(encode_time(account.created_at))),
        ("updated_at".into(), string(encode_time(account.updated_at))),
        (
            "last_authenticated_at".into(),
            string(encode_time(account.last_authenticated_at)),
        ),
    ])
}

fn alias_item(pk: String, account_id: AccountId) -> Item {
    HashMap::from([
        ("pk".into(), string(pk)),
        ("sk".into(), string("ACCOUNT")),
        ("entity".into(), string("account_lookup")),
        ("account_id".into(), string(account_id)),
    ])
}

fn challenge_item(challenge: &AuthChallenge) -> Item {
    let mut item = HashMap::from([
        ("pk".into(), string(challenge_key(challenge.id))),
        ("sk".into(), string("CHALLENGE")),
        ("entity".into(), string("auth_challenge")),
        ("challenge_id".into(), string(challenge.id)),
        ("purpose".into(), string(challenge.purpose.as_str())),
        ("attempts".into(), number(challenge.attempts)),
        (
            "created_at".into(),
            string(encode_time(challenge.created_at)),
        ),
        (
            "expires_at".into(),
            string(encode_time(challenge.expires_at)),
        ),
        (
            "ttl_epoch".into(),
            number((challenge.expires_at + Duration::hours(24)).timestamp()),
        ),
    ]);
    insert_optional_uuid(&mut item, "account_id", challenge.account_id);
    insert_optional_string(&mut item, "email_lookup", challenge.email_lookup.as_deref());
    insert_optional_string(
        &mut item,
        "link_token_digest",
        challenge.link_token_digest.as_deref(),
    );
    insert_optional_string(&mut item, "code_digest", challenge.code_digest.as_deref());
    insert_optional_string(
        &mut item,
        "webauthn_state_json",
        challenge.webauthn_state_json.as_deref(),
    );
    insert_optional_time(&mut item, "consumed_at", challenge.consumed_at);
    item
}

fn session_item(session: &AuthSession) -> Item {
    let mut item = HashMap::from([
        ("pk".into(), string(account_key(session.account_id))),
        ("sk".into(), string(session_key(session.id))),
        ("entity".into(), string("auth_session")),
        ("session_id".into(), string(session.id)),
        ("account_id".into(), string(session.account_id)),
        ("token_digest".into(), string(&session.token_digest)),
        ("kind".into(), string(session.kind.as_str())),
        ("created_at".into(), string(encode_time(session.created_at))),
        (
            "last_seen_at".into(),
            string(encode_time(session.last_seen_at)),
        ),
        ("expires_at".into(), string(encode_time(session.expires_at))),
        (
            "absolute_expires_at".into(),
            string(encode_time(session.absolute_expires_at)),
        ),
        (
            "ttl_epoch".into(),
            number(
                session
                    .expires_at
                    .min(session.absolute_expires_at)
                    .timestamp(),
            ),
        ),
    ]);
    insert_optional_string(&mut item, "csrf_digest", session.csrf_digest.as_deref());
    insert_optional_time(&mut item, "revoked_at", session.revoked_at);
    item
}

fn passkey_item(passkey: &PasskeyCredential) -> Item {
    let mut item = HashMap::from([
        ("pk".into(), string(account_key(passkey.account_id))),
        ("sk".into(), string(passkey_key(passkey.id))),
        ("entity".into(), string("passkey")),
        ("passkey_id".into(), string(passkey.id)),
        ("credential_id".into(), string(&passkey.credential_id)),
        ("account_id".into(), string(passkey.account_id)),
        ("credential_json".into(), string(&passkey.credential_json)),
        ("label".into(), string(&passkey.label)),
        ("created_at".into(), string(encode_time(passkey.created_at))),
    ]);
    insert_optional_time(&mut item, "last_used_at", passkey.last_used_at);
    insert_optional_time(&mut item, "revoked_at", passkey.revoked_at);
    item
}

fn api_key_item(api_key: &ApiKey) -> Result<Item> {
    let mut item = HashMap::from([
        ("pk".into(), string(account_key(api_key.account_id))),
        ("sk".into(), string(api_key_key(api_key.id))),
        ("entity".into(), string("api_key")),
        ("key_id".into(), string(api_key.id)),
        ("account_id".into(), string(api_key.account_id)),
        ("secret_digest".into(), string(&api_key.secret_digest)),
        ("display_prefix".into(), string(&api_key.display_prefix)),
        ("name".into(), string(&api_key.name)),
        (
            "scopes_json".into(),
            string(serde_json::to_string(&api_key.scopes)?),
        ),
        ("queue_weight".into(), number(api_key.queue_weight)),
        ("created_at".into(), string(encode_time(api_key.created_at))),
    ]);
    insert_optional_time(&mut item, "expires_at", api_key.expires_at);
    insert_optional_time(&mut item, "last_used_at", api_key.last_used_at);
    insert_optional_time(&mut item, "revoked_at", api_key.revoked_at);
    if let Some(expires_at) = api_key.expires_at {
        item.insert(
            "ttl_epoch".into(),
            number((expires_at + Duration::days(30)).timestamp()),
        );
    }
    Ok(item)
}

fn account_from_item(item: &Item) -> Result<Account> {
    Ok(Account {
        id: required_uuid(item, "account_id")?,
        email: required_string(item, "email")?,
        email_lookup: required_string(item, "email_lookup")?,
        email_verified_at: required_time(item, "email_verified_at")?,
        webauthn_user_handle: required_string(item, "webauthn_user_handle")?,
        status: AccountStatus::parse(&required_string(item, "status")?)?,
        created_at: required_time(item, "created_at")?,
        updated_at: required_time(item, "updated_at")?,
        last_authenticated_at: required_time(item, "last_authenticated_at")?,
    })
}

fn challenge_from_item(item: &Item) -> Result<AuthChallenge> {
    Ok(AuthChallenge {
        id: required_uuid(item, "challenge_id")?,
        purpose: ChallengePurpose::parse(&required_string(item, "purpose")?)?,
        account_id: optional_uuid(item, "account_id")?,
        email_lookup: optional_string(item, "email_lookup"),
        link_token_digest: optional_string(item, "link_token_digest"),
        code_digest: optional_string(item, "code_digest"),
        webauthn_state_json: optional_string(item, "webauthn_state_json"),
        attempts: required_string(item, "attempts")?.parse()?,
        created_at: required_time(item, "created_at")?,
        expires_at: required_time(item, "expires_at")?,
        consumed_at: optional_time(item, "consumed_at")?,
    })
}

fn session_from_item(item: &Item) -> Result<AuthSession> {
    Ok(AuthSession {
        id: required_uuid(item, "session_id")?,
        token_digest: required_string(item, "token_digest")?,
        account_id: required_uuid(item, "account_id")?,
        kind: SessionKind::parse(&required_string(item, "kind")?)?,
        csrf_digest: optional_string(item, "csrf_digest"),
        created_at: required_time(item, "created_at")?,
        last_seen_at: required_time(item, "last_seen_at")?,
        expires_at: required_time(item, "expires_at")?,
        absolute_expires_at: required_time(item, "absolute_expires_at")?,
        revoked_at: optional_time(item, "revoked_at")?,
    })
}

fn passkey_from_item(item: &Item) -> Result<PasskeyCredential> {
    Ok(PasskeyCredential {
        id: required_uuid(item, "passkey_id")?,
        credential_id: required_string(item, "credential_id")?,
        account_id: required_uuid(item, "account_id")?,
        credential_json: required_string(item, "credential_json")?,
        label: required_string(item, "label")?,
        created_at: required_time(item, "created_at")?,
        last_used_at: optional_time(item, "last_used_at")?,
        revoked_at: optional_time(item, "revoked_at")?,
    })
}

fn api_key_from_item(item: &Item) -> Result<ApiKey> {
    Ok(ApiKey {
        id: required_uuid(item, "key_id")?,
        account_id: required_uuid(item, "account_id")?,
        secret_digest: required_string(item, "secret_digest")?,
        display_prefix: required_string(item, "display_prefix")?,
        name: required_string(item, "name")?,
        scopes: serde_json::from_str(&required_string(item, "scopes_json")?)?,
        queue_weight: required_string(item, "queue_weight")?.parse()?,
        created_at: required_time(item, "created_at")?,
        expires_at: optional_time(item, "expires_at")?,
        last_used_at: optional_time(item, "last_used_at")?,
        revoked_at: optional_time(item, "revoked_at")?,
    })
}

fn account_key(account_id: AccountId) -> String {
    format!("ACCOUNT#{account_id}")
}

fn email_key(email_lookup: &str) -> String {
    format!("EMAIL#{}", lookup_digest(email_lookup))
}

fn user_key(user_handle: &str) -> String {
    format!("USER#{}", lookup_digest(user_handle))
}

fn credential_key(credential_id: &str) -> String {
    format!("CREDENTIAL#{}", lookup_digest(credential_id))
}

fn challenge_key(challenge_id: ChallengeId) -> String {
    format!("CHALLENGE#{challenge_id}")
}

fn session_key(session_id: SessionId) -> String {
    format!("SESSION#{session_id}")
}

fn passkey_key(passkey_id: Uuid) -> String {
    format!("PASSKEY#{passkey_id}")
}

fn api_key_key(key_id: ApiKeyId) -> String {
    format!("APIKEY#{key_id}")
}

fn lookup_digest(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}

fn string(value: impl ToString) -> AttributeValue {
    AttributeValue::S(value.to_string())
}

fn number(value: impl ToString) -> AttributeValue {
    AttributeValue::N(value.to_string())
}

fn insert_optional_string(item: &mut Item, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        item.insert(name.into(), string(value));
    }
}

fn insert_optional_uuid(item: &mut Item, name: &str, value: Option<Uuid>) {
    if let Some(value) = value {
        item.insert(name.into(), string(value));
    }
}

fn insert_optional_time(item: &mut Item, name: &str, value: Option<DateTime<Utc>>) {
    if let Some(value) = value {
        item.insert(name.into(), string(encode_time(value)));
    }
}

fn required_string(item: &Item, name: &str) -> Result<String> {
    match item.get(name) {
        Some(AttributeValue::S(value)) | Some(AttributeValue::N(value)) => Ok(value.clone()),
        _ => anyhow::bail!("DynamoDB identity item is missing string/number `{name}`"),
    }
}

fn optional_string(item: &Item, name: &str) -> Option<String> {
    match item.get(name) {
        Some(AttributeValue::S(value)) | Some(AttributeValue::N(value)) => Some(value.clone()),
        _ => None,
    }
}

fn required_uuid(item: &Item, name: &str) -> Result<Uuid> {
    Uuid::parse_str(&required_string(item, name)?)
        .with_context(|| format!("DynamoDB identity `{name}` is not a UUID"))
}

fn optional_uuid(item: &Item, name: &str) -> Result<Option<Uuid>> {
    optional_string(item, name)
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .with_context(|| format!("DynamoDB identity `{name}` is not a UUID"))
}

fn required_time(item: &Item, name: &str) -> Result<DateTime<Utc>> {
    decode_time(&required_string(item, name)?)
}

fn optional_time(item: &Item, name: &str) -> Result<Option<DateTime<Utc>>> {
    optional_string(item, name)
        .as_deref()
        .map(decode_time)
        .transpose()
}

fn encode_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

fn decode_time(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_and_api_key_items_are_account_partition_point_lookups() {
        let now = Utc::now();
        let account_id = Uuid::now_v7();
        let session = AuthSession {
            id: Uuid::now_v7(),
            token_digest: "session-digest".into(),
            account_id,
            kind: SessionKind::Browser,
            csrf_digest: Some("csrf-digest".into()),
            created_at: now,
            last_seen_at: now,
            expires_at: now + Duration::days(30),
            absolute_expires_at: now + Duration::days(90),
            revoked_at: None,
        };
        let session_item = session_item(&session);
        assert_eq!(
            required_string(&session_item, "pk").unwrap(),
            account_key(account_id)
        );
        assert_eq!(
            required_string(&session_item, "sk").unwrap(),
            session_key(session.id)
        );
        assert_eq!(
            required_string(&session_item, "ttl_epoch").unwrap(),
            session.expires_at.timestamp().to_string()
        );
        assert_eq!(session_from_item(&session_item).unwrap(), session);

        let api_key = ApiKey {
            id: Uuid::now_v7(),
            account_id,
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
        let api_key_item = api_key_item(&api_key).unwrap();
        assert_eq!(
            required_string(&api_key_item, "pk").unwrap(),
            account_key(account_id)
        );
        assert_eq!(
            required_string(&api_key_item, "sk").unwrap(),
            api_key_key(api_key.id)
        );
        assert!(!api_key_item.contains_key("ttl_epoch"));
        assert_eq!(api_key_from_item(&api_key_item).unwrap(), api_key);
    }

    #[test]
    fn lookup_partition_keys_do_not_expose_email_or_user_handle() {
        let email = "Astronomer+private@example.com";
        let user_handle = "base64-user-handle";
        assert!(!email_key(email).contains(email));
        assert!(!user_key(user_handle).contains(user_handle));
        assert_eq!(email_key(email), email_key(email));
        assert_ne!(email_key(email), email_key("other@example.com"));
    }
}
