#![allow(dead_code)]
//TODO : Remove dead_code for next PR if possible

use crate::openapi::validate_response::ValidationErrorDiscriminants;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CrashKind {
    Http5xx,
    Validation(ValidationErrorDiscriminants),
    HttpResponseCrash,
    TransportTimeout,
    TransportConnectionError,
    TransportDecodeError,
    TransportUnknownError,
}

impl std::fmt::Display for CrashKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http5xx => f.write_str("http_5xx"),
            Self::Validation(discriminant) => write!(f, "validation_{discriminant:?}"),
            Self::HttpResponseCrash => f.write_str("http_response_crash"),
            Self::TransportTimeout => f.write_str("transport_timeout"),
            Self::TransportConnectionError => f.write_str("transport_connection_error"),
            Self::TransportDecodeError => f.write_str("transport_decode_error"),
            Self::TransportUnknownError => f.write_str("transport_unknown_error"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseClass {
    Json,
    Empty,
    Plaintext,
    Html,
    InvalidJson,
    BinaryOrUnknown,
    TransportTimeout,
    TransportConnectionError,
    TransportDecodeError,
    TransportUnknownError,
}

impl std::fmt::Display for ResponseClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Json => "Json",
            Self::Empty => "Empty",
            Self::Plaintext => "Plaintext",
            Self::Html => "Html",
            Self::InvalidJson => "InvalidJson",
            Self::BinaryOrUnknown => "BinaryOrUnknown",
            Self::TransportTimeout => "TransportTimeout",
            Self::TransportConnectionError => "TransportConnectionError",
            Self::TransportDecodeError => "TransportDecodeError",
            Self::TransportUnknownError => "TransportUnknownError",
        };
        f.write_str(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedExitKind {
    Crash,
    Timeout,
}

impl std::fmt::Display for ObservedExitKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Crash => f.write_str("Crash"),
            Self::Timeout => f.write_str("Timeout"),
        }
    }
}

/// Coarse identity used to group crashes with similar characteristics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrashIdentity {
    pub exit_kind: ObservedExitKind,
    pub crash_kind: CrashKind,
    pub http_status: Option<u16>,
    pub validation_error_discriminant: Option<String>,
    pub endpoint: Option<String>,
    pub response_class: ResponseClass,
}

impl CrashIdentity {
    /// Returns the exact key used for crash clustering.
    pub fn cluster_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.exit_kind,
            self.crash_kind,
            self.http_status.map(|s| s.to_string()).unwrap_or_default(),
            self.validation_error_discriminant.as_deref().unwrap_or(""),
            self.endpoint.as_deref().unwrap_or(""),
            self.response_class,
        )
    }

    /// Returns whether `candidate` is close enough to this baseline identity
    /// to be accepted during minimization.
    pub fn compatible_with(&self, candidate: &CrashIdentity) -> bool {
        if candidate.exit_kind != self.exit_kind {
            return false;
        }
        if candidate.crash_kind != self.crash_kind {
            return false;
        }
        if candidate.response_class != self.response_class {
            return false;
        }
        if self.endpoint.is_some() && candidate.endpoint != self.endpoint {
            return false;
        }
        if self.http_status.is_some() && candidate.http_status != self.http_status {
            return false;
        }
        if self.validation_error_discriminant.is_some()
            && candidate.validation_error_discriminant != self.validation_error_discriminant
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> CrashIdentity {
        CrashIdentity {
            exit_kind: ObservedExitKind::Crash,
            crash_kind: CrashKind::Http5xx,
            http_status: Some(500),
            validation_error_discriminant: None,
            endpoint: Some("POST /users".into()),
            response_class: ResponseClass::Json,
        }
    }

    #[test]
    fn cluster_key_is_stable_for_equal_identities() {
        assert_eq!(identity().cluster_key(), identity().cluster_key());
    }

    #[test]
    fn compatible_with_accepts_same_identity() {
        let baseline = identity();
        let candidate = identity();
        assert!(baseline.compatible_with(&candidate));
    }

    #[test]
    fn compatible_with_rejects_different_crash_kind() {
        let baseline = identity();
        let mut candidate = identity();
        candidate.crash_kind = CrashKind::HttpResponseCrash;

        assert!(!baseline.compatible_with(&candidate));
    }

    #[test]
    fn compatible_with_tolerates_absent_baseline_optional_fields() {
        let mut baseline = identity();
        baseline.endpoint = None;
        baseline.http_status = None;

        let candidate = identity();

        assert!(baseline.compatible_with(&candidate));
    }

    #[test]
    fn compatible_with_rejects_missing_candidate_optional_field_when_baseline_has_it() {
        let baseline = identity();
        let mut candidate = identity();
        candidate.endpoint = None;

        assert!(!baseline.compatible_with(&candidate));
    }
}
