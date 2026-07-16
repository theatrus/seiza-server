use crate::{
    config::Config,
    models::{
        AstrometryId, JobId, JobLease, JobRecord, JobStatus, LegacyJobId, SolutionResponse,
        SolveOptions, ValidationDonation, astrometry_id_for_job,
    },
    repository::JobRepository,
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use aws_sdk_dynamodb::{
    Client,
    types::{AttributeValue, Put, TransactWriteItem, Update},
};
use chrono::{DateTime, Duration, Utc};
use std::{cmp::Ordering, collections::HashMap};
use uuid::Uuid;

type Item = HashMap<String, AttributeValue>;

/// DynamoDB's single-table implementation. The table only needs a string
/// partition key named `pk`; `JOB#…`, `ASTROMETRY#…`, and `CLIENT#…` records share
/// it. Leases are changed conditionally, so duplicate queue messages and
/// multiple worker processes are safe. UUID job IDs require no counter item.
pub struct DynamoDbJobRepository {
    client: Client,
    table: String,
}

impl DynamoDbJobRepository {
    pub async fn connect(config: &Config) -> Result<Self> {
        let table = config
            .dynamodb_table
            .clone()
            .context("SEIZA_DYNAMODB_TABLE is required for the DynamoDB job backend")?;
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        Ok(Self {
            client: Client::new(&sdk_config),
            table,
        })
    }

    async fn client_last_served(&self, owner: &str) -> Result<Option<DateTime<Utc>>> {
        let output = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("pk", string(client_key(owner)))
            .send()
            .await?;
        output
            .item()
            .and_then(|item| optional_string(item, "last_served_at"))
            .map(|value| decode_time(&value))
            .transpose()
    }

    async fn reclaim_expired(&self, now: DateTime<Utc>) -> Result<()> {
        let mut values = HashMap::new();
        values.insert(":job".into(), string("job"));
        values.insert(":solving".into(), string("solving"));
        values.insert(":now".into(), string(encode_time(now)));
        let mut names = HashMap::new();
        names.insert("#entity".into(), "entity".into());
        names.insert("#status".into(), "status".into());
        names.insert("#lease_expires_at".into(), "lease_expires_at".into());
        for item in self
            .scan(
                "#entity = :job AND #status = :solving AND #lease_expires_at <= :now",
                names,
                values,
                None,
            )
            .await?
        {
            let Some(pk) = optional_string(&item, "pk") else {
                continue;
            };
            let requeue = self
                .client
                .update_item()
                .table_name(&self.table)
                .key("pk", string(pk))
                .condition_expression("#status = :solving AND #lease_expires_at <= :now")
                .update_expression("SET #status = :queued REMOVE lease_token, #lease_expires_at, notification_delivered_at")
                .expression_attribute_names("#status", "status")
                .expression_attribute_names("#lease_expires_at", "lease_expires_at")
                .expression_attribute_values(":solving", string("solving"))
                .expression_attribute_values(":queued", string("queued"))
                .expression_attribute_values(":now", string(encode_time(now)))
                .send()
                .await;
            match requeue {
                Ok(_) => {}
                Err(error)
                    if error
                        .as_service_error()
                        .is_some_and(|error| error.is_conditional_check_failed_exception()) => {}
                Err(error) => return Err(error).context("requeueing an expired DynamoDB lease"),
            }
        }
        Ok(())
    }

    async fn queued_candidate(
        &self,
        requested_job_id: Option<JobId>,
        now: DateTime<Utc>,
    ) -> Result<Option<JobRecord>> {
        if let Some(job_id) = requested_job_id {
            return Ok(self
                .get(job_id)
                .await?
                .filter(|job| job.status == JobStatus::Queued));
        }
        let mut values = HashMap::new();
        values.insert(":job".into(), string("job"));
        values.insert(":queued".into(), string("queued"));
        let mut names = HashMap::new();
        names.insert("#entity".into(), "entity".into());
        names.insert("#status".into(), "status".into());
        let jobs = self
            .scan("#entity = :job AND #status = :queued", names, values, None)
            .await?
            .into_iter()
            .map(|item| record_from_item(&item))
            .collect::<Result<Vec<_>>>()?;
        let mut best: Option<(f64, JobRecord)> = None;
        for job in jobs {
            let score = match self.client_last_served(&job.owner).await? {
                Some(last) => {
                    (now - last).num_milliseconds() as f64 / 1_000.0 * job.queue_weight.max(0.01)
                }
                None => f64::MAX / 4.0,
            };
            if best.as_ref().is_none_or(|(best_score, best_job)| {
                score.total_cmp(best_score) == Ordering::Greater
                    || (score.total_cmp(best_score) == Ordering::Equal
                        && job.created_at < best_job.created_at)
            }) {
                best = Some((score, job));
            }
        }
        Ok(best.map(|(_, job)| job))
    }

    async fn scan(
        &self,
        filter_expression: &str,
        names: HashMap<String, String>,
        values: HashMap<String, AttributeValue>,
        limit: Option<usize>,
    ) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        let mut start_key = None;
        loop {
            let mut request = self
                .client
                .scan()
                .table_name(&self.table)
                .filter_expression(filter_expression)
                .set_expression_attribute_names(Some(names.clone()))
                .set_expression_attribute_values(Some(values.clone()));
            if let Some(key) = start_key {
                request = request.set_exclusive_start_key(Some(key));
            }
            if let Some(limit) = limit {
                request = request.limit(limit.min(i32::MAX as usize) as i32);
            }
            let output = request.send().await?;
            items.extend(output.items().iter().cloned());
            if let Some(max_items) = limit
                && items.len() >= max_items
            {
                items.truncate(max_items);
                return Ok(items);
            }
            start_key = output.last_evaluated_key().cloned();
            if start_key.is_none() {
                return Ok(items);
            }
        }
    }
}

#[async_trait]
impl JobRepository for DynamoDbJobRepository {
    async fn enqueue(&self, mut job: JobRecord) -> Result<JobRecord> {
        job.astrometry_id = astrometry_id_for_job(job.id);
        job.status = JobStatus::Queued;
        let job_put = Put::builder()
            .table_name(&self.table)
            .set_item(Some(job_item(&job)?))
            .condition_expression("attribute_not_exists(pk)")
            .build()?;
        let index_put = Put::builder()
            .table_name(&self.table)
            .set_item(Some(HashMap::from([
                ("pk".into(), string(object_index_key(&job.object_key))),
                ("entity".into(), string("object_index")),
                ("job_id".into(), string(job.id)),
            ])))
            .condition_expression("attribute_not_exists(pk)")
            .build()?;
        let astrometry_index_put = Put::builder()
            .table_name(&self.table)
            .set_item(Some(HashMap::from([
                ("pk".into(), string(astrometry_index_key(job.astrometry_id))),
                ("entity".into(), string("astrometry_index")),
                ("job_id".into(), string(job.id)),
            ])))
            .condition_expression("attribute_not_exists(pk)")
            .build()?;
        let result = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().put(job_put).build())
            .transact_items(TransactWriteItem::builder().put(index_put).build())
            .transact_items(
                TransactWriteItem::builder()
                    .put(astrometry_index_put)
                    .build(),
            )
            .send()
            .await;
        match result {
            Ok(_) => Ok(job),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_transaction_canceled_exception()) =>
            {
                self.find_by_object_key(&job.object_key)
                    .await?
                    .context("idempotent DynamoDB enqueue could not find the existing job")
            }
            Err(error) => Err(error).context("persisting DynamoDB job and object index"),
        }
    }

    async fn get(&self, job_id: JobId) -> Result<Option<JobRecord>> {
        self.client
            .get_item()
            .table_name(&self.table)
            .key("pk", string(job_key(job_id)))
            .send()
            .await?
            .item()
            .map(record_from_item)
            .transpose()
    }

    async fn get_by_legacy_id(&self, legacy_id: LegacyJobId) -> Result<Option<JobRecord>> {
        let index = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("pk", string(legacy_index_key(legacy_id)))
            .consistent_read(true)
            .send()
            .await?;
        let Some(job_id) = index
            .item()
            .map(|item| required_uuid(item, "job_id"))
            .transpose()?
        else {
            return Ok(None);
        };
        self.get(job_id).await
    }

    async fn get_by_astrometry_id(&self, astrometry_id: AstrometryId) -> Result<Option<JobRecord>> {
        let index = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("pk", string(astrometry_index_key(astrometry_id)))
            .consistent_read(true)
            .send()
            .await?;
        let Some(job_id) = index
            .item()
            .map(|item| required_uuid(item, "job_id"))
            .transpose()?
        else {
            return Ok(None);
        };
        self.get(job_id).await
    }

    async fn find_by_object_key(&self, object_key: &str) -> Result<Option<JobRecord>> {
        let index = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("pk", string(object_index_key(object_key)))
            .consistent_read(true)
            .send()
            .await?;
        if let Some(job_id) = index
            .item()
            .map(|item| required_uuid(item, "job_id"))
            .transpose()?
        {
            return self
                .client
                .get_item()
                .table_name(&self.table)
                .key("pk", string(job_key(job_id)))
                .consistent_read(true)
                .send()
                .await?
                .item()
                .map(record_from_item)
                .transpose();
        }

        // Compatibility for jobs written before the object-key index existed.
        let mut values = HashMap::new();
        values.insert(":job".into(), string("job"));
        values.insert(":object_key".into(), string(object_key));
        let mut names = HashMap::new();
        names.insert("#entity".into(), "entity".into());
        names.insert("#object_key".into(), "object_key".into());
        self.scan(
            "#entity = :job AND #object_key = :object_key",
            names,
            values,
            Some(1),
        )
        .await?
        .first()
        .map(record_from_item)
        .transpose()
    }

    async fn queue_depth(&self) -> Result<usize> {
        let mut values = HashMap::new();
        values.insert(":job".into(), string("job"));
        values.insert(":queued".into(), string("queued"));
        let mut names = HashMap::new();
        names.insert("#entity".into(), "entity".into());
        names.insert("#status".into(), "status".into());
        Ok(self
            .scan("#entity = :job AND #status = :queued", names, values, None)
            .await?
            .len())
    }

    async fn claim(
        &self,
        requested_job_id: Option<JobId>,
        lease_seconds: u64,
    ) -> Result<Option<JobLease>> {
        let now = Utc::now();
        self.reclaim_expired(now).await?;
        let Some(job) = self.queued_candidate(requested_job_id, now).await? else {
            return Ok(None);
        };
        let lease_token = Uuid::now_v7().to_string();
        let lease_expires_at = now + Duration::seconds(lease_seconds.max(1) as i64);
        let job_update = Update::builder()
            .table_name(&self.table)
            .key("pk", string(job_key(job.id)))
            .condition_expression("#status = :queued")
            .update_expression("SET #status = :solving, started_at = if_not_exists(started_at, :started_at), lease_token = :lease_token, lease_expires_at = :lease_expires_at ADD attempts :one")
            .expression_attribute_names("#status", "status")
            .expression_attribute_values(":queued", string("queued"))
            .expression_attribute_values(":solving", string("solving"))
            .expression_attribute_values(":started_at", string(encode_time(now)))
            .expression_attribute_values(":lease_token", string(&lease_token))
            .expression_attribute_values(":lease_expires_at", string(encode_time(lease_expires_at)))
            .expression_attribute_values(":one", number(1))
            .build()?;
        let client_update = Update::builder()
            .table_name(&self.table)
            .key("pk", string(client_key(&job.owner)))
            .update_expression("SET #entity = :client, last_served_at = :now")
            .expression_attribute_names("#entity", "entity")
            .expression_attribute_values(":client", string("client"))
            .expression_attribute_values(":now", string(encode_time(now)))
            .build()?;
        let claim = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().update(job_update).build())
            .transact_items(TransactWriteItem::builder().update(client_update).build())
            .send()
            .await;
        match claim {
            Ok(_) => {}
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_transaction_canceled_exception()) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error).context("claiming DynamoDB job"),
        }
        Ok(Some(JobLease {
            job_id: job.id,
            lease_token,
            lease_expires_at,
            original_filename: job.original_filename,
            options: job.options,
        }))
    }

    async fn heartbeat(
        &self,
        job_id: JobId,
        lease_token: String,
        lease_seconds: u64,
    ) -> Result<bool> {
        let now = Utc::now();
        let result = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", string(job_key(job_id)))
            .condition_expression(
                "#status = :solving AND lease_token = :lease_token AND lease_expires_at > :now",
            )
            .update_expression("SET lease_expires_at = :lease_expires_at")
            .expression_attribute_names("#status", "status")
            .expression_attribute_values(":solving", string("solving"))
            .expression_attribute_values(":lease_token", string(lease_token))
            .expression_attribute_values(":now", string(encode_time(now)))
            .expression_attribute_values(
                ":lease_expires_at",
                string(encode_time(
                    now + Duration::seconds(lease_seconds.max(1) as i64),
                )),
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
            Err(error) => Err(error).context("renewing DynamoDB lease"),
        }
    }

    async fn input_key(&self, job_id: JobId, lease_token: String) -> Result<Option<String>> {
        let Some(job) = self.get(job_id).await? else {
            return Ok(None);
        };
        let item = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("pk", string(job_key(job_id)))
            .send()
            .await?
            .item()
            .cloned();
        let active = item.as_ref().is_some_and(|item| {
            optional_string(item, "lease_token").as_deref() == Some(&lease_token)
                && optional_string(item, "lease_expires_at")
                    .and_then(|value| decode_time(&value).ok())
                    .is_some_and(|expires_at| expires_at > Utc::now())
        });
        Ok((job.status == JobStatus::Solving && active).then(|| job.input_object_key().to_owned()))
    }

    async fn complete(
        &self,
        job_id: JobId,
        lease_token: String,
        solution: Option<SolutionResponse>,
        error: Option<String>,
    ) -> Result<bool> {
        if solution.is_none() && error.is_none() {
            bail!("worker completion requires a solution or an error");
        }
        let now = Utc::now();
        let request = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", string(job_key(job_id)))
            .condition_expression("#status = :solving AND lease_token = :lease_token AND lease_expires_at > :now")
            .update_expression("SET #status = :status, completed_at = :completed_at, solution_json = :solution_json, #error = :error REMOVE lease_token, lease_expires_at")
            .expression_attribute_names("#status", "status")
            .expression_attribute_names("#error", "error")
            .expression_attribute_values(":solving", string("solving"))
            .expression_attribute_values(":status", string(if solution.is_some() { "succeeded" } else { "failed" }))
            .expression_attribute_values(":lease_token", string(lease_token))
            .expression_attribute_values(":now", string(encode_time(now)))
            .expression_attribute_values(":completed_at", string(encode_time(now)))
            .expression_attribute_values(":solution_json", nullable_json(solution)?)
            .expression_attribute_values(":error", error.map_or(AttributeValue::Null(true), string));
        let result = request.send().await;
        match result {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_conditional_check_failed_exception()) =>
            {
                Ok(false)
            }
            Err(error) => Err(error).context("completing DynamoDB job"),
        }
    }

    async fn retry_failed(&self, job_id: JobId, options: SolveOptions) -> Result<bool> {
        let result = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", string(job_key(job_id)))
            .condition_expression("#status = :failed")
            .update_expression("SET #status = :queued, options_json = :options_json REMOVE started_at, completed_at, solution_json, #error, lease_token, lease_expires_at, notification_delivered_at")
            .expression_attribute_names("#status", "status")
            .expression_attribute_names("#error", "error")
            .expression_attribute_values(":failed", string("failed"))
            .expression_attribute_values(":queued", string("queued"))
            .expression_attribute_values(":options_json", string(serde_json::to_string(&options)?))
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
            Err(error) => Err(error).context("retrying failed DynamoDB job"),
        }
    }

    async fn donate_validation(&self, job_id: JobId, donation: ValidationDonation) -> Result<bool> {
        let result = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", string(job_key(job_id)))
            .condition_expression("#status = :succeeded OR #status = :failed")
            .update_expression("SET validation_object_key = :object_key, validation_donated_at = if_not_exists(validation_donated_at, :donated_at), validation_comment = :comment, validation_solve_is_invalid = :solve_is_invalid, validation_license_version = :license_version")
            .expression_attribute_names("#status", "status")
            .expression_attribute_values(":succeeded", string("succeeded"))
            .expression_attribute_values(":failed", string("failed"))
            .expression_attribute_values(":object_key", string(donation.object_key))
            .expression_attribute_values(":donated_at", string(encode_time(donation.donated_at)))
            .expression_attribute_values(":license_version", string(donation.license_version))
            .expression_attribute_values(
                ":solve_is_invalid",
                AttributeValue::Bool(donation.solve_is_invalid),
            )
            .expression_attribute_values(
                ":comment",
                donation
                    .comment
                    .map_or(AttributeValue::Null(true), string),
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
            Err(error) => Err(error).context("donating DynamoDB job to validation set"),
        }
    }

    async fn pending_notifications(&self, limit: usize) -> Result<Vec<JobId>> {
        self.reclaim_expired(Utc::now()).await?;
        let mut values = HashMap::new();
        values.insert(":job".into(), string("job"));
        values.insert(":queued".into(), string("queued"));
        let mut names = HashMap::new();
        names.insert("#entity".into(), "entity".into());
        names.insert("#status".into(), "status".into());
        self.scan(
            "#entity = :job AND #status = :queued AND attribute_not_exists(notification_delivered_at)",
            names,
            values,
            Some(limit),
        )
        .await?
        .into_iter()
        .map(|item| required_uuid(&item, "id"))
        .collect()
    }

    async fn mark_notification_delivered(&self, job_id: JobId) -> Result<()> {
        self.client
            .update_item()
            .table_name(&self.table)
            .key("pk", string(job_key(job_id)))
            .update_expression("SET notification_delivered_at = :now")
            .expression_attribute_values(":now", string(encode_time(Utc::now())))
            .send()
            .await?;
        Ok(())
    }
}

fn job_item(job: &JobRecord) -> Result<Item> {
    let mut item = HashMap::from([
        ("pk".into(), string(job_key(job.id))),
        ("entity".into(), string("job")),
        ("id".into(), string(job.id)),
        ("astrometry_id".into(), number(job.astrometry_id)),
        ("owner".into(), string(&job.owner)),
        ("queue_weight".into(), number(job.queue_weight)),
        ("object_key".into(), string(&job.object_key)),
        ("original_filename".into(), string(&job.original_filename)),
        (
            "options_json".into(),
            string(serde_json::to_string(&job.options)?),
        ),
        ("status".into(), string(job.status.as_str())),
        ("created_at".into(), string(encode_time(job.created_at))),
        ("attempts".into(), number(0)),
    ]);
    if let Some(content_type) = &job.content_type {
        item.insert("content_type".into(), string(content_type));
    }
    Ok(item)
}

fn record_from_item(item: &Item) -> Result<JobRecord> {
    Ok(JobRecord {
        id: required_uuid(item, "id")?,
        astrometry_id: required_number(item, "astrometry_id")?
            .parse()
            .context("DynamoDB Astrometry ID is not a u64")?,
        owner: required_string(item, "owner")?,
        queue_weight: required_number(item, "queue_weight")?.parse()?,
        object_key: required_string(item, "object_key")?,
        original_filename: required_string(item, "original_filename")?,
        content_type: optional_string(item, "content_type"),
        options: serde_json::from_str(&required_string(item, "options_json")?)?,
        status: JobStatus::parse(&required_string(item, "status")?).map_err(anyhow::Error::msg)?,
        created_at: decode_time(&required_string(item, "created_at")?)?,
        started_at: optional_string(item, "started_at")
            .map(|value| decode_time(&value))
            .transpose()?,
        completed_at: optional_string(item, "completed_at")
            .map(|value| decode_time(&value))
            .transpose()?,
        solution: optional_string(item, "solution_json")
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
        error: optional_string(item, "error"),
        validation_donation: optional_string(item, "validation_object_key")
            .map(|object_key| -> Result<ValidationDonation> {
                Ok(ValidationDonation {
                    object_key,
                    comment: optional_string(item, "validation_comment"),
                    solve_is_invalid: optional_bool(item, "validation_solve_is_invalid")
                        .unwrap_or(false),
                    license_version: required_string(item, "validation_license_version")?,
                    donated_at: decode_time(&required_string(item, "validation_donated_at")?)?,
                })
            })
            .transpose()?,
    })
}

fn string(value: impl ToString) -> AttributeValue {
    AttributeValue::S(value.to_string())
}
fn number(value: impl ToString) -> AttributeValue {
    AttributeValue::N(value.to_string())
}
fn nullable_json(value: Option<SolutionResponse>) -> Result<AttributeValue> {
    match value {
        Some(value) => Ok(AttributeValue::S(serde_json::to_string(&value)?)),
        None => Ok(AttributeValue::Null(true)),
    }
}
fn job_key(job_id: JobId) -> String {
    format!("JOB#{job_id}")
}
fn legacy_index_key(legacy_id: LegacyJobId) -> String {
    format!("LEGACY#{legacy_id}")
}
fn astrometry_index_key(astrometry_id: AstrometryId) -> String {
    format!("ASTROMETRY#{astrometry_id}")
}
fn client_key(owner: &str) -> String {
    format!("CLIENT#{owner}")
}
fn object_index_key(object_key: &str) -> String {
    format!("OBJECT#{object_key}")
}
fn encode_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}
fn decode_time(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}
fn optional_string(item: &Item, name: &str) -> Option<String> {
    item.get(name).and_then(|value| value.as_s().ok()).cloned()
}
fn optional_bool(item: &Item, name: &str) -> Option<bool> {
    item.get(name)
        .and_then(|value| value.as_bool().ok())
        .copied()
}
fn required_string(item: &Item, name: &str) -> Result<String> {
    optional_string(item, name).with_context(|| format!("DynamoDB item is missing string {name}"))
}
fn required_uuid(item: &Item, name: &str) -> Result<Uuid> {
    Uuid::parse_str(&required_string(item, name)?)
        .with_context(|| format!("DynamoDB item has invalid UUID {name}"))
}
fn required_number(item: &Item, name: &str) -> Result<String> {
    item.get(name)
        .and_then(|value| value.as_n().ok())
        .cloned()
        .with_context(|| format!("DynamoDB item is missing number {name}"))
}
