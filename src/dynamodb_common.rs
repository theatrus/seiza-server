//! Attribute and time helpers shared by the DynamoDB job and identity
//! repositories.

use anyhow::{Context, Result};
use aws_sdk_dynamodb::types::AttributeValue;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use uuid::Uuid;

pub(crate) type Item = HashMap<String, AttributeValue>;

pub(crate) fn string(value: impl ToString) -> AttributeValue {
    AttributeValue::S(value.to_string())
}

pub(crate) fn number(value: impl ToString) -> AttributeValue {
    AttributeValue::N(value.to_string())
}

/// Timestamps are stored as RFC 3339 text and compared lexicographically in
/// condition expressions. That is sound because every value is UTC (a fixed
/// `+00:00` suffix) and chrono's variable-length fractional seconds never
/// reorder distinct instants (`'+' < '0'..'9'`). Do not change this encoding:
/// existing items were written with it, and a different offset or suffix
/// style would not compare correctly against them.
pub(crate) fn encode_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

pub(crate) fn decode_time(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

pub(crate) fn optional_string(item: &Item, name: &str) -> Option<String> {
    match item.get(name) {
        Some(AttributeValue::S(value)) | Some(AttributeValue::N(value)) => Some(value.clone()),
        _ => None,
    }
}

pub(crate) fn required_string(item: &Item, name: &str) -> Result<String> {
    optional_string(item, name)
        .with_context(|| format!("DynamoDB item is missing string/number `{name}`"))
}

pub(crate) fn required_number(item: &Item, name: &str) -> Result<String> {
    item.get(name)
        .and_then(|value| value.as_n().ok())
        .cloned()
        .with_context(|| format!("DynamoDB item is missing number `{name}`"))
}

pub(crate) fn optional_bool(item: &Item, name: &str) -> Option<bool> {
    item.get(name)
        .and_then(|value| value.as_bool().ok())
        .copied()
}

pub(crate) fn required_uuid(item: &Item, name: &str) -> Result<Uuid> {
    Uuid::parse_str(&required_string(item, name)?)
        .with_context(|| format!("DynamoDB item `{name}` is not a UUID"))
}

pub(crate) fn optional_uuid(item: &Item, name: &str) -> Result<Option<Uuid>> {
    optional_string(item, name)
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .with_context(|| format!("DynamoDB item `{name}` is not a UUID"))
}

pub(crate) fn required_time(item: &Item, name: &str) -> Result<DateTime<Utc>> {
    decode_time(&required_string(item, name)?)
}

pub(crate) fn optional_time(item: &Item, name: &str) -> Result<Option<DateTime<Utc>>> {
    optional_string(item, name)
        .as_deref()
        .map(decode_time)
        .transpose()
}

pub(crate) fn insert_optional_string(item: &mut Item, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        item.insert(name.into(), string(value));
    }
}

pub(crate) fn insert_optional_uuid(item: &mut Item, name: &str, value: Option<Uuid>) {
    if let Some(value) = value {
        item.insert(name.into(), string(value));
    }
}

pub(crate) fn insert_optional_time(item: &mut Item, name: &str, value: Option<DateTime<Utc>>) {
    if let Some(value) = value {
        item.insert(name.into(), string(encode_time(value)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn encoded_times_compare_lexicographically_in_utc() {
        let earlier = Utc.with_ymd_and_hms(2026, 7, 18, 10, 0, 0).unwrap();
        let later = earlier + chrono::Duration::milliseconds(5);
        let earlier_text = encode_time(earlier);
        let later_text = encode_time(later);
        assert!(earlier_text < later_text);
        assert!(earlier_text.ends_with("+00:00"));
        assert_eq!(decode_time(&earlier_text).unwrap(), earlier);
    }
}
