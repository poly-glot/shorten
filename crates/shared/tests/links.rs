use chrono::{Duration, TimeZone, Utc};
use shared::error::AppError;
use shared::link::{Link, Rule};
use shared::secret;
use shared::table::DynamoRepo;
use shared::testing::{link, local_repo, rule};

const SECRET: &str = "a-management-secret";

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 12, 0, 0).single().expect("a real instant")
}

async fn stored(repo: &DynamoRepo, code: &str) -> Link {
    repo.get_link(code, now()).await.expect("get link").expect("the link is present")
}

#[tokio::test]
async fn a_created_link_reads_back_with_its_rules_and_secret_hash() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };
    let created = link("https://example.com/default", vec![rule("https://example.com/rule")], SECRET, now());

    assert!(repo.put_link_if_absent("aB3xK9mQ2p", &created).await.expect("create"), "the first create wins");

    let read = stored(&repo, "aB3xK9mQ2p").await;
    assert_eq!(read, created, "the row round trips unchanged");
    assert!(secret::verify(SECRET, &read.secret_hash), "the stored hash verifies the secret");
}

#[tokio::test]
async fn a_second_create_on_the_same_code_loses_its_condition() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };
    let first = link("https://example.com/first", Vec::new(), SECRET, now());
    let second = link("https://example.com/second", Vec::new(), SECRET, now());

    assert!(repo.put_link_if_absent("Collision1", &first).await.expect("create"), "the first create wins");
    assert!(
        !repo.put_link_if_absent("Collision1", &second).await.expect("create"),
        "the second create loses"
    );
    assert_eq!(stored(&repo, "Collision1").await.url, "https://example.com/first", "the winner is untouched");
}

#[tokio::test]
async fn a_link_past_its_ttl_reads_as_absent() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };
    let expired = Link {
        ttl: (now() - Duration::days(1)).timestamp(),
        ..link("https://example.com/old", Vec::new(), SECRET, now() - Duration::days(1900))
    };

    repo.put_link_if_absent("Expired001", &expired).await.expect("create");

    assert_eq!(
        repo.get_link("Expired001", now()).await.expect("get link"),
        None,
        "ttl lag does not resurrect a link"
    );
}

#[tokio::test]
async fn an_update_replaces_the_url_and_the_rules_it_is_given() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };
    let created = link("https://example.com/default", vec![rule("https://example.com/old")], SECRET, now());
    repo.put_link_if_absent("Updatable1", &created).await.expect("create");

    let replacement = vec![Rule {
        countries: Some(vec!["IN".into()]),
        ..rule("https://example.com/new")
    }];
    assert!(
        repo.update_link("Updatable1", Some("https://example.com/moved"), Some(&replacement))
            .await
            .expect("update"),
        "the update finds the link"
    );

    let read = stored(&repo, "Updatable1").await;
    assert_eq!(
        (read.url.as_str(), read.rules.as_slice()),
        ("https://example.com/moved", replacement.as_slice())
    );
}

#[tokio::test]
async fn an_update_leaves_the_field_it_was_not_given_alone() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };
    let created = link("https://example.com/default", vec![rule("https://example.com/keep")], SECRET, now());
    repo.put_link_if_absent("Partial001", &created).await.expect("create");

    repo.update_link("Partial001", Some("https://example.com/moved"), None).await.expect("update");

    let read = stored(&repo, "Partial001").await;
    assert_eq!(
        (read.url.as_str(), read.rules.as_slice()),
        ("https://example.com/moved", created.rules.as_slice())
    );
}

#[tokio::test]
async fn an_update_with_nothing_to_set_is_a_bad_request() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };

    let refused = repo.update_link("Anything01", None, None).await;
    assert!(matches!(refused, Err(AppError::BadRequest(_))), "got {refused:?}");
}

#[tokio::test]
async fn updating_and_deleting_an_absent_link_report_that_it_was_not_there() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };

    let updated = repo.update_link("Ghost00001", Some("https://example.com/x"), None).await.expect("update");
    let deleted = repo.delete_link("Ghost00001").await.expect("delete");

    assert_eq!((updated, deleted), (false, false), "neither call invents a row");
}

#[tokio::test]
async fn a_deleted_link_is_gone() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };
    repo.put_link_if_absent("Deletable1", &link("https://example.com/x", Vec::new(), SECRET, now()))
        .await
        .expect("create");

    assert!(repo.delete_link("Deletable1").await.expect("delete"), "the delete finds the link");
    assert_eq!(repo.get_link("Deletable1", now()).await.expect("get link"), None, "the row is gone");
}

#[tokio::test]
async fn a_first_rollup_adds_the_day_total_and_stamps_the_date() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };
    repo.put_link_if_absent("Rollup0001", &link("https://example.com/x", Vec::new(), SECRET, now()))
        .await
        .expect("create");

    let applied = repo.add_rollup_clicks("Rollup0001", "2026-01-02", 7).await.expect("add rollup clicks");

    let read = stored(&repo, "Rollup0001").await;
    assert_eq!(
        (applied, read.clicks_total, read.last_rollup.as_deref()),
        (true, 7, Some("2026-01-02")),
        "the first rollup for a day applies"
    );
}

#[tokio::test]
async fn a_replayed_rollup_for_the_same_date_is_a_no_op() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };
    repo.put_link_if_absent("Rollup0002", &link("https://example.com/x", Vec::new(), SECRET, now()))
        .await
        .expect("create");
    repo.add_rollup_clicks("Rollup0002", "2026-01-02", 7).await.expect("add rollup clicks");

    let replayed = repo.add_rollup_clicks("Rollup0002", "2026-01-02", 7).await.expect("add rollup clicks");

    let read = stored(&repo, "Rollup0002").await;
    assert_eq!(
        (replayed, read.clicks_total, read.last_rollup.as_deref()),
        (false, 7, Some("2026-01-02")),
        "a second run for the same day adds nothing"
    );
}

#[tokio::test]
async fn a_rollup_for_a_date_already_passed_is_a_no_op() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };
    repo.put_link_if_absent("Rollup0003", &link("https://example.com/x", Vec::new(), SECRET, now()))
        .await
        .expect("create");
    repo.add_rollup_clicks("Rollup0003", "2026-01-02", 7).await.expect("add rollup clicks");

    let backdated = repo.add_rollup_clicks("Rollup0003", "2026-01-01", 5).await.expect("add rollup clicks");

    let read = stored(&repo, "Rollup0003").await;
    assert_eq!(
        (backdated, read.clicks_total, read.last_rollup.as_deref()),
        (false, 7, Some("2026-01-02")),
        "an out-of-order replay never rewinds the stamp"
    );
}

#[tokio::test]
async fn a_rollup_for_a_later_date_adds_on_top_and_moves_the_stamp() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };
    repo.put_link_if_absent("Rollup0004", &link("https://example.com/x", Vec::new(), SECRET, now()))
        .await
        .expect("create");
    repo.add_rollup_clicks("Rollup0004", "2026-01-02", 7).await.expect("add rollup clicks");

    let applied = repo.add_rollup_clicks("Rollup0004", "2026-01-03", 5).await.expect("add rollup clicks");

    let read = stored(&repo, "Rollup0004").await;
    assert_eq!(
        (applied, read.clicks_total, read.last_rollup.as_deref()),
        (true, 12, Some("2026-01-03")),
        "the next day accumulates onto the running total"
    );
}

#[tokio::test]
async fn a_rollup_for_a_link_that_is_gone_never_recreates_it() {
    let Some(repo) = local_repo("links-test").await else {
        return;
    };

    let applied = repo.add_rollup_clicks("Rollup0005", "2026-01-02", 7).await.expect("add rollup clicks");

    assert_eq!(
        (applied, repo.get_link("Rollup0005", now()).await.expect("get link")),
        (false, None),
        "late logs for a deleted link do not resurrect a partial row"
    );
}
