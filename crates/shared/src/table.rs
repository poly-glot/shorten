use std::collections::HashMap;
use std::fmt::Display;
use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::{AttributeValue, KeysAndAttributes};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_dynamo::aws_sdk_dynamodb_1::{from_item, to_item};

use crate::error::AppError;
use crate::random;

pub const BACKOFF_CEILING: Duration = Duration::from_secs(3);
pub const BACKOFF_STEP: Duration = Duration::from_millis(50);

pub(crate) const APP: &str = "shorten";
pub(crate) const BATCH_GET_LIMIT: usize = 100;
pub(crate) const METADATA_SK: &str = "#METADATA";
pub(crate) const PK: &str = "PK";
pub(crate) const SK: &str = "SK";

const BACKOFF_MAX_DOUBLINGS: u32 = 16;

pub(crate) type Item = HashMap<String, AttributeValue>;

pub(crate) struct BatchGet {
    pub(crate) items: Vec<Item>,
    pub(crate) unprocessed: Vec<Item>,
}

#[derive(Clone)]
pub struct DynamoRepo {
    client: Client,
    table: String,
}

pub(crate) fn partition(kind: &str, id: impl Display) -> String {
    format!("{APP}#{kind}#{id}")
}

pub(crate) fn s(value: impl Into<String>) -> AttributeValue {
    AttributeValue::S(value.into())
}

pub(crate) fn n(value: u64) -> AttributeValue {
    AttributeValue::N(value.to_string())
}

pub(crate) fn item<T: Serialize>(entity: &T, keys: &[(&str, String)]) -> Result<Item, AppError> {
    let mut item = to_item(entity)?;
    item.extend(keys.iter().map(|(name, value)| ((*name).to_string(), s(value.clone()))));
    Ok(item)
}

pub(crate) fn key(pk: String, sk: &str) -> Item {
    Item::from([(PK.to_string(), s(pk)), (SK.to_string(), s(sk))])
}

pub(crate) fn condition_failed_as_false<T, E: Into<AppError>>(result: Result<T, E>) -> Result<bool, AppError> {
    match result.map_err(Into::into) {
        Ok(_) => Ok(true),
        Err(err) if err.is_condition_failed() => Ok(false),
        Err(err) => Err(err),
    }
}

impl DynamoRepo {
    pub fn new(client: Client, table: impl Into<String>) -> Self {
        Self { client, table: table.into() }
    }

    pub async fn from_env() -> Result<Self, std::env::VarError> {
        let client = Client::new(&aws_config::load_defaults(BehaviorVersion::latest()).await);
        Ok(Self::new(client, std::env::var("TABLE_NAME")?))
    }

    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    pub(crate) fn table(&self) -> &str {
        &self.table
    }

    pub(crate) async fn get<T: DeserializeOwned>(&self, pk: String, sk: &str, consistent: bool) -> Result<Option<T>, AppError> {
        let out = self
            .client
            .get_item()
            .table_name(&self.table)
            .key(PK, s(pk))
            .key(SK, s(sk))
            .consistent_read(consistent)
            .send()
            .await?;

        Ok(out.item.map(from_item).transpose()?)
    }

    pub(crate) async fn put<T: Serialize>(&self, entity: &T, keys: &[(&str, String)], condition: Option<&str>) -> Result<(), AppError> {
        self.client
            .put_item()
            .table_name(&self.table)
            .set_item(Some(item(entity, keys)?))
            .set_condition_expression(condition.map(String::from))
            .send()
            .await?;
        Ok(())
    }

    pub(crate) async fn update(
        &self,
        pk: String,
        sk: &str,
        expression: &str,
        condition: Option<&str>,
        names: &[(&str, &str)],
        values: Vec<(&str, AttributeValue)>,
    ) -> Result<(), AppError> {
        let mut request = self
            .client
            .update_item()
            .table_name(&self.table)
            .key(PK, s(pk))
            .key(SK, s(sk))
            .update_expression(expression)
            .set_condition_expression(condition.map(String::from));

        for (placeholder, name) in names {
            request = request.expression_attribute_names(*placeholder, *name);
        }
        for (placeholder, value) in values {
            request = request.expression_attribute_values(placeholder, value);
        }

        request.send().await?;
        Ok(())
    }

    pub(crate) async fn delete(&self, pk: String, sk: &str, condition: Option<&str>) -> Result<(), AppError> {
        self.client
            .delete_item()
            .table_name(&self.table)
            .key(PK, s(pk))
            .key(SK, s(sk))
            .set_condition_expression(condition.map(String::from))
            .send()
            .await?;
        Ok(())
    }

    pub(crate) async fn batch_get(&self, keys: Vec<Item>) -> Result<BatchGet, AppError> {
        let table = &self.table;
        let request = KeysAndAttributes::builder().set_keys(Some(keys)).build()?;
        let out = self.client.batch_get_item().request_items(table, request).send().await?;

        let items = out.responses.and_then(|mut by_table| by_table.remove(table)).unwrap_or_default();
        let unprocessed = out
            .unprocessed_keys
            .and_then(|mut by_table| by_table.remove(table))
            .map(|pending| pending.keys)
            .unwrap_or_default();

        Ok(BatchGet { items, unprocessed })
    }
}

fn backoff_cap(attempt: u32) -> Duration {
    let doublings = attempt.min(BACKOFF_MAX_DOUBLINGS);
    BACKOFF_STEP.saturating_mul(1u32 << doublings).min(BACKOFF_CEILING)
}

fn jittered(cap: Duration) -> Duration {
    let fraction = random::u64() as f64 / u64::MAX as f64;
    cap.mul_f64(fraction)
}

pub async fn backoff(attempt: u32) {
    tokio::time::sleep(jittered(backoff_cap(attempt))).await;
}

#[cfg(test)]
mod tests {
    use aws_sdk_dynamodb::types::error::{ConditionalCheckFailedException, ProvisionedThroughputExceededException};

    use super::*;

    fn lost_condition() -> aws_sdk_dynamodb::Error {
        aws_sdk_dynamodb::Error::ConditionalCheckFailedException(ConditionalCheckFailedException::builder().build())
    }

    fn throttled() -> aws_sdk_dynamodb::Error {
        aws_sdk_dynamodb::Error::ProvisionedThroughputExceededException(ProvisionedThroughputExceededException::builder().build())
    }

    #[test]
    fn every_partition_key_carries_the_app_prefix() {
        let cases = [
            ("a link", partition("L", "aB3xK9mQ2p"), "shorten#L#aB3xK9mQ2p"),
            ("a stats day", partition("S", "aB3xK9mQ2p#2026-01-02"), "shorten#S#aB3xK9mQ2p#2026-01-02"),
            (
                "a rate limit counter",
                partition("R", "203.0.113.7#2026-01-02"),
                "shorten#R#203.0.113.7#2026-01-02",
            ),
        ];
        for (label, built, expected) in cases {
            assert_eq!(built, expected, "{label}");
        }
    }

    #[test]
    fn condition_failed_as_false_only_swallows_a_lost_condition() {
        let cases: [(&str, Result<(), aws_sdk_dynamodb::Error>, Option<bool>); 3] = [
            ("a lost condition becomes false", Err(lost_condition()), Some(false)),
            ("a throttle is not swallowed", Err(throttled()), None),
            ("a plain success stays true", Ok(()), Some(true)),
        ];

        for (label, result, expected) in cases {
            let outcome = condition_failed_as_false(result);
            match expected {
                Some(want) => assert_eq!(outcome.ok(), Some(want), "{label}"),
                None => assert!(matches!(outcome, Err(AppError::Dynamo(_))), "{label}: got {outcome:?}"),
            }
        }
    }

    #[test]
    fn the_backoff_cap_doubles_from_the_step_and_stops_at_the_ceiling() {
        let cases = [
            ("the first retry waits at most one step", 0, BACKOFF_STEP),
            ("the second doubles", 1, Duration::from_millis(100)),
            ("the sixth is still under the ceiling", 5, Duration::from_millis(1600)),
            ("the seventh would exceed the ceiling", 6, BACKOFF_CEILING),
            ("a far attempt stays at the ceiling", 40, BACKOFF_CEILING),
        ];
        for (label, attempt, expected) in cases {
            assert_eq!(backoff_cap(attempt), expected, "{label}");
        }
    }

    #[test]
    fn backoff_jitter_spans_from_near_zero_to_the_cap() {
        let cap = Duration::from_millis(250);
        let samples: Vec<Duration> = (0..64).map(|_| jittered(cap)).collect();

        assert!(samples.iter().all(|sample| *sample <= cap), "every sample stays inside the cap");
        assert!(samples.iter().any(|sample| *sample < cap / 2), "some sample falls below half the cap");
        assert!(samples.iter().any(|sample| *sample > cap / 2), "some sample falls above half the cap");
    }
}
