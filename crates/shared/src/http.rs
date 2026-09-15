use lambda_http::http::StatusCode;
use lambda_http::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use lambda_http::{Body, Request, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::AppError;

const CACHE_CONTROL_NO_STORE: &str = "no-store";
const CONTENT_TYPE_JSON: &str = "application/json";
const EMPTY_JSON_BODY: &str = "{}";
const NO_CONTENT: u16 = 204;
const SERVER_ERROR_THRESHOLD: u16 = 500;

#[derive(Debug, Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

pub fn body<T: DeserializeOwned>(request: &Request) -> Result<T, AppError> {
    serde_json::from_slice(request.body().as_ref()).map_err(|err| AppError::BadRequest(format!("invalid body: {err}")))
}

fn error_body(err: &AppError) -> ErrorBody {
    ErrorBody {
        error: ErrorDetail {
            code: err.code(),
            message: err.public_message(),
        },
    }
}

fn json_string<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| EMPTY_JSON_BODY.into())
}

fn response(status: u16, json: Option<String>) -> Response<Body> {
    let has_body = json.is_some();
    let body = match json {
        None => Body::Empty,
        Some(json) => Body::from(json),
    };

    let mut builder = Response::builder().status(status).header(CACHE_CONTROL, CACHE_CONTROL_NO_STORE);
    if has_body {
        builder = builder.header(CONTENT_TYPE, CONTENT_TYPE_JSON);
    }

    builder.body(body).unwrap_or_else(|_| unbuildable())
}

pub fn unbuildable() -> Response<Body> {
    let mut response = Response::new(Body::Empty);
    *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;

    response
}

pub fn refused(err: AppError) -> Response<Body> {
    let status = err.status_code();
    if status >= SERVER_ERROR_THRESHOLD {
        tracing::error!(status, error = %err, "request failed");
    } else {
        tracing::warn!(status, error = %err, "request rejected");
    }

    response(status, Some(json_string(&error_body(&err))))
}

pub fn answered<T: Serialize>(status: u16, result: Result<T, AppError>) -> Response<Body> {
    let value = match result {
        Ok(value) => value,
        Err(err) => return refused(err),
    };

    if status == NO_CONTENT {
        return response(status, None);
    }
    response(status, Some(json_string(&value)))
}

#[cfg(test)]
mod tests {
    use lambda_http::http::header::HeaderName;
    use serde_json::{Value, json};

    use super::*;

    fn header_value(answer: &Response<Body>, name: HeaderName) -> Option<&str> {
        answer.headers().get(name).and_then(|value| value.to_str().ok())
    }

    #[test]
    fn body_rejects_a_payload_that_is_not_valid_json() {
        let request = Request::new(Body::from("not json".to_string()));

        let message = match body::<Value>(&request) {
            Err(AppError::BadRequest(message)) => message,
            other => panic!("expected a BadRequest, got {other:?}"),
        };
        assert!(message.starts_with("invalid body: "), "the message names the parse failure: {message}");
    }

    #[test]
    fn error_body_nests_the_code_and_the_public_message() {
        let cases = [
            (
                "a validation failure",
                AppError::BadRequest("url rejected: no host".into()),
                ("invalid_request", "url rejected: no host"),
            ),
            ("a wrong secret", AppError::Unauthorized, ("unauthorized", "invalid management secret")),
            ("a throttled creator", AppError::RateLimited, ("rate_limited", "too many requests")),
            ("an absent link", AppError::NotFound("link aB3xK9mQ2p".into()), ("not_found", "link aB3xK9mQ2p")),
            (
                "an internal fault never leaks its detail",
                AppError::Internal("table arn".into()),
                ("internal_error", "internal error"),
            ),
        ];

        for (label, error, expected) in cases {
            let body = error_body(&error);
            assert_eq!((body.error.code, body.error.message.as_str()), expected, "{label}");
        }
    }

    #[test]
    fn an_error_answers_with_its_own_status_not_the_success_status() {
        let answer = answered(201, Err::<(), _>(AppError::Unauthorized));
        assert_eq!(answer.status().as_u16(), 401, "the error status wins");
    }

    #[test]
    fn an_error_answer_carries_its_json_envelope_and_headers() {
        let answer = answered::<()>(200, Err(AppError::Unauthorized));
        let Ok(text) = std::str::from_utf8(answer.body()) else {
            panic!("a json body is valid utf8");
        };

        assert_eq!(
            (
                answer.status().as_u16(),
                text,
                header_value(&answer, CACHE_CONTROL),
                header_value(&answer, CONTENT_TYPE),
            ),
            (
                401,
                r#"{"error":{"code":"unauthorized","message":"invalid management secret"}}"#,
                Some(CACHE_CONTROL_NO_STORE),
                Some(CONTENT_TYPE_JSON),
            ),
            "an error answer uses its own status, not the caller's, plus the frozen envelope and headers"
        );
    }

    #[test]
    fn a_no_content_answer_carries_no_body() {
        let answer = answered(204, Ok(()));

        assert_eq!(
            (
                answer.status().as_u16(),
                answer.body().is_empty(),
                header_value(&answer, CACHE_CONTROL),
                header_value(&answer, CONTENT_TYPE)
            ),
            (204, true, Some(CACHE_CONTROL_NO_STORE), None),
            "a no-content answer carries no body and no content-type"
        );
    }

    #[test]
    fn a_body_is_attached_at_every_status_except_no_content_exactly() {
        let cases = [
            ("one below the no-content status", 203, true),
            ("exactly the no-content status", 204, false),
            ("one above the no-content status", 205, true),
        ];

        for (label, status, expects_body) in cases {
            let answer = answered(status, Ok(json!({ "ok": true })));
            assert_eq!(!answer.body().is_empty(), expects_body, "{label}");
        }
    }

    #[test]
    fn an_unrepresentable_status_answers_as_an_internal_error_never_an_empty_success() {
        let answer = answered(1000, Ok(json!({ "ok": true })));

        assert_eq!(
            (answer.status().as_u16(), answer.body().is_empty()),
            (500, true),
            "the builder's failure must not read as a 200"
        );
    }

    #[test]
    fn a_created_answer_carries_its_json_body() {
        let answer = answered(201, Ok(json!({ "code": "aB3xK9mQ2p" })));
        let Ok(text) = std::str::from_utf8(answer.body()) else {
            panic!("a json body is valid utf8");
        };

        assert_eq!(
            (
                answer.status().as_u16(),
                text,
                header_value(&answer, CACHE_CONTROL),
                header_value(&answer, CONTENT_TYPE)
            ),
            (201, r#"{"code":"aB3xK9mQ2p"}"#, Some(CACHE_CONTROL_NO_STORE), Some(CONTENT_TYPE_JSON)),
            "a json answer carries its body, its no-store cache header and its content type"
        );
    }
}
