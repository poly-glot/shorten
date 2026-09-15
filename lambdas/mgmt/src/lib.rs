use std::collections::BTreeMap;

use aws_sdk_dynamodb::Error as DynamoError;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use lambda_http::http::Method;
use lambda_http::request::RequestContext;
use lambda_http::{Body, Request, RequestExt, Response};
use serde::{Deserialize, Serialize};
use shared::code;
use shared::error::AppError;
use shared::http::{answered, body, refused};
use shared::link::{LINK_TTL_DAYS, Link, Rule};
use shared::ratelimit;
use shared::secret;
use shared::stats::StatsDay;
use shared::table::{DynamoRepo, backoff};
use shared::url;

pub const MANAGE_SECRET_HEADER: &str = "x-manage-secret";
pub const ORIGIN_VERIFY_HEADER: &str = "x-origin-verify";
pub const ORIGIN_VERIFY_VAR: &str = "ORIGIN_VERIFY";
pub const PUBLIC_BASE_URL_VAR: &str = "PUBLIC_BASE_URL";

const CREATE_ATTEMPTS: u32 = 8;
const DATE_FORMAT: &str = "%Y-%m-%d";
const FALLBACK_HOST: &str = "localhost";
const FORWARDED_FOR_HEADER: &str = "x-forwarded-for";
const FORWARDED_HOST_HEADER: &str = "x-forwarded-host";
const FORWARDED_PROTO_HEADER: &str = "x-forwarded-proto";
const HOST_HEADER: &str = "host";
const LOCAL_HOSTS: [&str; 2] = ["127.0.0.1", "localhost"];
const UNKNOWN_IP: &str = "unknown";

pub type CodeSource = Box<dyn Fn() -> String + Send + Sync>;

#[derive(Debug, Deserialize)]
pub struct LinkInput {
    #[serde(default)]
    pub rules: Vec<Rule>,
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct LinkPatch {
    pub rules: Option<Vec<Rule>>,
    pub url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Created {
    pub code: String,
    pub manage_secret: String,
    pub short_url: String,
}

#[derive(Debug, Serialize)]
pub struct LinkView {
    pub rules: Vec<Rule>,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct DayView {
    pub clicks: u64,
    pub date: NaiveDate,
    pub seg: BTreeMap<String, u64>,
}

#[derive(Debug, Serialize)]
pub struct StatsView {
    pub days: Vec<DayView>,
    pub total_90d: u64,
}

impl From<StatsDay> for DayView {
    fn from(day: StatsDay) -> Self {
        Self {
            clicks: day.clicks,
            date: day.date,
            seg: day.seg,
        }
    }
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn forwarded_client(request: &Request) -> Option<&str> {
    header(request, FORWARDED_FOR_HEADER)?
        .split(',')
        .next()
        .map(str::trim)
        .filter(|address| !address.is_empty())
}

fn context_client(request: &Request) -> Option<&str> {
    let Some(RequestContext::ApiGatewayV2(context)) = request.request_context_ref() else {
        return None;
    };

    context.http.source_ip.as_deref().filter(|address| !address.is_empty())
}

fn client_ip(request: &Request) -> &str {
    forwarded_client(request).or_else(|| context_client(request)).unwrap_or(UNKNOWN_IP)
}

fn origin(request: &Request) -> String {
    let host = header(request, FORWARDED_HOST_HEADER)
        .or_else(|| header(request, HOST_HEADER))
        .unwrap_or(FALLBACK_HOST);
    let scheme = header(request, FORWARDED_PROTO_HEADER).unwrap_or(match LOCAL_HOSTS.iter().any(|local| host.starts_with(local)) {
        true => "http",
        false => "https",
    });

    format!("{scheme}://{host}")
}

fn param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
        .filter(|value| !value.is_empty())
}

fn day(query: &str, name: &str) -> Result<NaiveDate, AppError> {
    let Some(raw) = param(query, name) else {
        return Err(AppError::BadRequest(format!("{name} is required as YYYY-MM-DD")));
    };

    NaiveDate::parse_from_str(raw, DATE_FORMAT).map_err(|_| AppError::BadRequest(format!("{name} is not a YYYY-MM-DD date")))
}

fn is_transient(err: &AppError) -> bool {
    let AppError::Dynamo(err) = err else {
        return false;
    };

    matches!(
        **err,
        DynamoError::InternalServerError(_)
            | DynamoError::LimitExceededException(_)
            | DynamoError::ProvisionedThroughputExceededException(_)
            | DynamoError::RequestLimitExceeded(_)
            | DynamoError::ThrottlingException(_)
    )
}

fn outcome(status: u16) -> &'static str {
    match status {
        200..=299 => "ok",
        400..=499 => "rejected",
        _ => "failed",
    }
}

pub struct Mgmt {
    pub base_url: Option<String>,
    pub codes: CodeSource,
    pub origin_verify: Option<String>,
    pub repo: DynamoRepo,
}

impl Mgmt {
    pub fn new(repo: DynamoRepo) -> Self {
        Self {
            base_url: env_value(PUBLIC_BASE_URL_VAR),
            codes: Box::new(code::generate),
            origin_verify: env_value(ORIGIN_VERIFY_VAR).map(|value| secret::hash(&value)),
            repo,
        }
    }

    pub async fn handle(&self, request: Request) -> Response<Body> {
        if let Err(err) = self.check_origin(&request) {
            return refused(err);
        }

        let path = request.uri().path().trim_matches('/').to_string();
        let segments: Vec<&str> = path.split('/').collect();
        let code = segments.get(1).copied().unwrap_or_default();

        let response = self.route(&request, &segments, Utc::now()).await;
        let status = response.status().as_u16();

        tracing::info!(code, outcome = outcome(status), status, "mgmt request");

        response
    }

    fn check_origin(&self, request: &Request) -> Result<(), AppError> {
        let Some(expected_hash) = &self.origin_verify else {
            return Ok(());
        };
        if secret::verify(header(request, ORIGIN_VERIFY_HEADER).unwrap_or_default(), expected_hash) {
            return Ok(());
        }

        Err(AppError::NotFound("route".into()))
    }

    async fn route(&self, request: &Request, segments: &[&str], now: DateTime<Utc>) -> Response<Body> {
        match (request.method(), segments) {
            (&Method::POST, ["links"]) => answered(201, self.create(request, now).await),
            (&Method::GET, ["links", code]) => answered(200, self.read(code, request, now).await),
            (&Method::PATCH, ["links", code]) => answered(200, self.update(code, request, now).await),
            (&Method::DELETE, ["links", code]) => answered(204, self.remove(code, request, now).await),
            (&Method::GET, ["links", code, "stats"]) => answered(200, self.stats(code, request, now).await),
            _ => refused(AppError::NotFound("route".into())),
        }
    }

    async fn authorized(&self, code: &str, request: &Request, now: DateTime<Utc>) -> Result<Link, AppError> {
        if !code::is_valid(code) {
            return Err(AppError::NotFound("link".into()));
        }

        let Some(link) = self.repo.get_link(code, now).await? else {
            return Err(AppError::NotFound(format!("link {code}")));
        };

        if !secret::verify(header(request, MANAGE_SECRET_HEADER).unwrap_or_default(), &link.secret_hash) {
            return Err(AppError::Unauthorized);
        }

        Ok(link)
    }

    async fn insert(&self, link: &Link) -> Result<String, AppError> {
        for attempt in 0..CREATE_ATTEMPTS {
            let code = (self.codes)();

            match self.repo.put_link_if_absent(&code, link).await {
                Ok(true) => return Ok(code),
                Ok(false) => continue,
                Err(err) if is_transient(&err) => backoff(attempt).await,
                Err(err) => return Err(err),
            }
        }

        Err(AppError::Internal(format!("no free code after {CREATE_ATTEMPTS} attempts")))
    }

    async fn create(&self, request: &Request, now: DateTime<Utc>) -> Result<Created, AppError> {
        if ratelimit::is_throttled(self.repo.count_create(client_ip(request), now).await?) {
            return Err(AppError::RateLimited);
        }

        let input: LinkInput = body(request)?;
        url::validate(&input.url)?;
        Rule::validate(&input.rules)?;

        let manage_secret = secret::generate();
        let link = Link {
            clicks_total: 0,
            created_at: now.timestamp(),
            last_rollup: None,
            rules: input.rules,
            secret_hash: secret::hash(&manage_secret),
            ttl: (now + Duration::days(LINK_TTL_DAYS)).timestamp(),
            url: input.url,
        };

        let code = self.insert(&link).await?;
        let base = self.base_url.clone().unwrap_or_else(|| origin(request));

        tracing::info!(code, outcome = "created", "link created");

        Ok(Created {
            short_url: format!("{}/{code}", base.trim_end_matches('/')),
            code,
            manage_secret,
        })
    }

    async fn read(&self, code: &str, request: &Request, now: DateTime<Utc>) -> Result<LinkView, AppError> {
        let link = self.authorized(code, request, now).await?;

        Ok(LinkView {
            rules: link.rules,
            url: link.url,
        })
    }

    async fn update(&self, code: &str, request: &Request, now: DateTime<Utc>) -> Result<LinkView, AppError> {
        let link = self.authorized(code, request, now).await?;

        let patch: LinkPatch = body(request)?;
        if let Some(target) = &patch.url {
            url::validate(target)?;
        }
        if let Some(rules) = &patch.rules {
            Rule::validate(rules)?;
        }

        if !self.repo.update_link(code, patch.url.as_deref(), patch.rules.as_deref()).await? {
            return Err(AppError::NotFound(format!("link {code}")));
        }

        Ok(LinkView {
            rules: patch.rules.unwrap_or(link.rules),
            url: patch.url.unwrap_or(link.url),
        })
    }

    async fn remove(&self, code: &str, request: &Request, now: DateTime<Utc>) -> Result<(), AppError> {
        self.authorized(code, request, now).await?;

        if !self.repo.delete_link(code).await? {
            return Err(AppError::NotFound(format!("link {code}")));
        }

        Ok(())
    }

    async fn stats(&self, code: &str, request: &Request, now: DateTime<Utc>) -> Result<StatsView, AppError> {
        self.authorized(code, request, now).await?;

        let query = request.uri().query().unwrap_or_default();
        let days = self.repo.get_stats_days(code, day(query, "from")?, day(query, "to")?).await?;

        Ok(StatsView {
            total_90d: days.iter().map(|day| day.clicks).sum(),
            days: days.into_iter().map(DayView::from).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built(headers: &[(&str, &str)]) -> Request {
        let mut builder = lambda_http::http::Request::builder().uri("https://short.example/links");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::Empty).expect("a request")
    }

    #[test]
    fn the_client_is_the_leftmost_forwarded_address_before_the_invoking_peer() {
        let cases = [
            (
                "a chain added by cloudfront",
                &[("x-forwarded-for", "203.0.113.7, 130.176.1.1")][..],
                "203.0.113.7",
            ),
            ("a single forwarded address", &[("x-forwarded-for", "203.0.113.7")][..], "203.0.113.7"),
            ("a padded chain", &[("x-forwarded-for", "  203.0.113.7 , 130.176.1.1")][..], "203.0.113.7"),
            ("no forwarded header and no lambda context", &[][..], UNKNOWN_IP),
            ("an empty forwarded header", &[("x-forwarded-for", "")][..], UNKNOWN_IP),
        ];

        for (label, headers, expected) in cases {
            assert_eq!(client_ip(&built(headers)), expected, "{label}");
        }
    }

    #[test]
    fn the_origin_comes_from_the_forwarded_host_then_the_host_header() {
        let cases = [
            (
                "a cloudfront forwarded host",
                &[("x-forwarded-host", "short.example"), ("host", "abc.lambda-url.aws")][..],
                "https://short.example",
            ),
            ("a plain host header", &[("host", "short.example")][..], "https://short.example"),
            (
                "an explicit forwarded scheme",
                &[("host", "short.example"), ("x-forwarded-proto", "http")][..],
                "http://short.example",
            ),
            ("a local host keeps http", &[("host", "localhost:3000")][..], "http://localhost:3000"),
            ("a local address keeps http", &[("host", "127.0.0.1:9000")][..], "http://127.0.0.1:9000"),
            ("no host at all", &[][..], "http://localhost"),
        ];

        for (label, headers, expected) in cases {
            assert_eq!(origin(&built(headers)), expected, "{label}");
        }
    }

    #[test]
    fn a_stats_window_reads_only_its_own_named_parameters() {
        let cases = [
            ("both parameters", "from=2026-01-01&to=2026-01-10", Some("2026-01-01"), Some("2026-01-10")),
            ("reversed order", "to=2026-01-10&from=2026-01-01", Some("2026-01-01"), Some("2026-01-10")),
            (
                "a parameter that merely ends in the name",
                "xfrom=2026-01-01&to=2026-01-10",
                None,
                Some("2026-01-10"),
            ),
            ("an empty value", "from=&to=2026-01-10", None, Some("2026-01-10")),
            ("no query at all", "", None, None),
        ];

        for (label, query, from, to) in cases {
            assert_eq!((param(query, "from"), param(query, "to")), (from, to), "{label}");
        }
    }

    #[test]
    fn a_missing_or_malformed_window_bound_is_a_bad_request() {
        let cases = [
            ("a missing bound", "to=2026-01-10"),
            ("a bound that is not a date", "from=yesterday&to=2026-01-10"),
            ("a bound with a time", "from=2026-01-01T00:00:00Z&to=2026-01-10"),
        ];

        for (label, query) in cases {
            assert!(matches!(day(query, "from"), Err(AppError::BadRequest(_))), "{label}");
        }

        let parsed = day("from=2026-01-01&to=2026-01-10", "from").expect("a well-formed bound parses");
        assert_eq!(parsed, NaiveDate::from_ymd_opt(2026, 1, 1).expect("a real date"), "a well-formed bound");
    }

    #[test]
    fn only_a_throttle_or_a_server_fault_is_worth_retrying() {
        let throttled = AppError::Dynamo(Box::new(DynamoError::ThrottlingException(
            aws_sdk_dynamodb::types::error::ThrottlingException::builder().build(),
        )));
        let absent_table = AppError::Dynamo(Box::new(DynamoError::ResourceNotFoundException(
            aws_sdk_dynamodb::types::error::ResourceNotFoundException::builder().build(),
        )));

        let cases = [
            ("a throttled write", throttled, true),
            ("a table that does not exist", absent_table, false),
            ("a validation failure", AppError::BadRequest("x".into()), false),
        ];

        for (label, error, expected) in cases {
            assert_eq!(is_transient(&error), expected, "{label}");
        }
    }

    #[test]
    fn the_outcome_field_names_the_status_class() {
        let cases = [
            ("a created link", 201, "ok"),
            ("a deleted link", 204, "ok"),
            ("a wrong secret", 401, "rejected"),
            ("a rate limited creator", 429, "rejected"),
            ("a table fault", 500, "failed"),
        ];

        for (label, status, expected) in cases {
            assert_eq!(outcome(status), expected, "{label}");
        }
    }
}
