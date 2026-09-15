pub mod athena;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use shared::code;
use shared::error::AppError;
use shared::segment::SEPARATOR;
use shared::stats::{StatsDay, ttl_of};
use shared::table::{DynamoRepo, backoff};

pub const WRITES_PER_SECOND: u64 = 5;

const ENCODED_SEPARATORS: [&str; 2] = ["%7C", "%7c"];
const SEGMENT_FIELDS: usize = 4;
const SEGMENT_PARAM: &str = "s=";
const THROTTLE_ATTEMPTS: u32 = 8;
const WRITE_INTERVAL: Duration = Duration::from_millis(1_000 / WRITES_PER_SECOND);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRow {
    pub clicks: u64,
    pub uri_query: String,
    pub uri_stem: String,
}

pub trait LogSource {
    fn rows(&self, date: NaiveDate) -> impl Future<Output = Result<Vec<LogRow>, AppError>>;
}

#[derive(Debug, Default, Deserialize)]
pub struct RollupEvent {
    #[serde(default)]
    pub date: Option<NaiveDate>,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub clicks: u64,
    pub codes: usize,
    pub days_advanced: usize,
    pub discarded_clicks: u64,
    pub rows: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counted {
    pub clicks: u64,
    pub seg: BTreeMap<String, u64>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Aggregated {
    pub codes: BTreeMap<String, Counted>,
    pub discarded_clicks: u64,
}

pub fn previous_day(now: DateTime<Utc>) -> NaiveDate {
    (now - chrono::Duration::days(1)).date_naive()
}

pub fn target_date(event: &RollupEvent, now: DateTime<Utc>) -> NaiveDate {
    event.date.unwrap_or_else(|| previous_day(now))
}

fn link_code(uri_stem: &str) -> Option<&str> {
    let code = uri_stem.strip_prefix('/')?;
    code::is_valid(code).then_some(code)
}

fn decoded(raw: &str) -> Cow<'_, str> {
    if !raw.contains('%') {
        return Cow::Borrowed(raw);
    }

    let separator = SEPARATOR.to_string();
    let mut decoded = raw.to_string();
    for encoded in ENCODED_SEPARATORS {
        decoded = decoded.replace(encoded, &separator);
    }
    Cow::Owned(decoded)
}

fn is_intact(segment: &str) -> bool {
    segment.split(SEPARATOR).count() == SEGMENT_FIELDS && !segment.split(SEPARATOR).any(str::is_empty)
}

fn segment_of(uri_query: &str) -> Option<Cow<'_, str>> {
    let segment = decoded(uri_query.strip_prefix(SEGMENT_PARAM)?);
    is_intact(&segment).then_some(segment)
}

pub fn aggregate(rows: &[LogRow]) -> Aggregated {
    let mut aggregated = Aggregated::default();

    for row in rows {
        let (Some(code), Some(segment)) = (link_code(&row.uri_stem), segment_of(&row.uri_query)) else {
            aggregated.discarded_clicks += row.clicks;
            continue;
        };

        let counted = aggregated.codes.entry(code.to_string()).or_default();
        counted.clicks += row.clicks;
        *counted.seg.entry(segment.into_owned()).or_default() += row.clicks;
    }

    aggregated
}

fn stats_day(date: NaiveDate, counted: Counted) -> StatsDay {
    StatsDay {
        clicks: counted.clicks,
        date,
        seg: counted.seg,
        ttl: ttl_of(date),
    }
}

fn is_throttled(error: &AppError) -> bool {
    let AppError::Dynamo(dynamo) = error else {
        return false;
    };
    matches!(
        **dynamo,
        aws_sdk_dynamodb::Error::ProvisionedThroughputExceededException(_) | aws_sdk_dynamodb::Error::RequestLimitExceeded(_)
    )
}

pub struct Rollup<S> {
    pub repo: DynamoRepo,
    pub source: S,
    pub write_interval: Duration,
}

impl<S: LogSource> Rollup<S> {
    pub fn new(repo: DynamoRepo, source: S) -> Self {
        Self {
            repo,
            source,
            write_interval: WRITE_INTERVAL,
        }
    }

    async fn paced<T, F, W>(&self, mut write: F) -> Result<T, AppError>
    where
        F: FnMut() -> W,
        W: Future<Output = Result<T, AppError>>,
    {
        tokio::time::sleep(self.write_interval).await;

        for attempt in 0..THROTTLE_ATTEMPTS {
            match write().await {
                Err(error) if is_throttled(&error) => backoff(attempt).await,
                outcome => return outcome,
            }
        }

        Err(AppError::Internal(format!("the table stayed throttled across {THROTTLE_ATTEMPTS} attempts")))
    }

    pub async fn run(&self, date: NaiveDate) -> Result<Summary, AppError> {
        let rows = self.source.rows(date).await?;
        let aggregated = aggregate(&rows);
        let stamp = date.to_string();

        let mut summary = Summary {
            codes: aggregated.codes.len(),
            discarded_clicks: aggregated.discarded_clicks,
            rows: rows.len(),
            ..Summary::default()
        };

        if summary.discarded_clicks > 0 {
            tracing::warn!(
                date = %date,
                discarded_clicks = summary.discarded_clicks,
                outcome = "discarded",
                rows = summary.rows,
            );
        }

        for (code, counted) in aggregated.codes {
            let clicks = counted.clicks;
            let day = stats_day(date, counted);

            self.paced(|| self.repo.put_stats_day(&code, &day)).await?;
            let advanced = self.paced(|| self.repo.add_rollup_clicks(&code, &stamp, clicks)).await?;

            summary.clicks += clicks;
            summary.days_advanced += usize::from(advanced);
        }

        tracing::info!(
            clicks = summary.clicks,
            codes = summary.codes,
            date = %date,
            days_advanced = summary.days_advanced,
            discarded_clicks = summary.discarded_clicks,
            outcome = "rolled_up",
            rows = summary.rows,
        );

        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn row(uri_stem: &str, uri_query: &str, clicks: u64) -> LogRow {
        LogRow {
            clicks,
            uri_query: uri_query.into(),
            uri_stem: uri_stem.into(),
        }
    }

    fn clicks(aggregated: &Aggregated, code: &str) -> u64 {
        aggregated.codes.get(code).map_or(0, |counted| counted.clicks)
    }

    fn counts(aggregated: &Aggregated, code: &str) -> Vec<(String, u64)> {
        aggregated
            .codes
            .get(code)
            .map(|counted| counted.seg.iter().map(|(segment, clicks)| (segment.clone(), *clicks)).collect())
            .unwrap_or_default()
    }

    #[test]
    fn the_nightly_run_rolls_up_the_day_before_it_fires() {
        let cases = [
            ("a cron at 02:15 rolls up yesterday", (2026, 1, 2, 2, 15, 0), (2026, 1, 1)),
            ("the first of a month reaches back into the previous one", (2026, 3, 1, 2, 15, 0), (2026, 2, 28)),
            ("just after midnight still rolls up the previous day", (2026, 1, 1, 0, 5, 0), (2025, 12, 31)),
        ];

        for (label, (year, month, day, hour, minute, second), expected) in cases {
            let now = Utc.with_ymd_and_hms(year, month, day, hour, minute, second).single().expect("a real instant");
            let expected = NaiveDate::from_ymd_opt(expected.0, expected.1, expected.2).expect("a real date");

            assert_eq!(previous_day(now), expected, "{label}");
        }
    }

    #[test]
    fn a_replay_payload_names_the_day_instead_of_yesterday() {
        let now = Utc.with_ymd_and_hms(2026, 1, 2, 2, 15, 0).single().expect("a real instant");
        let replayed = NaiveDate::from_ymd_opt(2025, 12, 24).expect("a real date");

        let cases = [
            ("an empty schedule payload rolls up yesterday", RollupEvent::default(), previous_day(now)),
            ("a payload carrying a date replays it", RollupEvent { date: Some(replayed) }, replayed),
        ];

        for (label, event, expected) in cases {
            assert_eq!(target_date(&event, now), expected, "{label}");
        }
    }

    #[test]
    fn an_eventbridge_schedule_payload_deserialises_without_a_date() {
        let schedule = serde_json::json!({
            "detail": {},
            "detail-type": "Scheduled Event",
            "source": "aws.events",
            "time": "2026-01-02T02:15:00Z",
        });

        let event: RollupEvent = serde_json::from_value(schedule).expect("a schedule payload is a rollup event");
        assert_eq!(event.date, None, "a schedule carries no replay date");
    }

    #[test]
    fn a_replay_payload_deserialises_its_date() {
        let event: RollupEvent = serde_json::from_value(serde_json::json!({ "date": "2025-12-24" })).expect("a replay payload");
        assert_eq!(event.date, NaiveDate::from_ymd_opt(2025, 12, 24), "the replay date survives the payload");
    }

    #[test]
    fn a_uri_stem_yields_a_code_only_when_the_code_is_well_formed() {
        let cases = [
            ("a redirect path", "/aB3xK9mQ2p", Some("aB3xK9mQ2p")),
            ("both extra alphabet symbols", "/----______", Some("----______")),
            ("the frontend root", "/", None),
            ("an api path", "/api/links", None),
            ("a short path", "/short", None),
            ("a traversal attempt", "/../../etc/pwd", None),
            ("a favicon", "/favicon.ico", None),
            ("a stem with no leading slash", "aB3xK9mQ2p", None),
        ];

        for (label, uri_stem, expected) in cases {
            assert_eq!(link_code(uri_stem), expected, "{label}: {uri_stem:?}");
        }
    }

    #[test]
    fn a_uri_query_yields_a_segment_only_when_it_splits_into_four_fields() {
        let cases = [
            ("a canonical segment", "s=IN|MH|android|mobile", Some("IN|MH|android|mobile")),
            ("the unknown segment is a real segment", "s=XX|XX|other|other", Some("XX|XX|other|other")),
            ("three fields", "s=IN|MH|android", None),
            ("five fields", "s=IN|MH|android|mobile|extra", None),
            ("an empty device", "s=IN|MH|android|", None),
            ("an empty country", "s=|MH|android|mobile", None),
            ("separators only", "s=|||", None),
            ("an empty querystring", "", None),
            ("a dash, which is how a log records no querystring", "-", None),
            ("another parameter entirely", "code=aB3xK9mQ2p", None),
            ("the parameter without a value", "s=", None),
            (
                "cloudfront percent encoded every separator",
                "s=IN%7CMH%7Candroid%7Cmobile",
                Some("IN|MH|android|mobile"),
            ),
            (
                "cloudfront percent encoded them in lower case",
                "s=DE%7cBE%7cother%7cdesktop",
                Some("DE|BE|other|desktop"),
            ),
            ("a percent encoded segment short of a field is still malformed", "s=IN%7CMH%7Candroid", None),
            ("a percent encoded segment with an empty field", "s=IN%7C%7Candroid%7Cmobile", None),
        ];

        for (label, uri_query, expected) in cases {
            assert_eq!(segment_of(uri_query).as_deref(), expected, "{label}: {uri_query:?}");
        }
    }

    #[test]
    fn aggregate_sums_a_total_and_a_segment_map_per_code() {
        let rows = [
            row("/aB3xK9mQ2p", "s=IN|MH|android|mobile", 3),
            row("/aB3xK9mQ2p", "s=US|CA|ios|tablet", 1),
            row("/Zq7wLn4Tf1", "s=DE|BE|other|desktop", 2),
            row("/Zq7wLn4Tf1", "s=XX|XX|other|other", 1),
        ];

        let aggregated = aggregate(&rows);

        assert_eq!(
            aggregated.codes.keys().collect::<Vec<_>>(),
            ["Zq7wLn4Tf1", "aB3xK9mQ2p"],
            "every code that saw a redirect gets a bucket"
        );
        assert_eq!(
            (clicks(&aggregated, "aB3xK9mQ2p"), clicks(&aggregated, "Zq7wLn4Tf1")),
            (4, 3),
            "a code's total is the sum of its segments"
        );
        assert_eq!(
            counts(&aggregated, "aB3xK9mQ2p"),
            [("IN|MH|android|mobile".to_string(), 3), ("US|CA|ios|tablet".to_string(), 1)],
            "each segment keeps its own count"
        );
    }

    #[test]
    fn aggregate_folds_repeated_pairs_into_one_bucket() {
        let rows = [row("/aB3xK9mQ2p", "s=IN|MH|android|mobile", 3), row("/aB3xK9mQ2p", "s=IN|MH|android|mobile", 2)];

        let aggregated = aggregate(&rows);

        assert_eq!(
            (clicks(&aggregated, "aB3xK9mQ2p"), counts(&aggregated, "aB3xK9mQ2p")),
            (5, vec![("IN|MH|android|mobile".to_string(), 5)]),
            "a repeated pair adds rather than replaces"
        );
    }

    #[test]
    fn aggregate_discards_a_row_whose_code_or_segment_is_unusable() {
        let cases = [
            ("a path that is not a code", row("/short", "s=IN|MH|android|mobile", 7)),
            ("a traversal attempt", row("/../../etc/pwd", "s=IN|MH|android|mobile", 7)),
            ("the frontend root", row("/", "s=IN|MH|android|mobile", 7)),
            ("an api path", row("/api/links", "s=IN|MH|android|mobile", 7)),
            ("a segment short of four fields", row("/aB3xK9mQ2p", "s=IN|MH|android", 7)),
            ("a percent encoded segment short of four fields", row("/aB3xK9mQ2p", "s=IN%7CMH%7Candroid", 7)),
            ("a segment with an empty field", row("/aB3xK9mQ2p", "s=IN||android|mobile", 7)),
            ("a querystring that is not a segment", row("/aB3xK9mQ2p", "code=aB3xK9mQ2p", 7)),
            ("no querystring at all", row("/aB3xK9mQ2p", "-", 7)),
        ];

        for (label, discarded) in cases {
            let aggregated = aggregate(&[discarded]);
            assert_eq!((aggregated.codes.len(), aggregated.discarded_clicks), (0, 7), "{label}: the row was counted");
        }
    }

    #[test]
    fn aggregate_keeps_the_good_rows_in_a_batch_that_also_carries_bad_ones() {
        let rows = [
            row("/aB3xK9mQ2p", "s=IN|MH|android|mobile", 3),
            row("/short", "s=IN|MH|android|mobile", 7),
            row("/aB3xK9mQ2p", "s=IN|MH|android", 7),
        ];

        let aggregated = aggregate(&rows);

        assert_eq!(
            (aggregated.codes.len(), clicks(&aggregated, "aB3xK9mQ2p"), aggregated.discarded_clicks),
            (1, 3, 14),
            "the well-formed row survives and the rest are counted as lost"
        );
    }

    #[test]
    fn a_percent_encoded_pair_folds_into_the_same_segment_as_its_literal_form() {
        let rows = [
            row("/aB3xK9mQ2p", "s=IN|MH|android|mobile", 3),
            row("/aB3xK9mQ2p", "s=IN%7CMH%7Candroid%7Cmobile", 2),
        ];

        let aggregated = aggregate(&rows);

        assert_eq!(
            (clicks(&aggregated, "aB3xK9mQ2p"), counts(&aggregated, "aB3xK9mQ2p")),
            (5, vec![("IN|MH|android|mobile".to_string(), 5)]),
            "cloudfront's encoding choice never splits one segment into two keys"
        );
    }

    #[test]
    fn a_stats_day_expires_ninety_days_after_the_day_it_counts() {
        let Some(date) = NaiveDate::from_ymd_opt(2026, 1, 2) else {
            panic!("a real date");
        };
        let Some(ninety_days_on) = NaiveDate::from_ymd_opt(2026, 4, 2) else {
            panic!("a real date");
        };
        let counted = Counted {
            clicks: 4,
            seg: BTreeMap::from([("IN|MH|android|mobile".to_string(), 4)]),
        };

        let day = stats_day(date, counted);
        let Some(expiry) = DateTime::from_timestamp(day.ttl, 0) else {
            panic!("a real instant");
        };

        assert_eq!((day.date, day.clicks, expiry.date_naive()), (date, 4, ninety_days_on));
    }

    #[test]
    fn the_write_pace_stays_well_inside_the_shared_write_budget() {
        assert_eq!(
            (WRITES_PER_SECOND, WRITE_INTERVAL),
            (5, Duration::from_millis(200)),
            "the paced interval is the reciprocal of the declared rate"
        );
    }
}
