use aws_sdk_dynamodb::types::ReturnValue;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_dynamo::aws_sdk_dynamodb_1::from_item;

use crate::error::AppError;
use crate::table::{DynamoRepo, METADATA_SK, n, partition, s};

pub const MAX_CREATES_PER_IP_PER_DAY: u64 = 30;
pub const RATE_LIMIT_TTL_DAYS: i64 = 2;

const COUNT_ADVANCE: &str = "ADD #n :one SET #ttl = if_not_exists(#ttl, :ttl)";

pub fn is_throttled(count: u64) -> bool {
    count > MAX_CREATES_PER_IP_PER_DAY
}

fn rate_limit_pk(ip: &str, now: DateTime<Utc>) -> String {
    partition("R", format!("{ip}#{}", now.date_naive()))
}

#[derive(Deserialize)]
struct Counter {
    n: u64,
}

impl DynamoRepo {
    pub async fn count_create(&self, ip: &str, now: DateTime<Utc>) -> Result<u64, AppError> {
        let expires_at = (now + Duration::days(RATE_LIMIT_TTL_DAYS)).timestamp();
        let out = self
            .client()
            .update_item()
            .table_name(self.table())
            .key("PK", s(rate_limit_pk(ip, now)))
            .key("SK", s(METADATA_SK))
            .update_expression(COUNT_ADVANCE)
            .expression_attribute_names("#n", "n")
            .expression_attribute_names("#ttl", "ttl")
            .expression_attribute_values(":one", n(1))
            .expression_attribute_values(":ttl", n(expires_at as u64))
            .return_values(ReturnValue::UpdatedNew)
            .send()
            .await?;

        let Some(attributes) = out.attributes else {
            return Err(AppError::Internal("rate limit counter returned no attributes".into()));
        };

        let counter: Counter = from_item(attributes)?;
        Ok(counter.n)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn instant(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
        let Some(now) = Utc.with_ymd_and_hms(year, month, day, hour, minute, second).single() else {
            panic!("a representable instant");
        };
        now
    }

    #[test]
    fn the_thirty_first_create_of_a_day_is_the_first_one_throttled() {
        let cases = [
            ("the first create", 1, false),
            ("the last create allowed", MAX_CREATES_PER_IP_PER_DAY, false),
            ("one over the ceiling", MAX_CREATES_PER_IP_PER_DAY + 1, true),
        ];

        for (label, count, expected) in cases {
            assert_eq!(is_throttled(count), expected, "{label}");
        }
    }

    #[test]
    fn a_rate_limit_key_is_one_partition_per_ip_per_day() {
        let cases = [
            (
                "the date comes from the instant",
                "203.0.113.7",
                instant(2026, 1, 2, 23, 59, 59),
                "shorten#R#203.0.113.7#2026-01-02",
            ),
            (
                "the time of day does not change the key",
                "203.0.113.7",
                instant(2026, 1, 2, 0, 0, 0),
                "shorten#R#203.0.113.7#2026-01-02",
            ),
            (
                "a later day changes the key",
                "203.0.113.7",
                instant(2026, 1, 3, 0, 0, 0),
                "shorten#R#203.0.113.7#2026-01-03",
            ),
            (
                "the ip is carried through unchanged",
                "2001:db8::1",
                instant(2026, 1, 2, 12, 0, 0),
                "shorten#R#2001:db8::1#2026-01-02",
            ),
        ];

        for (label, ip, now, expected) in cases {
            assert_eq!(rate_limit_pk(ip, now), expected, "{label}");
        }
    }
}
