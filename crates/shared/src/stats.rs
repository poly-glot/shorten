use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use serde_dynamo::aws_sdk_dynamodb_1::from_items;

use crate::error::AppError;
use crate::table::{BATCH_GET_LIMIT, DynamoRepo, Item, METADATA_SK, backoff, key, partition};

pub const MAX_STATS_DAYS: i64 = 100;
pub const STATS_TTL_DAYS: i64 = 90;

const BATCH_ATTEMPTS: u32 = 8;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct StatsDay {
    pub clicks: u64,
    pub date: NaiveDate,
    #[serde(default)]
    pub seg: BTreeMap<String, u64>,
    pub ttl: i64,
}

pub fn ttl_of(date: NaiveDate) -> i64 {
    (date.and_time(NaiveTime::MIN).and_utc() + Duration::days(STATS_TTL_DAYS)).timestamp()
}

pub(crate) fn stats_pk(code: &str, date: NaiveDate) -> String {
    partition("S", format!("{code}#{date}"))
}

fn window_dates(from: NaiveDate, to: NaiveDate) -> Result<impl Iterator<Item = NaiveDate>, AppError> {
    let span = (to - from).num_days() + 1;
    if span < 1 {
        return Err(AppError::BadRequest("from is after to".into()));
    }
    if span > MAX_STATS_DAYS {
        return Err(AppError::BadRequest(format!("a stats window covers at most {MAX_STATS_DAYS} days")));
    }

    Ok(from.iter_days().take(span as usize))
}

fn next_chunk<T>(pending: &mut Vec<T>) -> Option<Vec<T>> {
    if pending.is_empty() {
        return None;
    }

    let size = pending.len().min(BATCH_GET_LIMIT);
    Some(pending.drain(..size).collect())
}

impl DynamoRepo {
    pub async fn put_stats_day(&self, code: &str, day: &StatsDay) -> Result<(), AppError> {
        let keys = [("PK", stats_pk(code, day.date)), ("SK", METADATA_SK.into())];
        self.put(day, &keys, None).await
    }

    pub async fn get_stats_days(&self, code: &str, from: NaiveDate, to: NaiveDate) -> Result<Vec<StatsDay>, AppError> {
        let mut pending: Vec<Item> = window_dates(from, to)?.map(|date| key(stats_pk(code, date), METADATA_SK)).collect();
        let mut items: Vec<Item> = Vec::with_capacity(pending.len());

        for attempt in 0..BATCH_ATTEMPTS {
            let Some(chunk) = next_chunk(&mut pending) else { break };

            let batch = self.batch_get(chunk).await?;
            items.extend(batch.items);

            if batch.unprocessed.is_empty() {
                continue;
            }
            pending.extend(batch.unprocessed);
            backoff(attempt).await;
        }

        if !pending.is_empty() {
            return Err(AppError::Internal(format!("{} stats keys stayed unprocessed", pending.len())));
        }

        let mut days: Vec<StatsDay> = from_items(items)?;
        days.sort_by_key(|day| day.date);
        Ok(days)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    fn day(day_of_month: u32) -> NaiveDate {
        let Some(date) = NaiveDate::from_ymd_opt(2026, 1, day_of_month) else {
            panic!("January {day_of_month} is a real date");
        };
        date
    }

    fn accepted(from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
        match window_dates(from, to) {
            Ok(dates) => dates.collect(),
            Err(err) => panic!("expected an accepted window, got {err:?}"),
        }
    }

    fn refused(from: NaiveDate, to: NaiveDate) -> String {
        match window_dates(from, to) {
            Err(AppError::BadRequest(message)) => message,
            Err(other) => panic!("expected a bad request, got {other:?}"),
            Ok(dates) => panic!("expected a refused window, got {} accepted days", dates.count()),
        }
    }

    #[test]
    fn a_stats_partition_key_carries_the_app_prefix_the_code_and_the_day() {
        assert_eq!(stats_pk("aB3xK9mQ2p", day(2)), "shorten#S#aB3xK9mQ2p#2026-01-02");
    }

    #[test]
    fn a_stats_row_expires_ninety_days_after_its_midnight() {
        assert_eq!(ttl_of(day(2)), 1_775_088_000, "2026-01-02T00:00:00Z plus ninety days");
    }

    #[test]
    fn window_dates_lists_every_day_between_from_and_to_inclusive() {
        let cases = [
            ("a single day window", day(1), day(1), vec![day(1)]),
            ("a three day window", day(1), day(3), vec![day(1), day(2), day(3)]),
        ];

        for (label, from, to, expected) in cases {
            assert_eq!(accepted(from, to), expected, "{label}");
        }
    }

    #[test]
    fn window_dates_exactly_at_the_cap_is_allowed() {
        let from = day(1);
        let to = from + Duration::days(MAX_STATS_DAYS - 1);

        let dates = accepted(from, to);

        assert_eq!(
            (dates.len(), dates.first().copied(), dates.last().copied()),
            (MAX_STATS_DAYS as usize, Some(from), Some(to)),
            "a hundred days is the widest allowed window"
        );
    }

    #[test]
    fn window_dates_is_refused_outside_the_stated_bounds() {
        let from = day(1);
        let cases = [
            (
                "a window wider than the cap",
                from,
                from + Duration::days(MAX_STATS_DAYS),
                "a stats window covers at most 100 days",
            ),
            ("a window that runs backwards", day(9), day(1), "from is after to"),
        ];

        for (label, from, to, expected) in cases {
            assert_eq!(refused(from, to), expected, "{label}");
        }
    }

    #[test]
    fn next_chunk_returns_at_most_the_batch_get_limit_and_leaves_the_rest_pending() {
        let cases = [
            ("no keys pending yields no chunk", 0, None, 0),
            ("a single key is its own chunk", 1, Some(1), 0),
            ("a chunk under the limit takes every key", BATCH_GET_LIMIT - 1, Some(BATCH_GET_LIMIT - 1), 0),
            ("a chunk exactly at the limit takes every key", BATCH_GET_LIMIT, Some(BATCH_GET_LIMIT), 0),
            (
                "a chunk over the limit is capped and leaves a remainder",
                BATCH_GET_LIMIT + 1,
                Some(BATCH_GET_LIMIT),
                1,
            ),
        ];

        for (label, total, expected_chunk, expected_remaining) in cases {
            let mut pending: Vec<u32> = (0..total as u32).collect();

            let chunk = next_chunk(&mut pending);

            assert_eq!((chunk.as_ref().map(Vec::len), pending.len()), (expected_chunk, expected_remaining), "{label}");
        }
    }

    #[test]
    fn next_chunk_drains_from_the_front_and_keeps_the_remainder_in_order() {
        let mut pending: Vec<u32> = (0..(BATCH_GET_LIMIT as u32) + 1).collect();

        let Some(chunk) = next_chunk(&mut pending) else {
            panic!("a non-empty queue always yields a chunk");
        };

        assert_eq!(chunk, (0..BATCH_GET_LIMIT as u32).collect::<Vec<_>>(), "the chunk takes the front of the queue");
        assert_eq!(
            pending.as_slice(),
            [BATCH_GET_LIMIT as u32].as_slice(),
            "the remainder keeps whatever did not fit"
        );
    }
}
