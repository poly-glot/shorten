use std::borrow::Cow;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use lambda_http::http::Method;
use lambda_http::http::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue, LOCATION};
use lambda_http::{Body, Request, Response};
use shared::code;
use shared::http::unbuildable;
use shared::link::resolve;
use shared::segment::Segment;
use shared::table::DynamoRepo;
use subtle::ConstantTimeEq;

#[cfg(test)]
mod bundle;

pub const FAVICON_PATH: &str = "/favicon.ico";
pub const HOME_PATH: &str = "/";
pub const INDEX_PAGE: &str = include_str!(concat!(env!("OUT_DIR"), "/index.html"));
pub const NOT_FOUND_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Not found</title><p>That short link does not exist.";
pub const ORIGIN_VERIFY_ENV: &str = "ORIGIN_VERIFY";
pub const ORIGIN_VERIFY_HEADER: &str = "x-origin-verify";
pub const PUBLIC_CACHE_CONTROL: &str = "public, max-age=300";
pub const UNAVAILABLE_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Unavailable</title><p>That short link could not be looked up right now.";

const ENCODED_SEPARATOR: &str = "%7C";
const ENCODED_SEPARATOR_LOWER: &str = "%7c";
const FORBIDDEN: u16 = 403;
const FOUND: u16 = 302;
const HTML_CONTENT_TYPE: &str = "text/html; charset=utf-8";
const NOT_FOUND: u16 = 404;
const NO_CONTENT: u16 = 204;
const NO_STORE: &str = "no-store";
const OK: u16 = 200;
const SEGMENT_PREFIX: &str = "s=";

static ORIGIN_SECRET: LazyLock<Option<String>> = LazyLock::new(|| configured_secret(std::env::var(ORIGIN_VERIFY_ENV).ok()));

fn configured_secret(value: Option<String>) -> Option<String> {
    value.filter(|secret| !secret.is_empty())
}

fn origin_verified(expected: Option<&str>, presented: Option<&HeaderValue>) -> bool {
    let Some(expected) = expected else {
        return true;
    };

    presented.is_some_and(|presented| bool::from(presented.as_bytes().ct_eq(expected.as_bytes())))
}

fn segment_value(query: Option<&str>) -> Option<&str> {
    query?.split('&').find_map(|pair| pair.strip_prefix(SEGMENT_PREFIX))
}

fn decoded(raw: &str) -> Cow<'_, str> {
    if !raw.contains('%') {
        return Cow::Borrowed(raw);
    }

    Cow::Owned(raw.replace(ENCODED_SEPARATOR, "|").replace(ENCODED_SEPARATOR_LOWER, "|"))
}

fn page(status: u16, cache_control: &'static str, body: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(CACHE_CONTROL, cache_control)
        .header(CONTENT_TYPE, HTML_CONTENT_TYPE)
        .body(Body::from(body))
        .unwrap_or_else(|_| unbuildable())
}

fn bodiless(status: u16, cache_control: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(CACHE_CONTROL, cache_control)
        .body(Body::Empty)
        .unwrap_or_else(|_| unbuildable())
}

fn found(target: &str) -> Response<Body> {
    Response::builder()
        .status(FOUND)
        .header(CACHE_CONTROL, PUBLIC_CACHE_CONTROL)
        .header(LOCATION, target)
        .body(Body::Empty)
        .unwrap_or_else(|_| unbuildable())
}

fn not_found() -> Response<Body> {
    page(NOT_FOUND, NO_STORE, NOT_FOUND_PAGE)
}

fn logged(outcome: &str, code: &str, segment: &Segment<'_>, response: Response<Body>) -> Response<Body> {
    tracing::info!(code, segment = %segment, outcome, status = response.status().as_u16(), "redirect");
    response
}

pub struct Redirect {
    origin_secret: Option<&'static str>,
    repo: DynamoRepo,
}

impl Redirect {
    pub fn new(repo: DynamoRepo) -> Self {
        Self {
            origin_secret: ORIGIN_SECRET.as_deref(),
            repo,
        }
    }

    pub fn protected_by(repo: DynamoRepo, origin_secret: Option<&'static str>) -> Self {
        Self { origin_secret, repo }
    }

    pub async fn handle(&self, request: Request) -> Response<Body> {
        self.route(&request, Utc::now()).await
    }

    async fn route(&self, request: &Request, now: DateTime<Utc>) -> Response<Body> {
        if !origin_verified(self.origin_secret, request.headers().get(ORIGIN_VERIFY_HEADER)) {
            return logged("origin_unverified", "", &Segment::UNKNOWN, bodiless(FORBIDDEN, NO_STORE));
        }

        match (request.method(), request.uri().path()) {
            (&Method::GET, HOME_PATH) => logged("home", "", &Segment::UNKNOWN, page(OK, PUBLIC_CACHE_CONTROL, INDEX_PAGE)),
            (&Method::GET, FAVICON_PATH) => logged("favicon", "", &Segment::UNKNOWN, bodiless(NO_CONTENT, PUBLIC_CACHE_CONTROL)),
            (&Method::GET, path) => self.redirect(path.trim_start_matches('/'), request.uri().query(), now).await,
            _ => logged("unroutable", "", &Segment::UNKNOWN, not_found()),
        }
    }

    async fn redirect(&self, code: &str, query: Option<&str>, now: DateTime<Utc>) -> Response<Body> {
        if !code::is_valid(code) {
            return logged("invalid_code", "", &Segment::UNKNOWN, not_found());
        }

        let raw = segment_value(query).map(decoded);
        let segment = raw.as_deref().map_or(Segment::UNKNOWN, Segment::parse);

        match self.repo.get_link(code, now).await {
            Err(err) => {
                let status = err.status_code();
                tracing::error!(code, segment = %segment, outcome = "lookup_failed", status, error = %err, "redirect");
                page(status, NO_STORE, UNAVAILABLE_PAGE)
            }
            Ok(None) => logged("absent", code, &segment, not_found()),
            Ok(Some(link)) => logged("matched", code, &segment, found(resolve(&link.rules, &link.url, &segment))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_SECRET: &str = "cloudfront-origin-secret";

    fn header(value: &str) -> HeaderValue {
        HeaderValue::from_str(value).expect("a header value")
    }

    #[test]
    fn an_absent_link_answers_a_minimal_uncached_page() {
        let answer = not_found();
        let cache_control = answer.headers().get(CACHE_CONTROL).and_then(|value| value.to_str().ok());

        assert_eq!((answer.status().as_u16(), cache_control), (404, Some(NO_STORE)));
    }

    #[test]
    fn a_match_answers_a_temporary_redirect_that_caches_for_five_minutes() {
        let answer = found("https://example.com/target");
        let value = |name| answer.headers().get(name).and_then(|value| value.to_str().ok());

        assert_eq!(
            (answer.status().as_u16(), value(LOCATION), value(CACHE_CONTROL)),
            (302, Some("https://example.com/target"), Some(PUBLIC_CACHE_CONTROL)),
            "a permanent redirect would be cached by browsers forever"
        );
    }

    #[test]
    fn an_unset_or_empty_origin_secret_means_no_origin_check_is_configured() {
        let cases = [
            ("the variable is unset", None, None),
            ("the variable is empty", Some(String::new()), None),
            ("a secret is configured", Some(EXPECTED_SECRET.into()), Some(EXPECTED_SECRET.to_string())),
        ];

        for (label, value, expected) in cases {
            assert_eq!(configured_secret(value), expected, "{label}");
        }
    }

    #[test]
    fn a_request_is_verified_only_when_it_presents_the_configured_secret() {
        let cases = [
            ("no secret configured skips the check entirely", None, None, true),
            ("no secret configured ignores whatever was presented", None, Some(header("anything")), true),
            ("the configured secret is accepted", Some(EXPECTED_SECRET), Some(header(EXPECTED_SECRET)), true),
            ("a different secret is refused", Some(EXPECTED_SECRET), Some(header("guessed-secret")), false),
            (
                "a prefix of the secret is refused",
                Some(EXPECTED_SECRET),
                Some(header(&EXPECTED_SECRET[..8])),
                false,
            ),
            ("an empty header is refused", Some(EXPECTED_SECRET), Some(header("")), false),
            ("an absent header is refused", Some(EXPECTED_SECRET), None, false),
        ];

        for (label, expected, presented, allowed) in cases {
            assert_eq!(origin_verified(expected, presented.as_ref()), allowed, "{label}");
        }
    }

    #[test]
    fn the_segment_parameter_is_read_out_of_the_raw_query_string() {
        let cases = [
            ("no query string at all", None, None),
            ("a query string without the parameter", Some("code=aB3xK9mQ2p"), None),
            ("the parameter alone", Some("s=IN|MH|android|mobile"), Some("IN|MH|android|mobile")),
            ("the parameter after another", Some("utm=x&s=DE|BE|ios|desktop"), Some("DE|BE|ios|desktop")),
            ("an empty parameter", Some("s="), Some("")),
            ("a parameter whose name merely starts with s", Some("source=x"), None),
        ];

        for (label, query, expected) in cases {
            assert_eq!(segment_value(query), expected, "{label}");
        }
    }

    #[test]
    fn a_percent_encoded_separator_decodes_back_to_the_canonical_segment() {
        let cases = [
            ("an unencoded segment is borrowed untouched", "IN|MH|android|mobile", "IN|MH|android|mobile"),
            ("an uppercase encoding", "IN%7CMH%7Candroid%7Cmobile", "IN|MH|android|mobile"),
            ("a lowercase encoding", "IN%7cMH%7candroid%7cmobile", "IN|MH|android|mobile"),
        ];

        for (label, raw, expected) in cases {
            assert_eq!(decoded(raw).as_ref(), expected, "{label}");
        }
    }

    #[test]
    fn a_segment_that_never_arrived_reads_as_the_unknown_segment() {
        let cases = [
            ("the edge function never ran", None),
            ("the parameter is empty", Some("s=")),
            ("the parameter is not a segment", Some("s=nonsense")),
            ("the parameter has too few dimensions", Some("s=IN|MH|android")),
        ];

        for (label, query) in cases {
            let raw = segment_value(query).map(decoded);
            assert_eq!(raw.as_deref().map_or(Segment::UNKNOWN, Segment::parse), Segment::UNKNOWN, "{label}");
        }
    }
}
