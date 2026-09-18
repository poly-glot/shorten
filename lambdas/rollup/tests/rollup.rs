use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use flate2::read::GzDecoder;
use rollup::{LogRow, LogSource, Rollup, Summary};
use shared::error::AppError;
use shared::stats::StatsDay;
use shared::table::DynamoRepo;
use shared::testing::{link, local_repo};

const CS_URI_QUERY: usize = 11;
const CS_URI_STEM: usize = 7;
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../analytics/fixtures/cloudfront-2026-01-02.gz");
const HEADER_LINES: usize = 2;
const REDIRECT: &str = "302";
const SC_STATUS: usize = 8;
const SECRET: &str = "a-management-secret";

struct Scripted(Vec<LogRow>);

impl LogSource for Scripted {
    async fn rows(&self, _date: NaiveDate) -> Result<Vec<LogRow>, AppError> {
        Ok(self.0.clone())
    }
}

fn fixture_rows() -> Vec<LogRow> {
    let file = std::fs::File::open(FIXTURE).expect("the fixture log is present");
    let mut text = String::new();
    GzDecoder::new(file).read_to_string(&mut text).expect("the fixture log is gzip");

    let mut grouped: BTreeMap<(String, String), u64> = BTreeMap::new();
    for line in text.lines().skip(HEADER_LINES) {
        let fields: Vec<&str> = line.split('\t').collect();
        let (Some(&stem), Some(&status), Some(&query)) = (fields.get(CS_URI_STEM), fields.get(SC_STATUS), fields.get(CS_URI_QUERY)) else {
            panic!("a log line short of its columns: {line:?}");
        };
        if status != REDIRECT {
            continue;
        }
        *grouped.entry((stem.to_string(), query.to_string())).or_default() += 1;
    }

    grouped
        .into_iter()
        .map(|((uri_stem, uri_query), clicks)| LogRow { clicks, uri_query, uri_stem })
        .collect()
}

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, 2).expect("a real date")
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 3, 2, 15, 0).single().expect("a real instant")
}

fn unpaced(repo: DynamoRepo) -> Rollup<Scripted> {
    Rollup {
        write_interval: Duration::ZERO,
        ..Rollup::new(repo, Scripted(fixture_rows()))
    }
}

async fn seed(repo: &DynamoRepo, code: &str) {
    repo.put_link_if_absent(code, &link("https://example.com/target", Vec::new(), SECRET, now()))
        .await
        .expect("create link");
}

async fn stats(repo: &DynamoRepo, code: &str, date: NaiveDate) -> Option<StatsDay> {
    repo.get_stats_days(code, date, date).await.expect("get stats days").into_iter().next()
}

async fn clicks_total(repo: &DynamoRepo, code: &str) -> u64 {
    repo.get_link(code, now()).await.expect("get link").expect("the link is present").clicks_total
}

fn segments(day: &StatsDay) -> Vec<(&str, u64)> {
    day.seg.iter().map(|(segment, clicks)| (segment.as_str(), *clicks)).collect()
}

#[test]
fn the_gzip_fixture_yields_one_grouped_row_per_stem_and_querystring() {
    let rows = fixture_rows();
    let pairs: Vec<(&str, &str, u64)> = rows.iter().map(|row| (row.uri_stem.as_str(), row.uri_query.as_str(), row.clicks)).collect();

    assert_eq!(
        pairs,
        [
            ("/../../etc/pwd", "s=IN|MH|android|mobile", 1),
            ("/Gh8pQr2vN5", "s=IN|KA|android|mobile", 1),
            ("/Zq7wLn4Tf1", "s=DE%7cBE%7cother%7cdesktop", 1),
            ("/Zq7wLn4Tf1", "s=DE|BE|other|desktop", 2),
            ("/Zq7wLn4Tf1", "s=XX|XX|other|other", 1),
            ("/aB3xK9mQ2p", "code=aB3xK9mQ2p", 1),
            ("/aB3xK9mQ2p", "s=IN%7CMH%7Candroid", 1),
            ("/aB3xK9mQ2p", "s=IN%7CMH%7Candroid%7Cmobile", 2),
            ("/aB3xK9mQ2p", "s=IN|MH|android", 1),
            ("/aB3xK9mQ2p", "s=IN|MH|android|mobile", 3),
            ("/aB3xK9mQ2p", "s=US|CA|ios|tablet", 1),
            ("/short", "s=IN|MH|android|mobile", 1),
        ],
        "the tab-delimited, two-header-line, gzip assumptions hold"
    );
}

#[test]
fn only_redirects_survive_the_status_filter_the_query_applies() {
    let rows = fixture_rows();
    let Some(popular) = rows
        .iter()
        .find(|row| row.uri_stem == "/aB3xK9mQ2p" && row.uri_query == "s=IN|MH|android|mobile")
    else {
        panic!("the busiest pair is missing from {rows:?}");
    };
    let non_redirect_stems: Vec<&str> = rows
        .iter()
        .map(|row| row.uri_stem.as_str())
        .filter(|stem| ["/", "/api/links"].contains(stem))
        .collect();

    assert_eq!(popular.clicks, 3, "the 404 on the same pair is not counted");
    assert_eq!(non_redirect_stems, Vec::<&str>::new(), "the frontend and the api never reach the rollup");
}

#[tokio::test]
async fn a_fixture_day_writes_the_exact_per_code_and_per_segment_counts() {
    let Some(repo) = local_repo("rollup-test").await else {
        return;
    };
    seed(&repo, "aB3xK9mQ2p").await;
    seed(&repo, "Zq7wLn4Tf1").await;

    let summary = unpaced(repo.clone()).run(day()).await.expect("rollup");

    let busiest = stats(&repo, "aB3xK9mQ2p", day()).await.expect("the busiest link has a stats day");
    let quieter = stats(&repo, "Zq7wLn4Tf1", day()).await.expect("the quieter link has a stats day");

    assert_eq!(
        summary,
        Summary {
            clicks: 14,
            codes: 3,
            days_advanced: 2,
            discarded_clicks: 2,
            rows: 12,
            unsegmented_clicks: 3,
        },
        "the summary counts what it read and what it wrote"
    );
    assert_eq!(
        (busiest.clicks, segments(&busiest)),
        (9, vec![("IN|MH|android|mobile", 5), ("US|CA|ios|tablet", 1), ("XX|XX|other|other", 3)]),
        "the busiest link keeps a count per segment, whichever way cloudfront encoded it"
    );
    assert_eq!(
        (quieter.clicks, segments(&quieter)),
        (4, vec![("DE|BE|other|desktop", 3), ("XX|XX|other|other", 1)]),
        "the quieter link keeps a count per segment"
    );
    assert_eq!(
        (clicks_total(&repo, "aB3xK9mQ2p").await, clicks_total(&repo, "Zq7wLn4Tf1").await),
        (9, 4),
        "each link's running total is its day total"
    );
}

#[tokio::test]
async fn a_second_invocation_of_the_same_day_changes_nothing() {
    let Some(repo) = local_repo("rollup-test").await else {
        return;
    };
    seed(&repo, "aB3xK9mQ2p").await;
    seed(&repo, "Zq7wLn4Tf1").await;

    let first = unpaced(repo.clone()).run(day()).await.expect("rollup");
    let after_first = stats(&repo, "aB3xK9mQ2p", day()).await.expect("a stats day");
    let total_after_first = clicks_total(&repo, "aB3xK9mQ2p").await;

    let second = unpaced(repo.clone()).run(day()).await.expect("rollup");
    let after_second = stats(&repo, "aB3xK9mQ2p", day()).await.expect("a stats day");

    assert_eq!(after_second, after_first, "the stats item is identical after a replay");
    assert_eq!(
        (clicks_total(&repo, "aB3xK9mQ2p").await, total_after_first),
        (9, 9),
        "the running total does not double count"
    );
    assert_eq!(
        (second.clicks, second.days_advanced, first.days_advanced),
        (first.clicks, 0, 2),
        "the replay reads the same clicks but advances no link"
    );
}

#[tokio::test]
async fn a_rollup_for_an_older_date_after_a_newer_one_leaves_the_running_total_alone() {
    let Some(repo) = local_repo("rollup-test").await else {
        return;
    };
    seed(&repo, "aB3xK9mQ2p").await;
    let older = day() - chrono::Duration::days(1);

    unpaced(repo.clone()).run(day()).await.expect("rollup");
    let backdated = unpaced(repo.clone()).run(older).await.expect("rollup");

    assert_eq!(
        (clicks_total(&repo, "aB3xK9mQ2p").await, backdated.days_advanced),
        (9, 0),
        "a backfill of an earlier day never adds to the total again"
    );
    assert_eq!(
        stats(&repo, "aB3xK9mQ2p", older).await.map(|day| day.clicks),
        Some(9),
        "the earlier day still gets its own stats item"
    );
}

#[tokio::test]
async fn a_code_with_no_link_gets_its_stats_day_without_resurrecting_the_link() {
    let Some(repo) = local_repo("rollup-test").await else {
        return;
    };

    unpaced(repo.clone()).run(day()).await.expect("rollup");

    assert_eq!(
        stats(&repo, "Gh8pQr2vN5", day()).await.map(|day| day.clicks),
        Some(1),
        "the stats item is written whether or not the link still exists"
    );
    assert_eq!(
        repo.get_link("Gh8pQr2vN5", now()).await.expect("get link"),
        None,
        "late logs never recreate a deleted link"
    );
}

#[tokio::test]
async fn a_malformed_segment_counts_under_the_unknown_segment_rather_than_vanishing() {
    let Some(repo) = local_repo("rollup-test").await else {
        return;
    };
    seed(&repo, "aB3xK9mQ2p").await;

    unpaced(repo.clone()).run(day()).await.expect("rollup");

    let busiest = stats(&repo, "aB3xK9mQ2p", day()).await.expect("a stats day");
    let raw_segments: Vec<&str> = segments(&busiest)
        .into_iter()
        .map(|(segment, _)| segment)
        .filter(|segment| ["IN%7CMH%7Candroid", "IN|MH|android", "code=aB3xK9mQ2p"].contains(segment))
        .collect();
    let unknown = segments(&busiest).into_iter().find(|(segment, _)| *segment == "XX|XX|other|other");

    assert_eq!(raw_segments, Vec::<&str>::new(), "a malformed segment never becomes a key");
    assert_eq!(
        (busiest.clicks, unknown),
        (9, Some(("XX|XX|other|other", 3))),
        "the three unsegmented rows for this code are counted under the unknown segment"
    );
}

#[tokio::test]
async fn a_path_that_is_not_a_code_gets_no_stats_item() {
    let Some(repo) = local_repo("rollup-test").await else {
        return;
    };

    unpaced(repo.clone()).run(day()).await.expect("rollup");

    assert_eq!(
        repo.get_stats_days("short", day(), day()).await.expect("get stats days"),
        Vec::new(),
        "a path that is not a code gets no stats item"
    );
}
