use crate::{
    config::Config,
    dynamodb_common::{
        Item, encode_time, insert_optional_string, insert_optional_time, insert_optional_uuid,
        number, optional_string, optional_time, optional_uuid, required_string, required_time,
        required_uuid, string,
    },
    identity::{
        Account, AccountId, AccountStatus, ApiKey, ApiKeyId, AuthChallenge, AuthSession,
        ChallengeId, ChallengePurpose, CompletedEmailSignIn, CompletedPasskeySignIn,
        IdentityRepository, PasskeyCredential, SessionId, SessionKind,
    },
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_dynamodb::{
    Client,
    types::{AttributeValue, ConditionCheck, Delete, Put, ReturnValue, TransactWriteItem, Update},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use uuid::Uuid;

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

    async fn create_email_challenge(
        &self,
        challenge: AuthChallenge,
        max_live: usize,
    ) -> Result<()> {
        if challenge.purpose != ChallengePurpose::EmailLogin {
            anyhow::bail!("email challenge storage requires purpose=email-login");
        }
        if max_live == 0 {
            anyhow::bail!("email challenge live limit must be at least one");
        }
        let email_lookup = challenge
            .email_lookup
            .as_deref()
            .context("email challenge is missing email_lookup")?;
        let tracker_pk = email_key(email_lookup);
        for _ in 0..5 {
            let tracker = self
                .get_item(tracker_pk.clone(), "CHALLENGES".into())
                .await?;
            let version = tracker
                .as_ref()
                .map(|item| required_string(item, "version"))
                .transpose()?
                .map(|value| value.parse::<u64>())
                .transpose()?
                .unwrap_or(0);
            let mut challenge_ids = tracker
                .as_ref()
                .map(tracker_challenge_ids)
                .transpose()?
                .unwrap_or_default();
            challenge_ids.retain(|id| *id != challenge.id);
            challenge_ids.insert(0, challenge.id);
            let evicted = challenge_ids.split_off(challenge_ids.len().min(max_live));

            let challenge_put = Put::builder()
                .table_name(&self.table)
                .set_item(Some(challenge_item(&challenge)))
                .condition_expression("attribute_not_exists(pk)")
                .build()?;
            let mut tracker_put =
                Put::builder()
                    .table_name(&self.table)
                    .set_item(Some(challenge_tracker_item(
                        tracker_pk.clone(),
                        version + 1,
                        &challenge_ids,
                    )));
            tracker_put = if tracker.is_some() {
                tracker_put
                    .condition_expression("#version = :version")
                    .expression_attribute_names("#version", "version")
                    .expression_attribute_values(":version", number(version))
            } else {
                tracker_put.condition_expression("attribute_not_exists(pk)")
            };
            let mut transaction = self
                .client
                .transact_write_items()
                .transact_items(TransactWriteItem::builder().put(challenge_put).build())
                .transact_items(
                    TransactWriteItem::builder()
                        .put(tracker_put.build()?)
                        .build(),
                );
            // Delete rather than mark-consumed: an unconditioned Update would
            // upsert a permanent stub if TTL already removed the item.
            for challenge_id in evicted {
                let delete = Delete::builder()
                    .table_name(&self.table)
                    .key("pk", string(challenge_key(challenge_id)))
                    .key("sk", string("CHALLENGE"))
                    .build()?;
                transaction =
                    transaction.transact_items(TransactWriteItem::builder().delete(delete).build());
            }
            match transaction.send().await {
                Ok(_) => return Ok(()),
                Err(error)
                    if error
                        .as_service_error()
                        .is_some_and(|error| error.is_transaction_canceled_exception()) => {}
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("email challenge tracker changed too frequently; retry the request")
    }

    async fn record_challenge_failure(
        &self,
        challenge_id: ChallengeId,
        now: DateTime<Utc>,
        max_attempts: u32,
    ) -> Result<Option<AuthChallenge>> {
        let result = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", string(challenge_key(challenge_id)))
            .key("sk", string("CHALLENGE"))
            .condition_expression("attribute_not_exists(consumed_at) AND expires_at > :now AND attempts < :max_attempts")
            .update_expression("SET attempts = attempts + :one")
            .expression_attribute_values(":now", string(encode_time(now)))
            .expression_attribute_values(":max_attempts", number(max_attempts))
            .expression_attribute_values(":one", number(1))
            .return_values(ReturnValue::AllNew)
            .send()
            .await;
        match result {
            Ok(output) => output
                .attributes
                .as_ref()
                .map(challenge_from_item)
                .transpose(),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_conditional_check_failed_exception()) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn complete_email_challenge(
        &self,
        challenge_id: ChallengeId,
        now: DateTime<Utc>,
        max_attempts: u32,
        new_account: Account,
        mut new_session: AuthSession,
    ) -> Result<Option<CompletedEmailSignIn>> {
        for _ in 0..5 {
            let Some(challenge) = self.challenge_by_id(challenge_id).await? else {
                return Ok(None);
            };
            let Some(email_lookup) = challenge.email_lookup.as_deref() else {
                return Ok(None);
            };
            if challenge.purpose != ChallengePurpose::EmailLogin
                || challenge.consumed_at.is_some()
                || challenge.expires_at <= now
                || challenge.attempts >= max_attempts
                || new_account.email_lookup != email_lookup
            {
                return Ok(None);
            }

            let tracker_pk = email_key(email_lookup);
            let tracker = self
                .get_item(tracker_pk.clone(), "CHALLENGES".into())
                .await?;
            let tracker_version = tracker
                .as_ref()
                .map(|item| required_string(item, "version"))
                .transpose()?
                .map(|value| value.parse::<u64>())
                .transpose()?
                .unwrap_or(0);
            let live_ids = tracker
                .as_ref()
                .map(tracker_challenge_ids)
                .transpose()?
                .unwrap_or_else(|| vec![challenge_id]);
            let existing_account = self.account_by_email_lookup(email_lookup).await?;
            if existing_account
                .as_ref()
                .is_some_and(|account| account.status != AccountStatus::Active)
            {
                return Ok(None);
            }
            let account = existing_account
                .clone()
                .unwrap_or_else(|| new_account.clone());
            let account_created = existing_account.is_none();
            new_session.account_id = account.id;

            let consume = Update::builder()
                .table_name(&self.table)
                .key("pk", string(challenge_key(challenge_id)))
                .key("sk", string("CHALLENGE"))
                .condition_expression("purpose = :purpose AND attribute_not_exists(consumed_at) AND expires_at > :now AND attempts < :max_attempts")
                .update_expression("SET consumed_at = :now")
                .expression_attribute_values(":purpose", string(ChallengePurpose::EmailLogin.as_str()))
                .expression_attribute_values(":now", string(encode_time(now)))
                .expression_attribute_values(":max_attempts", number(max_attempts))
                .build()?;
            let mut transaction = self
                .client
                .transact_write_items()
                .transact_items(TransactWriteItem::builder().update(consume).build());
            // Delete rather than mark-consumed: an unconditioned Update would
            // upsert a permanent stub if TTL already removed the item.
            for other_id in live_ids.into_iter().filter(|id| *id != challenge_id) {
                let invalidate = Delete::builder()
                    .table_name(&self.table)
                    .key("pk", string(challenge_key(other_id)))
                    .key("sk", string("CHALLENGE"))
                    .build()?;
                transaction = transaction
                    .transact_items(TransactWriteItem::builder().delete(invalidate).build());
            }

            let mut tracker_put =
                Put::builder()
                    .table_name(&self.table)
                    .set_item(Some(challenge_tracker_item(
                        tracker_pk,
                        tracker_version + 1,
                        &[],
                    )));
            tracker_put = if tracker.is_some() {
                tracker_put
                    .condition_expression("#version = :version")
                    .expression_attribute_names("#version", "version")
                    .expression_attribute_values(":version", number(tracker_version))
            } else {
                tracker_put.condition_expression("attribute_not_exists(pk)")
            };
            transaction = transaction.transact_items(
                TransactWriteItem::builder()
                    .put(tracker_put.build()?)
                    .build(),
            );

            if account_created {
                let profile = Put::builder()
                    .table_name(&self.table)
                    .set_item(Some(account_item(&account)))
                    .condition_expression("attribute_not_exists(pk)")
                    .build()?;
                let email_alias = Put::builder()
                    .table_name(&self.table)
                    .set_item(Some(alias_item(email_key(email_lookup), account.id)))
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
                transaction = transaction
                    .transact_items(TransactWriteItem::builder().put(profile).build())
                    .transact_items(TransactWriteItem::builder().put(email_alias).build())
                    .transact_items(TransactWriteItem::builder().put(user_alias).build());
            } else {
                let update_account = Update::builder()
                    .table_name(&self.table)
                    .key("pk", string(account_key(account.id)))
                    .key("sk", string("PROFILE"))
                    .condition_expression("#status = :active")
                    .update_expression("SET updated_at = :now, last_authenticated_at = :now")
                    .expression_attribute_names("#status", "status")
                    .expression_attribute_values(":active", string(AccountStatus::Active.as_str()))
                    .expression_attribute_values(":now", string(encode_time(now)))
                    .build()?;
                transaction = transaction
                    .transact_items(TransactWriteItem::builder().update(update_account).build());
            }
            let session_put = Put::builder()
                .table_name(&self.table)
                .set_item(Some(session_item(&new_session)))
                .condition_expression("attribute_not_exists(pk) AND attribute_not_exists(sk)")
                .build()?;
            transaction =
                transaction.transact_items(TransactWriteItem::builder().put(session_put).build());

            match transaction.send().await {
                Ok(_) => {
                    let mut account = account;
                    account.updated_at = now;
                    account.last_authenticated_at = now;
                    return Ok(Some(CompletedEmailSignIn {
                        account,
                        session: new_session,
                        account_created,
                    }));
                }
                Err(error)
                    if error
                        .as_service_error()
                        .is_some_and(|error| error.is_transaction_canceled_exception()) =>
                {
                    let still_live =
                        self.challenge_by_id(challenge_id)
                            .await?
                            .is_some_and(|challenge| {
                                challenge.consumed_at.is_none()
                                    && challenge.expires_at > now
                                    && challenge.attempts < max_attempts
                            });
                    if !still_live {
                        return Ok(None);
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("email sign-in state changed too frequently; retry the request")
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

    async fn touch_session(
        &self,
        account_id: AccountId,
        session_id: SessionId,
        last_seen_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<bool> {
        let result = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", string(account_key(account_id)))
            .key("sk", string(session_key(session_id)))
            .condition_expression("entity = :entity AND attribute_not_exists(revoked_at) AND absolute_expires_at > :last_seen_at")
            .update_expression("SET last_seen_at = :last_seen_at, expires_at = :expires_at, ttl_epoch = :ttl_epoch")
            .expression_attribute_values(":entity", string("auth_session"))
            .expression_attribute_values(":last_seen_at", string(encode_time(last_seen_at)))
            .expression_attribute_values(":expires_at", string(encode_time(expires_at)))
            .expression_attribute_values(":ttl_epoch", number(expires_at.timestamp()))
            .send()
            .await;
        match result {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_conditional_check_failed_exception()) =>
            {
                Ok(false)
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn revoke_session(
        &self,
        account_id: AccountId,
        session_id: SessionId,
        revoked_at: DateTime<Utc>,
    ) -> Result<bool> {
        let result = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", string(account_key(account_id)))
            .key("sk", string(session_key(session_id)))
            .condition_expression("entity = :entity AND attribute_not_exists(revoked_at)")
            .update_expression("SET revoked_at = :revoked_at, ttl_epoch = :ttl_epoch")
            .expression_attribute_values(":entity", string("auth_session"))
            .expression_attribute_values(":revoked_at", string(encode_time(revoked_at)))
            .expression_attribute_values(
                ":ttl_epoch",
                number((revoked_at + Duration::days(30)).timestamp()),
            )
            .send()
            .await;
        match result {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_conditional_check_failed_exception()) =>
            {
                Ok(false)
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn revoke_all_sessions(
        &self,
        account_id: AccountId,
        revoked_at: DateTime<Utc>,
    ) -> Result<u64> {
        let sessions = self.list_sessions(account_id).await?;
        let mut revoked = 0;
        for session in sessions {
            if self
                .revoke_session(account_id, session.id, revoked_at)
                .await?
            {
                revoked += 1;
            }
        }
        Ok(revoked)
    }

    async fn delete_session(&self, account_id: AccountId, session_id: SessionId) -> Result<()> {
        self.client
            .delete_item()
            .table_name(&self.table)
            .key("pk", string(account_key(account_id)))
            .key("sk", string(session_key(session_id)))
            .condition_expression("attribute_not_exists(pk) OR entity = :entity")
            .expression_attribute_values(":entity", string("auth_session"))
            .send()
            .await?;
        Ok(())
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

    async fn complete_passkey_registration(
        &self,
        challenge_id: ChallengeId,
        passkey: PasskeyCredential,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        for _ in 0..5 {
            let consume = Update::builder()
                .table_name(&self.table)
                .key("pk", string(challenge_key(challenge_id)))
                .key("sk", string("CHALLENGE"))
                .condition_expression("purpose = :purpose AND account_id = :account_id AND attribute_not_exists(consumed_at) AND expires_at > :now")
                .update_expression("SET consumed_at = :now")
                .expression_attribute_values(":purpose", string(ChallengePurpose::PasskeyRegistration.as_str()))
                .expression_attribute_values(":account_id", string(passkey.account_id))
                .expression_attribute_values(":now", string(encode_time(now)))
                .build()?;
            let passkey_put = Put::builder()
                .table_name(&self.table)
                .set_item(Some(passkey_item(&passkey)))
                .condition_expression("attribute_not_exists(pk) AND attribute_not_exists(sk)")
                .build()?;
            let credential_put = Put::builder()
                .table_name(&self.table)
                .set_item(Some(HashMap::from([
                    ("pk".into(), string(credential_key(&passkey.credential_id))),
                    ("sk".into(), string("ACCOUNT")),
                    ("entity".into(), string("passkey_lookup")),
                    ("account_id".into(), string(passkey.account_id)),
                    ("passkey_id".into(), string(passkey.id)),
                ])))
                .condition_expression("attribute_not_exists(pk)")
                .build()?;
            let result = self
                .client
                .transact_write_items()
                .transact_items(TransactWriteItem::builder().update(consume).build())
                .transact_items(
                    TransactWriteItem::builder()
                        .condition_check(self.active_account_check(passkey.account_id)?)
                        .build(),
                )
                .transact_items(TransactWriteItem::builder().put(passkey_put).build())
                .transact_items(TransactWriteItem::builder().put(credential_put).build())
                .send()
                .await;
            match result {
                Ok(_) => return Ok(true),
                Err(error) => match error
                    .as_service_error()
                    .and_then(canceled_only_by_condition_checks)
                {
                    // A failed condition check is a definitive refusal.
                    Some(true) => return Ok(false),
                    // Conflicting concurrent transaction or throttling: retry.
                    Some(false) => {}
                    None => return Err(error.into()),
                },
            }
        }
        anyhow::bail!("passkey registration conflicted with concurrent account activity; retry")
    }

    async fn complete_passkey_sign_in(
        &self,
        challenge_id: ChallengeId,
        passkey: PasskeyCredential,
        session: AuthSession,
        now: DateTime<Utc>,
    ) -> Result<Option<CompletedPasskeySignIn>> {
        if passkey.account_id != session.account_id {
            anyhow::bail!("passkey and session account IDs differ");
        }
        for _ in 0..5 {
            let consume = Update::builder()
            .table_name(&self.table)
            .key("pk", string(challenge_key(challenge_id)))
            .key("sk", string("CHALLENGE"))
            .condition_expression(
                "purpose = :purpose AND attribute_not_exists(consumed_at) AND expires_at > :now",
            )
            .update_expression("SET consumed_at = :now")
            .expression_attribute_values(
                ":purpose",
                string(ChallengePurpose::PasskeyAuthentication.as_str()),
            )
            .expression_attribute_values(":now", string(encode_time(now)))
            .build()?;
            let update_passkey = Update::builder()
                .table_name(&self.table)
                .key("pk", string(account_key(passkey.account_id)))
                .key("sk", string(passkey_key(passkey.id)))
                .condition_expression(
                    "attribute_not_exists(revoked_at) AND credential_id = :credential_id",
                )
                .update_expression("SET credential_json = :credential_json, last_used_at = :now")
                .expression_attribute_values(":credential_id", string(&passkey.credential_id))
                .expression_attribute_values(":credential_json", string(&passkey.credential_json))
                .expression_attribute_values(":now", string(encode_time(now)))
                .build()?;
            let session_put = Put::builder()
                .table_name(&self.table)
                .set_item(Some(session_item(&session)))
                .condition_expression("attribute_not_exists(pk) AND attribute_not_exists(sk)")
                .build()?;
            let update_account = Update::builder()
                .table_name(&self.table)
                .key("pk", string(account_key(passkey.account_id)))
                .key("sk", string("PROFILE"))
                .condition_expression("#status = :active")
                .update_expression("SET updated_at = :now, last_authenticated_at = :now")
                .expression_attribute_names("#status", "status")
                .expression_attribute_values(":active", string(AccountStatus::Active.as_str()))
                .expression_attribute_values(":now", string(encode_time(now)))
                .build()?;
            let result = self
                .client
                .transact_write_items()
                .transact_items(TransactWriteItem::builder().update(consume).build())
                .transact_items(TransactWriteItem::builder().update(update_passkey).build())
                .transact_items(TransactWriteItem::builder().put(session_put).build())
                .transact_items(TransactWriteItem::builder().update(update_account).build())
                .send()
                .await;
            match result {
                Ok(_) => {
                    let Some(account) = self.account_by_id(passkey.account_id).await? else {
                        anyhow::bail!(
                            "passkey account disappeared after successful authentication"
                        );
                    };
                    let mut passkey = passkey;
                    passkey.last_used_at = Some(now);
                    return Ok(Some(CompletedPasskeySignIn {
                        account,
                        session,
                        passkey,
                    }));
                }
                Err(error) => match error
                    .as_service_error()
                    .and_then(canceled_only_by_condition_checks)
                {
                    // A failed condition check is a definitive refusal.
                    Some(true) => return Ok(None),
                    // Conflicting concurrent transaction or throttling: retry.
                    Some(false) => {}
                    None => return Err(error.into()),
                },
            }
        }
        anyhow::bail!("passkey sign-in conflicted with concurrent account activity; retry")
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

    async fn revoke_passkey(
        &self,
        account_id: AccountId,
        passkey_id: crate::identity::PasskeyId,
        revoked_at: DateTime<Utc>,
    ) -> Result<bool> {
        for _ in 0..5 {
            // The credential lookup item is deleted alongside the revocation so
            // the same authenticator can be registered again later.
            let passkey = self
                .get_item(account_key(account_id), passkey_key(passkey_id))
                .await?
                .as_ref()
                .map(passkey_from_item)
                .transpose()?;
            let Some(passkey) = passkey else {
                return Ok(false);
            };
            if passkey.revoked_at.is_some() {
                return Ok(false);
            }
            let revoke = Update::builder()
                .table_name(&self.table)
                .key("pk", string(account_key(account_id)))
                .key("sk", string(passkey_key(passkey_id)))
                .condition_expression(
                    "entity = :entity AND attribute_not_exists(revoked_at) AND credential_id = :credential_id",
                )
                .update_expression("SET revoked_at = :revoked_at")
                .expression_attribute_values(":entity", string("passkey"))
                .expression_attribute_values(":credential_id", string(&passkey.credential_id))
                .expression_attribute_values(":revoked_at", string(encode_time(revoked_at)))
                .build()?;
            let delete_lookup = Delete::builder()
                .table_name(&self.table)
                .key("pk", string(credential_key(&passkey.credential_id)))
                .key("sk", string("ACCOUNT"))
                .build()?;
            let result = self
                .client
                .transact_write_items()
                .transact_items(TransactWriteItem::builder().update(revoke).build())
                .transact_items(TransactWriteItem::builder().delete(delete_lookup).build())
                .send()
                .await;
            match result {
                Ok(_) => return Ok(true),
                Err(error) => match error
                    .as_service_error()
                    .and_then(canceled_only_by_condition_checks)
                {
                    Some(true) => return Ok(false),
                    Some(false) => {}
                    None => return Err(error.into()),
                },
            }
        }
        anyhow::bail!("passkey revocation conflicted with concurrent account activity; retry")
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

    async fn touch_api_key(
        &self,
        account_id: AccountId,
        key_id: ApiKeyId,
        last_used_at: DateTime<Utc>,
    ) -> Result<bool> {
        let result = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", string(account_key(account_id)))
            .key("sk", string(api_key_key(key_id)))
            .condition_expression("entity = :entity AND attribute_not_exists(revoked_at) AND (attribute_not_exists(expires_at) OR expires_at > :now)")
            .update_expression("SET last_used_at = :now")
            .expression_attribute_values(":entity", string("api_key"))
            .expression_attribute_values(":now", string(encode_time(last_used_at)))
            .send()
            .await;
        match result {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_conditional_check_failed_exception()) =>
            {
                Ok(false)
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn revoke_api_key(
        &self,
        account_id: AccountId,
        key_id: ApiKeyId,
        revoked_at: DateTime<Utc>,
    ) -> Result<bool> {
        let result = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", string(account_key(account_id)))
            .key("sk", string(api_key_key(key_id)))
            .condition_expression("entity = :entity AND attribute_not_exists(revoked_at)")
            .update_expression("SET revoked_at = :revoked_at, ttl_epoch = :ttl_epoch")
            .expression_attribute_values(":entity", string("api_key"))
            .expression_attribute_values(":revoked_at", string(encode_time(revoked_at)))
            .expression_attribute_values(
                ":ttl_epoch",
                number((revoked_at + Duration::days(30)).timestamp()),
            )
            .send()
            .await;
        match result {
            Ok(_) => {}
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_conditional_check_failed_exception()) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        }
        // Best-effort sweep of sessions minted from this key so revocation is
        // visible in listings immediately. Authentication re-validates the key
        // itself, so a session missed here still fails closed.
        for session in self.list_sessions(account_id).await? {
            if session.api_key_id == Some(key_id) && session.revoked_at.is_none() {
                self.revoke_session(account_id, session.id, revoked_at)
                    .await?;
            }
        }
        Ok(true)
    }

    async fn purge_expired(&self, _now: DateTime<Utc>) -> Result<u64> {
        // Every challenge, session, and tracker item carries a `ttl_epoch`;
        // DynamoDB TTL performs the deletion.
        Ok(0)
    }
}

/// Classifies a `TransactWriteItems` service error: `Some(true)` when the
/// transaction was canceled solely by failed condition checks (a definitive
/// refusal from the data), `Some(false)` when it was canceled by a concurrent
/// transaction conflict or throttling (retryable), and `None` for every other
/// error.
fn canceled_only_by_condition_checks(
    error: &aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError,
) -> Option<bool> {
    use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
    let TransactWriteItemsError::TransactionCanceledException(canceled) = error else {
        return None;
    };
    let reasons = canceled.cancellation_reasons();
    Some(
        reasons.is_empty()
            || reasons.iter().all(|reason| {
                matches!(
                    reason.code(),
                    None | Some("None" | "ConditionalCheckFailed")
                )
            }),
    )
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

fn challenge_tracker_item(pk: String, version: u64, challenge_ids: &[ChallengeId]) -> Item {
    HashMap::from([
        ("pk".into(), string(pk)),
        ("sk".into(), string("CHALLENGES")),
        ("entity".into(), string("email_challenge_tracker")),
        ("version".into(), number(version)),
        (
            "challenge_ids".into(),
            AttributeValue::L(challenge_ids.iter().copied().map(string).collect()),
        ),
        // Trackers are created by unauthenticated requests — one per email
        // address ever entered — so each write refreshes a TTL that outlives
        // the challenges it can reference. A version restart after TTL
        // deletion is safe: creation treats a missing tracker as version zero.
        (
            "ttl_epoch".into(),
            number((Utc::now() + Duration::hours(48)).timestamp()),
        ),
    ])
}

fn tracker_challenge_ids(item: &Item) -> Result<Vec<ChallengeId>> {
    let Some(AttributeValue::L(values)) = item.get("challenge_ids") else {
        anyhow::bail!("DynamoDB email challenge tracker is missing challenge_ids");
    };
    values
        .iter()
        .map(|value| match value {
            AttributeValue::S(value) => Uuid::parse_str(value)
                .context("DynamoDB email challenge tracker contains a non-UUID ID"),
            _ => anyhow::bail!("DynamoDB email challenge tracker contains a non-string ID"),
        })
        .collect()
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
    insert_optional_uuid(&mut item, "api_key_id", session.api_key_id);
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
        api_key_id: optional_uuid(item, "api_key_id")?,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the shared `IdentityRepository` contract against a real table so
    /// the DynamoDB transactions exercise the same invariants the SQLx
    /// backend is tested with. DynamoDB Local works:
    /// `SEIZA_TEST_IDENTITY_TABLE=seiza-identity-test AWS_ENDPOINT_URL=http://localhost:8000 \
    ///  cargo test --features aws -- --ignored dynamodb_satisfies`
    #[tokio::test]
    #[ignore = "requires SEIZA_TEST_IDENTITY_TABLE and DynamoDB credentials"]
    async fn dynamodb_satisfies_the_identity_repository_contract() {
        let table = std::env::var("SEIZA_TEST_IDENTITY_TABLE")
            .expect("set SEIZA_TEST_IDENTITY_TABLE to run this test");
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        let repository = DynamoDbIdentityRepository {
            client: Client::new(&sdk_config),
            table,
        };
        crate::identity::contract::assert_contract(&repository).await;
    }

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
            api_key_id: None,
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
