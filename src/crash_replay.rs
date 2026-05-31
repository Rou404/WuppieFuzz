#![allow(dead_code)]

use crate::{
    crash_identity::{CrashIdentity, CrashKind, ObservedExitKind, ResponseClass},
    openapi::validate_response::{Response, ValidationError, ValidationErrorDiscriminants},
};
use reqwest::StatusCode;
use strum::IntoDiscriminant;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedCrash {
    pub identity: CrashIdentity,
    pub crashing_request_index: usize,
}
fn coarse_response_class(response: &Response) -> ResponseClass {
    if response.content_length() == 0 {
        return ResponseClass::Empty;
    }

    if response.json::<serde_json::Value>().is_ok() {
        return ResponseClass::Json;
    }

    match response.text() {
        Ok(text) => {
            let trimmed = text.trim_start();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                ResponseClass::InvalidJson
            } else if looks_like_html(&text) {
                ResponseClass::Html
            } else {
                ResponseClass::Plaintext
            }
        }
        Err(_) => ResponseClass::BinaryOrUnknown,
    }
}

fn looks_like_html(text: &str) -> bool {
    let trimmed = text.trim_start().to_ascii_lowercase();
    trimmed.starts_with("<!doctype html>")
        || trimmed.starts_with("<html")
        || trimmed.contains("<body")
}

fn response_crash_kind(
    status: StatusCode,
    validation_error: Option<&ValidationError>,
) -> CrashKind {
    if status.is_server_error() {
        return CrashKind::Http5xx;
    }
    validation_error
        .map(|err| CrashKind::Validation(err.discriminant()))
        .unwrap_or(CrashKind::HttpResponseCrash)
}

fn transport_response_class(error: &reqwest::Error) -> ResponseClass {
    if error.is_timeout() {
        ResponseClass::TransportTimeout
    } else if error.is_connect() {
        ResponseClass::TransportConnectionError
    } else if error.is_decode() {
        ResponseClass::TransportDecodeError
    } else {
        ResponseClass::TransportUnknownError
    }
}

fn transport_crash_kind(error: &reqwest::Error) -> CrashKind {
    if error.is_timeout() {
        CrashKind::TransportTimeout
    } else if error.is_connect() {
        CrashKind::TransportConnectionError
    } else if error.is_decode() {
        CrashKind::TransportDecodeError
    } else {
        CrashKind::TransportUnknownError
    }
}

fn observed_response_identity(
    status: StatusCode,
    validation_error: Option<&ValidationError>,
    endpoint: Option<String>,
    response_class: ResponseClass,
) -> CrashIdentity {
    CrashIdentity {
        exit_kind: ObservedExitKind::Crash,
        crash_kind: response_crash_kind(status, validation_error),
        http_status: Some(status.as_u16()),
        validation_error_discriminant: validation_error
            .map(|err| format!("{:?}", err.discriminant())),
        endpoint,
        response_class,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Method;

    fn validation_error() -> ValidationError {
        ValidationError::OperationNotInSpec {
            path: "/items".into(),
            method: Method::Get,
        }
    }

    #[test]
    fn response_crash_kind_prefers_http_5xx() {
        let validation_error = validation_error();

        assert_eq!(
            response_crash_kind(StatusCode::INTERNAL_SERVER_ERROR, Some(&validation_error),),
            CrashKind::Http5xx
        );
    }

    #[test]
    fn response_crash_kind_uses_validation_discriminant_for_non_5xx_crash() {
        let validation_error = validation_error();

        assert_eq!(
            response_crash_kind(StatusCode::BAD_REQUEST, Some(&validation_error),),
            CrashKind::Validation(ValidationErrorDiscriminants::OperationNotInSpec)
        );
    }

    #[test]
    fn response_crash_kind_falls_back_for_unclassified_response_crash() {
        assert_eq!(
            response_crash_kind(StatusCode::BAD_REQUEST, None),
            CrashKind::HttpResponseCrash
        );
    }

    #[test]
    fn looks_like_html_recognizes_common_html_bodies() {
        assert!(looks_like_html("<!doctype html><html></html>"));
        assert!(looks_like_html("  <html><body>error</body></html>"));
        assert!(looks_like_html("prefix <body>error</body>"));
        assert!(!looks_like_html("{\"error\":\"not html\"}"));
    }

    #[test]
    fn observed_response_identity_contains_core_signals() {
        let identity = observed_response_identity(
            StatusCode::INTERNAL_SERVER_ERROR,
            None,
            Some("GET /items/{id}".into()),
            ResponseClass::Json,
        );

        assert_eq!(identity.exit_kind, ObservedExitKind::Crash);
        assert_eq!(identity.crash_kind, CrashKind::Http5xx);
        assert_eq!(identity.http_status, Some(500));
        assert_eq!(identity.validation_error_discriminant, None);
        assert_eq!(identity.endpoint.as_deref(), Some("GET /items/{id}"));
        assert_eq!(identity.response_class, ResponseClass::Json);
    }

    #[test]
    fn observed_response_identity_records_validation_discriminant() {
        let validation_error = validation_error();

        let identity = observed_response_identity(
            StatusCode::BAD_REQUEST,
            Some(&validation_error),
            Some("GET /items/{id}".into()),
            ResponseClass::Json,
        );

        assert_eq!(
            identity.validation_error_discriminant.as_deref(),
            Some("OperationNotInSpec")
        );
        assert_eq!(
            identity.crash_kind,
            CrashKind::Validation(ValidationErrorDiscriminants::OperationNotInSpec)
        );
    }
}
