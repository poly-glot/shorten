use aws_sdk_dynamodb::error::{BuildError, SdkError};
use thiserror::Error;

pub(crate) const CONDITIONAL_CHECK_FAILED: &str = "ConditionalCheckFailed";

const INTERNAL_ERROR_MESSAGE: &str = "internal error";

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("DynamoDB error: {0}")]
    Dynamo(Box<aws_sdk_dynamodb::Error>),
    #[error("Internal error: {0}")]
    Internal(String),
    #[error("{0}")]
    NotFound(String),
    #[error("too many requests")]
    RateLimited,
    #[error("invalid management secret")]
    Unauthorized,
}

struct Wire {
    code: &'static str,
    status: u16,
}

impl AppError {
    fn wire(&self) -> Wire {
        match self {
            Self::BadRequest(_) => Wire {
                code: "invalid_request",
                status: 422,
            },
            Self::Dynamo(_) | Self::Internal(_) => Wire {
                code: "internal_error",
                status: 500,
            },
            Self::NotFound(_) => Wire {
                code: "not_found",
                status: 404,
            },
            Self::RateLimited => Wire {
                code: "rate_limited",
                status: 429,
            },
            Self::Unauthorized => Wire {
                code: "unauthorized",
                status: 401,
            },
        }
    }

    pub fn status_code(&self) -> u16 {
        self.wire().status
    }

    pub fn code(&self) -> &'static str {
        self.wire().code
    }

    pub fn public_message(&self) -> String {
        match self {
            Self::Dynamo(_) | Self::Internal(_) => INTERNAL_ERROR_MESSAGE.to_string(),
            other => other.to_string(),
        }
    }

    pub fn is_condition_failed(&self) -> bool {
        let Self::Dynamo(err) = self else {
            return false;
        };

        match &**err {
            aws_sdk_dynamodb::Error::ConditionalCheckFailedException(_) => true,
            aws_sdk_dynamodb::Error::TransactionCanceledException(cancelled) => cancelled
                .cancellation_reasons()
                .iter()
                .any(|reason| reason.code() == Some(CONDITIONAL_CHECK_FAILED)),
            _ => false,
        }
    }
}

impl From<aws_sdk_dynamodb::Error> for AppError {
    fn from(err: aws_sdk_dynamodb::Error) -> Self {
        Self::Dynamo(Box::new(err))
    }
}

impl<E, R> From<SdkError<E, R>> for AppError
where
    aws_sdk_dynamodb::Error: From<SdkError<E, R>>,
{
    fn from(err: SdkError<E, R>) -> Self {
        Self::Dynamo(Box::new(err.into()))
    }
}

fn internal(err: impl std::fmt::Display) -> AppError {
    AppError::Internal(err.to_string())
}

impl From<BuildError> for AppError {
    fn from(err: BuildError) -> Self {
        internal(err)
    }
}

impl From<serde_dynamo::Error> for AppError {
    fn from(err: serde_dynamo::Error) -> Self {
        internal(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_dynamodb::types::error::{ConditionalCheckFailedException, ResourceNotFoundException, ThrottlingException, TransactionCanceledException};
    use aws_sdk_dynamodb::types::{CancellationReason, KeySchemaElement};
    use std::collections::HashMap;

    fn condition_failed() -> aws_sdk_dynamodb::Error {
        aws_sdk_dynamodb::Error::ConditionalCheckFailedException(ConditionalCheckFailedException::builder().build())
    }

    fn dynamo(err: aws_sdk_dynamodb::Error) -> AppError {
        AppError::from(err)
    }

    fn not_found() -> aws_sdk_dynamodb::Error {
        aws_sdk_dynamodb::Error::ResourceNotFoundException(ResourceNotFoundException::builder().build())
    }

    fn throttled() -> aws_sdk_dynamodb::Error {
        aws_sdk_dynamodb::Error::ThrottlingException(ThrottlingException::builder().build())
    }

    fn transaction_cancelled(codes: [&str; 2]) -> aws_sdk_dynamodb::Error {
        let mut builder = TransactionCanceledException::builder();
        for code in codes {
            builder = builder.cancellation_reasons(CancellationReason::builder().code(code).build());
        }
        aws_sdk_dynamodb::Error::TransactionCanceledException(builder.build())
    }

    #[test]
    fn every_variant_maps_to_its_frozen_wire_shape() {
        let cases = [
            (
                "a validation failure",
                AppError::BadRequest("url is not http".into()),
                422,
                "invalid_request",
                "url is not http",
            ),
            ("a table fault", dynamo(not_found()), 500, "internal_error", "internal error"),
            (
                "an internal fault",
                AppError::Internal("table arn".into()),
                500,
                "internal_error",
                "internal error",
            ),
            (
                "an absent row",
                AppError::NotFound("link aB3xK9mQ2p".into()),
                404,
                "not_found",
                "link aB3xK9mQ2p",
            ),
            ("a throttled creator", AppError::RateLimited, 429, "rate_limited", "too many requests"),
            ("a wrong secret", AppError::Unauthorized, 401, "unauthorized", "invalid management secret"),
        ];

        for (label, error, status, code, message) in cases {
            assert_eq!(
                (error.status_code(), error.code(), error.public_message().as_str()),
                (status, code, message),
                "{label}"
            );
        }
    }

    #[test]
    fn only_a_failed_condition_counts_as_condition_failed() {
        let cases = [
            ("a conditional put that lost its condition", dynamo(condition_failed()), true),
            (
                "a transaction cancelled because one item failed its condition",
                dynamo(transaction_cancelled(["ConditionalCheckFailed", "None"])),
                true,
            ),
            ("a throttled write", dynamo(throttled()), false),
            (
                "a transaction cancelled for a reason that is not a lost condition",
                dynamo(transaction_cancelled(["ThrottlingError", "None"])),
                false,
            ),
            ("a missing table", dynamo(not_found()), false),
            ("an error that never reached DynamoDB", AppError::BadRequest("x".into()), false),
        ];

        for (label, error, expected) in cases {
            assert_eq!(error.is_condition_failed(), expected, "{label}");
        }
    }

    #[test]
    fn a_builder_failure_becomes_an_internal_error() {
        let Err(build_err) = KeySchemaElement::builder().build() else {
            panic!("an empty key schema element is missing its required fields");
        };

        assert!(matches!(AppError::from(build_err), AppError::Internal(_)), "builder errors are internal");
    }

    #[test]
    fn a_deserialisation_failure_becomes_an_internal_error() {
        let mut item: HashMap<String, aws_sdk_dynamodb::types::AttributeValue> = HashMap::new();
        item.insert("value".into(), aws_sdk_dynamodb::types::AttributeValue::S("not a number".into()));

        let Err(serde_err) = serde_dynamo::aws_sdk_dynamodb_1::from_item::<u64>(item) else {
            panic!("a string cannot deserialise into a u64");
        };

        assert!(matches!(AppError::from(serde_err), AppError::Internal(_)), "serde errors are internal");
    }
}
