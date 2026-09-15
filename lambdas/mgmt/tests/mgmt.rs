use std::sync::Mutex;

use chrono::{Duration, Utc};
use lambda_http::http::{Method, Request, Response};
use lambda_http::{Body, RequestExt};
use mgmt::{CodeSource, MANAGE_SECRET_HEADER, Mgmt, ORIGIN_VERIFY_HEADER};
use serde_json::{Value, json};
use shared::code;
use shared::link::MAX_RULES;
use shared::ratelimit::MAX_CREATES_PER_IP_PER_DAY;
use shared::secret;
use shared::table::DynamoRepo;
use shared::testing::{link, local_repo, stats_day};

const CREATOR_IP: &str = "203.0.113.7";
const FORWARDED_FOR_HEADER: &str = "x-forwarded-for";
const ORIGIN_SECRET: &str = "a-random-origin-header-value";
const TARGET_URL: &str = "https://example.com/landing";

struct Managed {
    code: String,
    secret: String,
}

fn api(repo: &DynamoRepo) -> Mgmt {
    Mgmt {
        base_url: None,
        codes: Box::new(code::generate),
        origin_verify: None,
        repo: repo.clone(),
    }
}

fn scripted(codes: &[&str]) -> CodeSource {
    let queue = Mutex::new(codes.iter().rev().map(|code| (*code).to_string()).collect::<Vec<String>>());

    Box::new(move || queue.lock().expect("the scripted codes").pop().unwrap_or_else(code::generate))
}

fn link_body(url: &str, rules: Value) -> Value {
    json!({ "rules": rules, "url": url })
}

fn plain_body() -> Value {
    link_body(TARGET_URL, json!([]))
}

fn request(method: Method, path: &str, secret: Option<&str>, body: Option<&Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path).header(FORWARDED_FOR_HEADER, CREATOR_IP);

    if let Some(secret) = secret {
        builder = builder.header(MANAGE_SECRET_HEADER, secret);
    }

    let payload = body.map_or(Body::Empty, |body| Body::from(body.to_string()));

    builder.body(payload).expect("a request")
}

fn post(body: &Value) -> Request<Body> {
    request(Method::POST, "/links", None, Some(body))
}

fn get(path: &str, secret: &str) -> Request<Body> {
    request(Method::GET, path, Some(secret), None)
}

fn patch(code: &str, secret: &str, body: &Value) -> Request<Body> {
    request(Method::PATCH, &format!("/links/{code}"), Some(secret), Some(body))
}

fn delete(code: &str, secret: &str) -> Request<Body> {
    request(Method::DELETE, &format!("/links/{code}"), Some(secret), None)
}

fn read(response: Response<Body>) -> (u16, Value) {
    let status = response.status().as_u16();
    let payload = serde_json::from_slice(response.body().as_ref()).unwrap_or(Value::Null);

    (status, payload)
}

async fn answered(api: &Mgmt, request: Request<Body>) -> (u16, Value) {
    read(api.handle(request).await)
}

async fn created(api: &Mgmt, body: &Value) -> Managed {
    let (status, payload) = answered(api, post(body)).await;
    assert_eq!(status, 201, "a valid create is accepted: {payload}");

    let code = payload["code"].as_str().expect("a created link carries its code").to_string();
    let secret = payload["manage_secret"].as_str().expect("a created link carries its secret once").to_string();

    assert_eq!(
        (code::is_valid(&code), payload["short_url"].as_str()),
        (true, Some(format!("http://localhost/{code}").as_str())),
        "the short url is the request origin plus the code"
    );

    Managed { code, secret }
}

async fn viewed(api: &Mgmt, managed: &Managed) -> (u16, Value) {
    let (status, payload) = answered(api, get(&format!("/links/{}", managed.code), &managed.secret)).await;

    assert_eq!(
        payload.get("manage_secret"),
        None,
        "a secret is returned once, at creation, and never read back"
    );

    (status, payload)
}

async fn refused(api: &Mgmt, body: &Value, label: &str) {
    let (status, payload) = answered(api, post(body)).await;

    assert_eq!(
        (status, payload["error"]["code"].as_str()),
        (422, Some("invalid_request")),
        "{label}: {payload}"
    );
}

#[tokio::test]
async fn a_link_is_created_read_edited_and_finally_deleted() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let api = api(&repo);
    let rules = json!([{ "countries": ["IN", "PK"], "platforms": ["android"], "url": "https://play.google.com/store/apps/details?id=com.example" }]);

    let managed = created(&api, &link_body(TARGET_URL, rules.clone())).await;

    let (status, payload) = viewed(&api, &managed).await;
    assert_eq!(
        (status, &payload["url"], &payload["rules"]),
        (200, &json!(TARGET_URL), &rules),
        "the stored link reads back whole"
    );

    let edited = link_body("https://example.com/moved", json!([]));
    let (status, payload) = answered(&api, patch(&managed.code, &managed.secret, &edited)).await;
    assert_eq!((status, &payload["url"]), (200, &json!("https://example.com/moved")), "{payload}");

    let (_, payload) = viewed(&api, &managed).await;
    assert_eq!(
        (&payload["url"], &payload["rules"]),
        (&json!("https://example.com/moved"), &json!([])),
        "a patch replaces the url and the rule list whole"
    );

    let (status, payload) = answered(&api, delete(&managed.code, &managed.secret)).await;
    assert_eq!((status, payload), (204, Value::Null), "a delete answers with no content");

    let (status, _) = viewed(&api, &managed).await;
    assert_eq!(status, 404, "a deleted link is gone");
}

#[tokio::test]
async fn only_the_secret_handed_out_at_creation_opens_a_link() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let api = api(&repo);
    let managed = created(&api, &plain_body()).await;
    let wrong = Managed {
        code: managed.code.clone(),
        secret: secret::generate(),
    };

    let (status, payload) = viewed(&api, &wrong).await;
    assert_eq!(
        (status, payload["error"]["code"].as_str()),
        (401, Some("unauthorized")),
        "a wrong secret is refused"
    );

    let (status, _) = viewed(&api, &managed).await;
    assert_eq!(status, 200, "the issued secret is accepted");

    let unknown = Managed {
        code: code::generate(),
        secret: managed.secret.clone(),
    };
    let (status, _) = viewed(&api, &unknown).await;
    assert_eq!(status, 404, "an unknown code is not found before any secret is weighed");
}

#[tokio::test]
async fn a_rule_list_longer_than_the_limit_is_refused() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let api = api(&repo);
    let rule = json!({ "url": TARGET_URL });
    let at_limit: Vec<Value> = (0..MAX_RULES).map(|_| rule.clone()).collect();
    let over_limit: Vec<Value> = (0..=MAX_RULES).map(|_| rule.clone()).collect();

    created(&api, &link_body(TARGET_URL, json!(at_limit))).await;
    refused(&api, &link_body(TARGET_URL, json!(over_limit)), "a twenty-first rule").await;
}

#[tokio::test]
async fn a_target_that_is_not_a_public_http_url_is_refused() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let api = api(&repo);
    let cases = [
        ("a javascript url as the default", link_body("javascript:alert(1)", json!([]))),
        ("the instance metadata service as the default", link_body("http://169.254.169.254/", json!([]))),
        (
            "a javascript url inside a rule",
            link_body(TARGET_URL, json!([{ "url": "javascript:alert(1)" }])),
        ),
        (
            "the instance metadata service inside a rule",
            link_body(TARGET_URL, json!([{ "url": "http://169.254.169.254/latest/meta-data/" }])),
        ),
        (
            "an unknown platform in a rule",
            link_body(TARGET_URL, json!([{ "platforms": ["windows"], "url": TARGET_URL }])),
        ),
    ];

    for (label, body) in cases {
        refused(&api, &body, label).await;
    }
}

#[tokio::test]
async fn one_ip_creates_thirty_links_a_day_and_no_more() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let api = api(&repo);
    let body = plain_body();

    for _ in 0..MAX_CREATES_PER_IP_PER_DAY {
        created(&api, &body).await;
    }

    let (status, payload) = answered(&api, post(&body)).await;
    assert_eq!(
        (status, payload["error"]["code"].as_str()),
        (429, Some("rate_limited")),
        "the thirty-first create in a day is refused"
    );
}

#[tokio::test]
async fn a_code_that_is_already_taken_is_regenerated_rather_than_overwritten() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let taken = code::generate();
    let free = code::generate();
    let occupant = link("https://example.com/occupant", vec![], "the-occupants-secret", Utc::now());

    assert!(
        repo.put_link_if_absent(&taken, &occupant).await.expect("the seed write"),
        "the seed link is new"
    );

    let mut api = api(&repo);
    api.codes = scripted(&[&taken, &free]);

    let managed = created(&api, &plain_body()).await;
    assert_eq!(managed.code, free, "the taken code lost its conditional put and the next one was used");

    let stored = repo
        .get_link(&taken, Utc::now())
        .await
        .expect("the occupant reads back")
        .expect("the occupant is still there");
    assert_eq!(stored, occupant, "the link already at that code is untouched");
}

#[tokio::test]
async fn stats_come_back_in_date_order_with_their_total() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let api = api(&repo);
    let managed = created(&api, &plain_body()).await;

    let today = Utc::now().date_naive();
    let seeded = [
        stats_day(today, &[("IN|MH|android|mobile", 5)]),
        stats_day(today - Duration::days(2), &[("US|CA|ios|tablet", 2), ("DE|XX|other|desktop", 1)]),
    ];
    for day in &seeded {
        repo.put_stats_day(&managed.code, day).await.expect("the seed write");
    }

    let path = format!("/links/{}/stats?from={}&to={}", managed.code, today - Duration::days(3), today);
    let (status, payload) = answered(&api, get(&path, &managed.secret)).await;
    assert_eq!(status, 200, "{payload}");

    let dates: Vec<&str> = payload["days"]
        .as_array()
        .expect("a day list")
        .iter()
        .map(|day| day["date"].as_str().unwrap_or_default())
        .collect();
    let expected: Vec<String> = seeded.iter().rev().map(|day| day.date.to_string()).collect();

    assert_eq!(dates, expected, "the days arrive oldest first");
    assert_eq!(
        payload["total_90d"],
        json!(seeded.iter().map(|day| day.clicks).sum::<u64>()),
        "the total is the sum of the window"
    );
    assert_eq!(
        payload["days"][0]["seg"],
        json!({ "DE|XX|other|desktop": 1, "US|CA|ios|tablet": 2 }),
        "each day carries its segment counts"
    );
}

#[tokio::test]
async fn a_stats_window_wider_than_the_batch_limit_is_refused() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let api = api(&repo);
    let managed = created(&api, &plain_body()).await;

    let today = Utc::now().date_naive();
    let path = format!("/links/{}/stats?from={}&to={}", managed.code, today - Duration::days(120), today);
    let (status, payload) = answered(&api, get(&path, &managed.secret)).await;

    assert_eq!(
        (status, payload["error"]["code"].as_str()),
        (422, Some("invalid_request")),
        "a window over a hundred days is refused: {payload}"
    );
}

#[tokio::test]
async fn the_origin_verify_header_is_required_only_when_it_is_configured() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let mut api = api(&repo);
    api.origin_verify = Some(secret::hash(ORIGIN_SECRET));

    let with_header = |value: &str| {
        let mut builder = Request::builder().method(Method::POST).uri("/links").header(FORWARDED_FOR_HEADER, CREATOR_IP);
        builder = builder.header(ORIGIN_VERIFY_HEADER, value);
        builder.body(Body::from(plain_body().to_string())).expect("a request")
    };

    let (status, _) = answered(&api, with_header(ORIGIN_SECRET)).await;
    assert_eq!(status, 201, "the matching header passes");

    let (status, payload) = answered(&api, with_header("not-the-origin-header")).await;
    assert_eq!(
        (status, payload["error"]["code"].as_str()),
        (404, Some("not_found")),
        "a wrong header is rejected"
    );

    let (status, payload) = answered(&api, post(&plain_body())).await;
    assert_eq!(
        (status, payload["error"]["code"].as_str()),
        (404, Some("not_found")),
        "a missing header is rejected"
    );

    api.origin_verify = None;
    let (status, _) = answered(&api, post(&plain_body())).await;
    assert_eq!(status, 201, "an unset origin secret skips the check");
}

#[tokio::test]
async fn an_unknown_route_is_not_found() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let api = api(&repo);
    let cases = [
        ("the api prefix cloudfront already stripped", Method::POST, "/api/links"),
        ("a route that does not exist", Method::GET, "/links/x/y/z"),
        ("the root", Method::GET, "/"),
    ];

    for (label, method, path) in cases {
        let (status, payload) = answered(&api, request(method, path, None, Some(&plain_body()))).await;
        assert_eq!(status, 404, "{label}: {payload}");
    }

    let (status, _) = answered(&api, request(Method::POST, "/links/", None, Some(&plain_body()))).await;
    assert_eq!(status, 201, "a trailing slash is tolerated");
}

#[tokio::test]
async fn the_short_url_prefers_the_configured_public_origin() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let mut api = api(&repo);
    api.base_url = Some("https://sh.example/".into());

    let (status, payload) = answered(&api, post(&plain_body())).await;
    let code = payload["code"].as_str().unwrap_or_default();

    assert_eq!(
        (status, payload["short_url"].as_str()),
        (201, Some(format!("https://sh.example/{code}").as_str())),
        "the configured origin wins over the request host and keeps one slash"
    );
}

#[tokio::test]
async fn the_lambda_request_context_supplies_the_creator_ip_when_nothing_is_forwarded() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let api = api(&repo);
    let body = plain_body();

    let direct = || {
        Request::builder()
            .method(Method::POST)
            .uri("/links")
            .body(Body::from(body.to_string()))
            .expect("a request")
            .with_request_context(lambda_http::request::RequestContext::ApiGatewayV2(context()))
    };

    for _ in 0..MAX_CREATES_PER_IP_PER_DAY {
        let (status, payload) = answered(&api, direct()).await;
        assert_eq!(status, 201, "{payload}");
    }

    let (status, payload) = answered(&api, direct()).await;
    assert_eq!(
        (status, payload["error"]["code"].as_str()),
        (429, Some("rate_limited")),
        "the source ip of the function url invocation is counted"
    );
}

fn context() -> lambda_http::aws_lambda_events::apigw::ApiGatewayV2httpRequestContext {
    let mut context = lambda_http::aws_lambda_events::apigw::ApiGatewayV2httpRequestContext::default();
    context.http.source_ip = Some("198.51.100.9".into());
    context
}

#[tokio::test]
async fn a_partial_patch_leaves_the_field_it_omits_alone() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let api = api(&repo);
    let rules = json!([{ "countries": ["IN"], "url": "https://play.google.com/store" }]);
    let managed = created(&api, &link_body(TARGET_URL, rules.clone())).await;

    let (status, payload) = answered(&api, patch(&managed.code, &managed.secret, &json!({ "url": "https://example.com/moved" }))).await;
    assert_eq!(status, 200, "a url-only patch is accepted: {payload}");
    assert_eq!(payload["rules"], rules, "a patch that names no rules leaves the stored rules standing");

    let (_, stored) = viewed(&api, &managed).await;
    assert_eq!(
        (stored["url"].as_str(), &stored["rules"]),
        (Some("https://example.com/moved"), &rules),
        "the stored link kept its rules and took the new url"
    );

    let (status, payload) = answered(&api, patch(&managed.code, &managed.secret, &json!({ "rules": [] }))).await;
    assert_eq!(status, 200, "a rules-only patch is accepted: {payload}");
    assert_eq!(
        (payload["url"].as_str(), payload["rules"].as_array().map(Vec::len)),
        (Some("https://example.com/moved"), Some(0)),
        "a patch that names no url keeps the url and clears the rules it did name"
    );
}

#[tokio::test]
async fn a_patch_naming_nothing_is_refused_rather_than_wiping_the_link() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let api = api(&repo);
    let managed = created(&api, &plain_body()).await;

    let (status, payload) = answered(&api, patch(&managed.code, &managed.secret, &json!({}))).await;
    assert_eq!(
        (status, payload["error"]["code"].as_str()),
        (422, Some("invalid_request")),
        "an empty patch is refused"
    );

    let (_, stored) = viewed(&api, &managed).await;
    assert_eq!(stored["url"].as_str(), Some(TARGET_URL), "the link is untouched by a refused patch");
}

#[tokio::test]
async fn a_flood_of_invalid_bodies_is_still_rate_limited() {
    let Some(repo) = local_repo("mgmt-test").await else {
        return;
    };
    let api = api(&repo);
    let rejected = link_body("javascript:alert(1)", json!([]));

    for attempt in 0..MAX_CREATES_PER_IP_PER_DAY {
        let (status, payload) = answered(&api, post(&rejected)).await;
        assert_eq!(status, 422, "attempt {attempt} is refused for its url, not its rate: {payload}");
    }

    let (status, payload) = answered(&api, post(&rejected)).await;
    assert_eq!(
        (status, payload["error"]["code"].as_str()),
        (429, Some("rate_limited")),
        "the counter advances before validation, so invalid bodies cannot flood for free"
    );

    let (status, payload) = answered(&api, post(&plain_body())).await;
    assert_eq!(status, 429, "a valid body from the same ip is refused too: {payload}");
}
