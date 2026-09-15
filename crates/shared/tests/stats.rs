use chrono::{NaiveDate, TimeZone, Utc};
use shared::error::AppError;
use shared::stats::MAX_STATS_DAYS;
use shared::testing::{local_repo, stats_day};

fn day(day_of_month: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, day_of_month).expect("a real date")
}

#[tokio::test]
async fn a_window_returns_only_the_days_that_were_written_in_date_order() {
    let Some(repo) = local_repo("stats-test").await else {
        return;
    };
    let written = [
        stats_day(day(3), &[("IN|MH|android|mobile", 4), ("US|CA|ios|tablet", 1)]),
        stats_day(day(1), &[("IN|MH|android|mobile", 2)]),
    ];
    for stats in &written {
        repo.put_stats_day("aB3xK9mQ2p", stats).await.expect("put stats day");
    }

    let read = repo.get_stats_days("aB3xK9mQ2p", day(1), day(5)).await.expect("get stats days");

    let [first, second] = read.as_slice() else {
        panic!("expected the two written days, got {read:?}");
    };
    assert_eq!((first.date, first.clicks, second.date, second.clicks), (day(1), 2, day(3), 5));
}

#[tokio::test]
async fn a_rewritten_day_replaces_the_previous_counts() {
    let Some(repo) = local_repo("stats-test").await else {
        return;
    };
    repo.put_stats_day("Idempotent", &stats_day(day(1), &[("IN|MH|android|mobile", 2)]))
        .await
        .expect("put stats day");
    let rerun = stats_day(day(1), &[("IN|MH|android|mobile", 7)]);
    repo.put_stats_day("Idempotent", &rerun).await.expect("put stats day");

    let read = repo.get_stats_days("Idempotent", day(1), day(1)).await.expect("get stats days");

    assert_eq!(read.as_slice(), [rerun].as_slice(), "a replayed rollup overwrites rather than adds");
}

#[tokio::test]
async fn an_empty_window_returns_no_days_rather_than_failing() {
    let Some(repo) = local_repo("stats-test").await else {
        return;
    };

    let read = repo.get_stats_days("NeverSeen1", day(1), day(9)).await.expect("get stats days");

    assert_eq!(read, Vec::new(), "a link with no clicks has no days");
}

#[tokio::test]
async fn a_window_outside_the_cap_is_refused_before_any_read() {
    let Some(repo) = local_repo("stats-test").await else {
        return;
    };
    let from = day(1);
    let cases = [
        ("a window wider than the cap", from, from + chrono::Duration::days(MAX_STATS_DAYS)),
        ("a window that runs backwards", day(9), day(1)),
    ];

    for (label, from, to) in cases {
        let refused = repo.get_stats_days("aB3xK9mQ2p", from, to).await;
        assert!(matches!(refused, Err(AppError::BadRequest(_))), "{label}: got {refused:?}");
    }
}

#[tokio::test]
async fn a_window_exactly_at_the_cap_is_allowed() {
    let Some(repo) = local_repo("stats-test").await else {
        return;
    };
    let from = day(1);

    let read = repo.get_stats_days("aB3xK9mQ2p", from, from + chrono::Duration::days(MAX_STATS_DAYS - 1)).await;

    assert!(read.is_ok(), "a hundred days is the widest allowed window, got {read:?}");
}

#[tokio::test]
async fn the_create_counter_climbs_once_per_call_within_one_ip_day() {
    let Some(repo) = local_repo("ratelimit-test").await else {
        return;
    };
    let now = Utc.with_ymd_and_hms(2026, 1, 2, 12, 0, 0).single().expect("a real instant");

    let mut counts = Vec::new();
    for _ in 0..3 {
        counts.push(repo.count_create("203.0.113.7", now).await.expect("count create"));
    }
    let other_ip = repo.count_create("203.0.113.8", now).await.expect("count create");
    let next_day = repo.count_create("203.0.113.7", now + chrono::Duration::days(1)).await.expect("count create");

    assert_eq!(counts, vec![1, 2, 3], "each create adds one");
    assert_eq!((other_ip, next_day), (1, 1), "a different ip and a different day each start again");
}
