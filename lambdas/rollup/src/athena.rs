use std::time::{Duration, Instant};

use aws_config::BehaviorVersion;
use aws_sdk_athena::Client;
use aws_sdk_athena::types::{QueryExecutionContext, QueryExecutionState, ResultConfiguration, Row};
use chrono::NaiveDate;
use shared::error::AppError;
use shared::table::backoff;

use crate::{LogRow, LogSource};

const POLL_DEADLINE: Duration = Duration::from_secs(300);
const RESULT_PAGES: usize = 200;

pub struct AthenaSource {
    client: Client,
    database: String,
    output: String,
    table: String,
    workgroup: String,
}

fn failed(stage: &str, detail: impl std::fmt::Display) -> AppError {
    AppError::Internal(format!("athena {stage}: {detail}"))
}

pub fn query(database: &str, table: &str, date: NaiveDate) -> String {
    format!(
        "SELECT cs_uri_stem, cs_uri_query, count(*) AS clicks \
         FROM \"{database}\".\"{table}\" \
         WHERE year = '{year}' AND month = '{month}' AND day = '{day}' AND sc_status = 302 \
         GROUP BY 1, 2",
        year = date.format("%Y"),
        month = date.format("%m"),
        day = date.format("%d"),
    )
}

fn parse_row(row: &Row) -> Option<LogRow> {
    let [uri_stem, uri_query, clicks] = row.data() else {
        return None;
    };

    Some(LogRow {
        clicks: clicks.var_char_value()?.parse().ok()?,
        uri_query: uri_query.var_char_value()?.to_string(),
        uri_stem: uri_stem.var_char_value()?.to_string(),
    })
}

impl AthenaSource {
    pub fn new(client: Client, database: impl Into<String>, table: impl Into<String>, output: impl Into<String>, workgroup: impl Into<String>) -> Self {
        Self {
            client,
            database: database.into(),
            output: output.into(),
            table: table.into(),
            workgroup: workgroup.into(),
        }
    }

    pub async fn from_env() -> Result<Self, std::env::VarError> {
        let client = Client::new(&aws_config::load_defaults(BehaviorVersion::latest()).await);

        Ok(Self::new(
            client,
            std::env::var("GLUE_DATABASE")?,
            std::env::var("GLUE_TABLE")?,
            std::env::var("ATHENA_OUTPUT")?,
            std::env::var("ATHENA_WORKGROUP")?,
        ))
    }

    async fn start(&self, date: NaiveDate) -> Result<String, AppError> {
        let context = QueryExecutionContext::builder().database(&self.database).build();
        let results = ResultConfiguration::builder().output_location(&self.output).build();

        let out = self
            .client
            .start_query_execution()
            .query_execution_context(context)
            .query_string(query(&self.database, &self.table, date))
            .result_configuration(results)
            .work_group(&self.workgroup)
            .send()
            .await
            .map_err(|error| failed("start_query_execution", error))?;

        out.query_execution_id
            .ok_or_else(|| failed("start_query_execution", "returned no execution id"))
    }

    async fn wait(&self, execution_id: &str) -> Result<(), AppError> {
        let started = Instant::now();
        let mut attempt = 0;

        while started.elapsed() < POLL_DEADLINE {
            let out = self
                .client
                .get_query_execution()
                .query_execution_id(execution_id)
                .send()
                .await
                .map_err(|error| failed("get_query_execution", error))?;

            let status = out.query_execution().and_then(|execution| execution.status());
            match status.and_then(|status| status.state()) {
                Some(QueryExecutionState::Succeeded) => return Ok(()),
                Some(state @ (QueryExecutionState::Cancelled | QueryExecutionState::Failed)) => {
                    let reason = status.and_then(|status| status.state_change_reason()).unwrap_or("no reason given");
                    return Err(failed("query", format!("{execution_id} ended {}: {reason}", state.as_str())));
                }
                _ => backoff(attempt).await,
            }

            attempt += 1;
        }

        Err(failed("query", format!("{execution_id} was still running after {POLL_DEADLINE:?}")))
    }

    async fn collect(&self, execution_id: &str) -> Result<Vec<LogRow>, AppError> {
        let mut rows: Vec<LogRow> = Vec::new();
        let mut token: Option<String> = None;
        let mut header_pending = true;

        for _ in 0..RESULT_PAGES {
            let out = self
                .client
                .get_query_results()
                .query_execution_id(execution_id)
                .set_next_token(token)
                .send()
                .await
                .map_err(|error| failed("get_query_results", error))?;

            let page = out.result_set().map(|set| set.rows()).unwrap_or_default();
            let page = if header_pending { page.get(1..).unwrap_or_default() } else { page };
            header_pending = false;

            rows.extend(page.iter().filter_map(parse_row));

            token = out.next_token().map(str::to_string);
            if token.is_none() {
                return Ok(rows);
            }
        }

        Err(failed("get_query_results", format!("{execution_id} returned more than {RESULT_PAGES} pages")))
    }
}

impl LogSource for AthenaSource {
    async fn rows(&self, date: NaiveDate) -> Result<Vec<LogRow>, AppError> {
        let execution_id = self.start(date).await?;
        self.wait(&execution_id).await?;
        self.collect(&execution_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, 2).expect("a real date")
    }

    #[test]
    fn the_query_prunes_on_all_three_projected_partition_columns() {
        let built = query("aws_cloud", "cloudfront_logs", day());
        let cases = [
            ("the year is zero padded to four digits", "year = '2026'"),
            ("the month is zero padded to two digits", "month = '01'"),
            ("the day is zero padded to two digits", "day = '02'"),
        ];

        for (label, predicate) in cases {
            assert!(built.contains(predicate), "{label}: {built}");
        }
    }

    #[test]
    fn the_query_selects_only_the_stem_the_querystring_and_a_count_of_redirects() {
        let built = query("aws_cloud", "cloudfront_logs", day());

        assert_eq!(
            built,
            "SELECT cs_uri_stem, cs_uri_query, count(*) AS clicks \
             FROM \"aws_cloud\".\"cloudfront_logs\" \
             WHERE year = '2026' AND month = '01' AND day = '02' AND sc_status = 302 \
             GROUP BY 1, 2"
        );
    }

    #[test]
    fn a_single_digit_month_and_day_keep_their_leading_zero() {
        let built = query("aws_cloud", "cloudfront_logs", NaiveDate::from_ymd_opt(2026, 9, 7).expect("a real date"));

        assert!(built.contains("month = '09' AND day = '07'"), "{built}");
    }

    #[test]
    fn a_result_row_parses_only_when_it_carries_all_three_columns() {
        let datum = |value: &str| aws_sdk_athena::types::Datum::builder().var_char_value(value).build();
        let row = |values: &[&str]| {
            let mut builder = Row::builder();
            for value in values {
                builder = builder.data(datum(value));
            }
            builder.build()
        };

        let cases = [
            (
                "a grouped redirect count",
                row(&["/aB3xK9mQ2p", "s=IN|MH|android|mobile", "4"]),
                Some(LogRow {
                    clicks: 4,
                    uri_query: "s=IN|MH|android|mobile".into(),
                    uri_stem: "/aB3xK9mQ2p".into(),
                }),
            ),
            ("a row short of a column", row(&["/aB3xK9mQ2p", "s=IN|MH|android|mobile"]), None),
            ("a row with an extra column", row(&["/aB3xK9mQ2p", "s=IN|MH|android|mobile", "4", "5"]), None),
            ("a count that is not a number", row(&["/aB3xK9mQ2p", "s=IN|MH|android|mobile", "many"]), None),
        ];

        for (label, row, expected) in cases {
            assert_eq!(parse_row(&row), expected, "{label}");
        }
    }
}
