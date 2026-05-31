#![allow(dead_code)]

use crate::{
    authentication::{Authentication, build_http_client},
    configuration::Configuration,
    crash_identity::{CrashIdentity, CrashKind, ObservedExitKind, ResponseClass},
    executor::process_response,
    input::{OpenApiInput, OpenApiRequest},
    openapi::{
        build_request::build_request_from_input,
        spec::Spec,
        validate_response::{Response, ValidationError, ValidationErrorDiscriminants},
    },
    parameter_feedback::ParameterFeedback,
};
use std::time::Duration;

use anyhow::Result;
use libafl::executors::ExitKind;
use libafl_bolts::HasLen;
use reqwest::StatusCode;
use strum::IntoDiscriminant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedCrash {
    pub identity: CrashIdentity,
    pub crashing_request_index: usize,
}
fn coarse_response_class(response: &Response) -> ResponseClass {
    // This uses WuppieFuzz's buffered Response wrapper, so reading JSON/text here
    // does not consume the response for later validation or status access.
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

enum ReplayStep {
    Continue,
    Stop,
    Crash(ObservedCrash),
}

enum BuiltReplayRequest {
    Request(reqwest::blocking::Request),
    // WuppieFuzz could not turn this input into a request; keep replaying the sequence.
    Skip,
    // Reqwest rejected the already-built request; mirror reproducer behavior and stop replay.
    Stop,
}

pub fn replay_input(
    input: &OpenApiInput,
    api: &Spec,
    config: &Configuration,
) -> Result<Option<ObservedCrash>> {
    let (mut authentication, cookie_store, client) = build_http_client(api)?;

    replay_input_with_client(
        input,
        api,
        config.request_timeout,
        &config.crash_criteria,
        &client,
        &mut authentication,
        &cookie_store,
    )
}

fn replay_input_with_client(
    input: &OpenApiInput,
    api: &Spec,
    request_timeout_ms: u64,
    crash_criteria: &[ValidationErrorDiscriminants],
    client: &reqwest::blocking::Client,
    authentication: &mut Authentication,
    cookie_store: &std::sync::Arc<reqwest_cookie_store::CookieStoreMutex>,
) -> Result<Option<ObservedCrash>> {
    let mut parameter_feedback = ParameterFeedback::new(input.len());
    for (request_index, request) in input.0.iter().enumerate() {
        match replay_request(
            request_index,
            request,
            api,
            request_timeout_ms,
            crash_criteria,
            client,
            authentication,
            cookie_store,
            &mut parameter_feedback,
        )? {
            ReplayStep::Continue => {}
            ReplayStep::Stop => break,
            ReplayStep::Crash(crash) => return Ok(Some(crash)),
        }
    }

    Ok(None)
}
fn replay_request(
    request_index: usize,
    request: &OpenApiRequest,
    api: &Spec,
    request_timeout_ms: u64,
    crash_criteria: &[ValidationErrorDiscriminants],
    client: &reqwest::blocking::Client,
    authentication: &mut Authentication,
    cookie_store: &std::sync::Arc<reqwest_cookie_store::CookieStoreMutex>,
    parameter_feedback: &mut ParameterFeedback,
) -> Result<ReplayStep> {
    let mut request = request.clone();

    if let Err(error) = request.resolve_parameter_references(parameter_feedback) {
        log::debug!(
            "Cannot instantiate request while replaying crash: missing backreference: {error}"
        );
        return Ok(ReplayStep::Stop);
    }

    parameter_feedback.process_request(request_index, &request);

    let request_built = match build_replay_request(
        client,
        authentication,
        cookie_store,
        api,
        request_timeout_ms,
        &request,
    )? {
        BuiltReplayRequest::Request(request) => request,
        BuiltReplayRequest::Skip => return Ok(ReplayStep::Continue),
        BuiltReplayRequest::Stop => return Ok(ReplayStep::Stop),
    };

    match client.execute(request_built) {
        Ok(response) => {
            let response: Response = response.into();
            let response_class = coarse_response_class(&response);
            let mut exit_kind = ExitKind::Ok;

            let validation_error = process_response(
                request_index,
                &request,
                &response,
                api,
                crash_criteria,
                &mut exit_kind,
                parameter_feedback,
            );

            if exit_kind == ExitKind::Crash {
                Ok(ReplayStep::Crash(ObservedCrash {
                    identity: observed_response_identity(
                        response.status(),
                        validation_error.as_ref(),
                        Some(endpoint_string(&request)),
                        response_class,
                    ),
                    crashing_request_index: request_index,
                }))
            } else {
                Ok(ReplayStep::Continue)
            }
        }
        Err(error) => Ok(ReplayStep::Crash(ObservedCrash {
            identity: observed_transport_identity(&error, Some(endpoint_string(&request))),
            crashing_request_index: request_index,
        })),
    }
}
fn build_replay_request(
    client: &reqwest::blocking::Client,
    authentication: &mut Authentication,
    cookie_store: &std::sync::Arc<reqwest_cookie_store::CookieStoreMutex>,
    api: &Spec,
    request_timeout_ms: u64,
    request: &OpenApiRequest,
) -> Result<BuiltReplayRequest> {
    let request_builder =
        match build_request_from_input(client, authentication, cookie_store, api, request) {
            Ok(builder) => builder.timeout(Duration::from_millis(request_timeout_ms)),
            Err(error) => {
                log::warn!("Could not generate HTTP request while replaying crash: {error}");
                return Ok(BuiltReplayRequest::Skip);
            }
        };

    match request_builder.build() {
        Ok(request) => Ok(BuiltReplayRequest::Request(request)),
        Err(error) => {
            log::warn!("Reqwest failed to build replay request: {error}");
            Ok(BuiltReplayRequest::Stop)
        }
    }
}
fn observed_transport_identity(error: &reqwest::Error, endpoint: Option<String>) -> CrashIdentity {
    CrashIdentity {
        // The executor reports all transport failures as LibAFL timeouts; CrashKind keeps
        // the more specific timeout/connection/decode distinction.
        exit_kind: ObservedExitKind::Timeout,
        crash_kind: transport_crash_kind(error),
        http_status: None,
        validation_error_discriminant: None,
        endpoint,
        response_class: transport_response_class(error),
    }
}

fn endpoint_string(request: &OpenApiRequest) -> String {
    format!("{} {}", request.method, request.path)
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
