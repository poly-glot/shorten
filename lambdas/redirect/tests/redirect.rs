use aws_config::BehaviorVersion;
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::config::retry::RetryConfig;
use aws_sdk_dynamodb::config::{Credentials, Region};
use chrono::{Duration, Utc};
use lambda_http::http::{Method, Request};
use lambda_http::{Body, Response};
use redirect::{INDEX_PAGE, ORIGIN_VERIFY_HEADER, PUBLIC_CACHE_CONTROL, Redirect};
use shared::link::{LINK_TTL_DAYS, Rule};
use shared::table::DynamoRepo;
use shared::testing::{link, local_repo, rule};

const APP_STORE: &str = "https://apps.apple.com/app/id123456789";
const BIG_SCREEN: &str = "https://example.com/tablet";
const CACHE_CONTROL: &str = "cache-control";
const CONTENT_TYPE: &str = "content-type";
const DEFAULT_URL: &str = "https://example.com/default";
const DEVICE_ROUTED: &str = "Qz7-mK2xLp";
const EXPECTED_SECRET: &str = "cloudfront-origin-secret";
const EXPIRED: &str = "ZzYyXxWwVv";
const LOCATION: &str = "location";
const MAHARASHTRA: &str = "https://example.com/in-mh";
const MANAGE_SECRET: &str = "a-management-secret";
const MOBILE_SITE: &str = "https://example.com/mobile";
const PLAY_STORE: &str = "https://play.google.com/store/apps/details?id=com.example.app";
const ROUTED: &str = "aB3xK9mQ2p";
const UNREACHABLE_ENDPOINT: &str = "http://127.0.0.1:1";
const UNSEEDED: &str = "0123456789";

async fn unreachable_repo() -> DynamoRepo {
    let config = aws_config::defaults(BehaviorVersion::latest())
        .credentials_provider(Credentials::new("local", "local", None, None, "redirect-test"))
        .endpoint_url(UNREACHABLE_ENDPOINT)
        .region(Region::new("eu-west-2"))
        .retry_config(RetryConfig::disabled())
        .load()
        .await;

    DynamoRepo::new(Client::new(&config), "never-read")
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().method(Method::GET).uri(uri).body(Body::Empty).expect("a request")
}

fn verified(uri: &str, secret: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(ORIGIN_VERIFY_HEADER, secret)
        .body(Body::Empty)
        .expect("a request")
}

fn header_of<'a>(response: &'a Response<Body>, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|value| value.to_str().ok())
}

fn status_of(response: &Response<Body>) -> u16 {
    response.status().as_u16()
}

async fn target_for(handler: &Redirect, code: &str, segment: &str) -> (u16, String) {
    let response = handler.handle(get(&format!("/{code}?s={segment}"))).await;
    let target = header_of(&response, LOCATION).unwrap_or_default().to_string();

    (status_of(&response), target)
}

async fn seed(repo: &DynamoRepo, code: &str, url: &str, rules: Vec<Rule>) {
    let stored = link(url, rules, MANAGE_SECRET, Utc::now());
    assert!(
        repo.put_link_if_absent(code, &stored).await.expect("the seed write succeeds"),
        "{code} was seeded once"
    );
}

fn store_rules() -> Vec<Rule> {
    vec![
        Rule {
            countries: Some(vec!["IN".into()]),
            regions: Some(vec!["MH".into()]),
            ..rule(MAHARASHTRA)
        },
        Rule {
            countries: Some(vec!["IN".into(), "PK".into()]),
            platforms: Some(vec!["android".into()]),
            ..rule(PLAY_STORE)
        },
        Rule {
            platforms: Some(vec!["ios".into()]),
            ..rule(APP_STORE)
        },
    ]
}

fn device_rules() -> Vec<Rule> {
    vec![
        Rule {
            devices: Some(vec!["mobile".into()]),
            ..rule(MOBILE_SITE)
        },
        Rule {
            devices: Some(vec!["tablet".into()]),
            ..rule(BIG_SCREEN)
        },
    ]
}

#[tokio::test]
async fn an_invalid_code_answers_without_ever_reaching_the_table() {
    let handler = Redirect::protected_by(unreachable_repo().await, None);
    let cases = [
        ("one character short", "/aB3xK9mQ2"),
        ("one character long", "/aB3xK9mQ2pZ"),
        ("a character outside the alphabet", "/aB3xK9mQ2!"),
        ("a percent escape in the path", "/aB3xK9mQ%32"),
        ("a nested path", "/assets/application.js"),
    ];

    for (label, uri) in cases {
        assert_eq!(status_of(&handler.handle(get(uri)).await), 404, "{label}: {uri}");
    }

    let reached = handler.handle(get("/aB3xK9mQ2p")).await;
    assert_eq!(
        status_of(&reached),
        500,
        "a well-formed code does reach the unreachable table, so the 404s above cannot have reached it either"
    );
}

#[tokio::test]
async fn the_home_page_and_the_favicon_are_served_without_reaching_the_table() {
    let handler = Redirect::protected_by(unreachable_repo().await, None);

    let home = handler.handle(get("/")).await;
    assert_eq!(
        (status_of(&home), header_of(&home, CONTENT_TYPE), home.body().len()),
        (200, Some("text/html; charset=utf-8"), INDEX_PAGE.len()),
        "the embedded frontend is served whole"
    );

    let favicon = handler.handle(get("/favicon.ico")).await;
    assert_eq!((status_of(&favicon), favicon.body().is_empty()), (204, true), "a favicon request costs nothing");
}

#[tokio::test]
async fn a_request_that_is_not_a_get_of_a_known_path_is_a_not_found() {
    let handler = Redirect::protected_by(unreachable_repo().await, None);
    let cases = [
        ("a post to a code", Method::POST, "/aB3xK9mQ2p"),
        ("a head of the home page", Method::HEAD, "/"),
        ("a delete of the favicon", Method::DELETE, "/favicon.ico"),
    ];

    for (label, method, uri) in cases {
        let request = Request::builder().method(method).uri(uri).body(Body::Empty).expect("a request");
        assert_eq!(status_of(&handler.handle(request).await), 404, "{label}");
    }
}

#[tokio::test]
async fn the_origin_verify_header_gates_every_request_when_a_secret_is_configured() {
    let handler = Redirect::protected_by(unreachable_repo().await, Some(EXPECTED_SECRET));
    let open = Redirect::protected_by(unreachable_repo().await, None);

    let cases = [
        (
            "cloudfront presents the configured secret",
            &handler,
            verified("/favicon.ico", EXPECTED_SECRET),
            204,
        ),
        ("a direct caller guesses the secret", &handler, verified("/favicon.ico", "guessed-secret"), 403),
        ("a direct caller sends no header at all", &handler, get("/favicon.ico"), 403),
        ("an unset secret skips the check", &open, get("/favicon.ico"), 204),
        ("an unset secret ignores a stray header", &open, verified("/favicon.ico", "stray"), 204),
    ];

    for (label, handler, request, expected) in cases {
        assert_eq!(status_of(&handler.handle(request).await), expected, "{label}");
    }
}

#[tokio::test]
async fn a_viewer_takes_the_first_rule_that_matches_every_dimension_it_lists() {
    let Some(repo) = local_repo("redirect-test").await else {
        return;
    };
    let handler = Redirect::protected_by(repo.clone(), None);
    seed(&repo, ROUTED, DEFAULT_URL, store_rules()).await;

    let cases = [
        (
            "the first matching rule wins over a later rule that also matches",
            "IN|MH|android|mobile",
            MAHARASHTRA,
        ),
        (
            "a rule listing two dimensions is skipped when only one matches",
            "IN|KA|other|desktop",
            DEFAULT_URL,
        ),
        ("a value later in a list matches just as the first does", "PK|PB|android|mobile", PLAY_STORE),
        ("an omitted dimension matches anything", "DE|BE|ios|desktop", APP_STORE),
        ("the unknown segment falls through to the link default", "XX|XX|other|other", DEFAULT_URL),
        ("a malformed segment falls through to the link default", "not-a-segment", DEFAULT_URL),
    ];

    for (label, segment, expected) in cases {
        assert_eq!(target_for(&handler, ROUTED, segment).await, (302, expected.to_string()), "{label}");
    }
}

#[tokio::test]
async fn a_tablet_viewer_is_not_served_the_mobile_rule_that_precedes_it() {
    let Some(repo) = local_repo("redirect-test").await else {
        return;
    };
    let handler = Redirect::protected_by(repo.clone(), None);
    seed(&repo, DEVICE_ROUTED, DEFAULT_URL, device_rules()).await;

    let cases = [
        ("an ipad takes the tablet rule", "US|CA|ios|tablet", BIG_SCREEN),
        ("a phone takes the mobile rule", "US|CA|ios|mobile", MOBILE_SITE),
        ("a laptop takes neither", "US|CA|other|desktop", DEFAULT_URL),
    ];

    for (label, segment, expected) in cases {
        assert_eq!(target_for(&handler, DEVICE_ROUTED, segment).await, (302, expected.to_string()), "{label}");
    }
}

#[tokio::test]
async fn a_request_without_a_segment_parameter_still_redirects_to_the_link_default() {
    let Some(repo) = local_repo("redirect-test").await else {
        return;
    };
    let handler = Redirect::protected_by(repo.clone(), None);
    seed(&repo, ROUTED, DEFAULT_URL, store_rules()).await;

    let response = handler.handle(get(&format!("/{ROUTED}"))).await;

    assert_eq!(
        (status_of(&response), header_of(&response, LOCATION)),
        (302, Some(DEFAULT_URL)),
        "a misfiring edge function must never cost the viewer their redirect"
    );
}

#[tokio::test]
async fn a_found_link_answers_a_temporary_redirect_cached_for_five_minutes() {
    let Some(repo) = local_repo("redirect-test").await else {
        return;
    };
    let handler = Redirect::protected_by(repo.clone(), None);
    seed(&repo, ROUTED, DEFAULT_URL, Vec::new()).await;

    let response = handler.handle(get(&format!("/{ROUTED}?s=XX|XX|other|other"))).await;

    assert_eq!(
        (status_of(&response), header_of(&response, CACHE_CONTROL)),
        (302, Some(PUBLIC_CACHE_CONTROL)),
        "a 301 would be cached by browsers forever and the rules are editable"
    );
}

#[tokio::test]
async fn a_code_with_no_row_behind_it_is_a_not_found() {
    let Some(repo) = local_repo("redirect-test").await else {
        return;
    };
    let handler = Redirect::protected_by(repo.clone(), None);

    let response = handler.handle(get(&format!("/{UNSEEDED}?s=IN|MH|android|mobile"))).await;

    assert_eq!(
        (status_of(&response), header_of(&response, LOCATION)),
        (404, None),
        "an unknown code carries no destination"
    );
}

#[tokio::test]
async fn a_link_whose_ttl_has_passed_is_a_not_found_before_dynamodb_sweeps_it() {
    let Some(repo) = local_repo("redirect-test").await else {
        return;
    };
    let handler = Redirect::protected_by(repo.clone(), None);
    let long_expired = link(DEFAULT_URL, Vec::new(), MANAGE_SECRET, Utc::now() - Duration::days(LINK_TTL_DAYS + 1));
    assert!(
        repo.put_link_if_absent(EXPIRED, &long_expired).await.expect("the seed write succeeds"),
        "the expired link was seeded"
    );

    let response = handler.handle(get(&format!("/{EXPIRED}?s=IN|MH|android|mobile"))).await;

    assert_eq!(
        (status_of(&response), header_of(&response, LOCATION)),
        (404, None),
        "TTL lag must not resurrect a link"
    );
}
