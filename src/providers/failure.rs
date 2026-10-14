//! Model failure facts travel with a request; diagnostic details stay out of
//! conversation records. Certificate rejection is distinct from API identity.
use std::error::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelFailureKind {
    Authentication,
    Forbidden,
    CertificateRevoked,
    CertificateInvalid,
    Connection,
    Timeout,
    RateLimited,
    Unavailable,
    ModelUnavailable,
    RegionUnavailable,
    Failed,
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("{detail}")]
pub(crate) struct ModelRequestFailure {
    pub(crate) kind: ModelFailureKind,
    detail: String,
}
impl ModelRequestFailure {
    pub(crate) fn report(kind: ModelFailureKind, detail: String) -> miette::Report {
        miette::Report::new(Self { kind, detail })
    }
    pub(crate) fn code(&self) -> &'static str {
        match self.kind {
            ModelFailureKind::Authentication => "model_authentication_failed",
            ModelFailureKind::Forbidden => "model_access_denied",
            ModelFailureKind::CertificateRevoked => "model_certificate_revoked",
            ModelFailureKind::CertificateInvalid => "model_certificate_invalid",
            ModelFailureKind::Connection => "model_connection_failed",
            ModelFailureKind::Timeout => "model_request_timeout",
            ModelFailureKind::RateLimited => "model_rate_limited",
            ModelFailureKind::Unavailable => "model_service_unavailable",
            ModelFailureKind::ModelUnavailable => "model_unavailable",
            ModelFailureKind::RegionUnavailable => "model_region_unavailable",
            ModelFailureKind::Failed => "model_request_failed",
        }
    }
    pub(crate) fn retryable(&self) -> bool {
        !matches!(
            self.kind,
            ModelFailureKind::Authentication
                | ModelFailureKind::Forbidden
                | ModelFailureKind::CertificateRevoked
                | ModelFailureKind::CertificateInvalid
                | ModelFailureKind::ModelUnavailable
                | ModelFailureKind::RegionUnavailable
        )
    }
    pub(crate) fn request_kind(error: &reqwest::Error) -> ModelFailureKind {
        if let Some(kind) = certificate_failure(error) {
            return kind;
        }
        if error.is_timeout() {
            ModelFailureKind::Timeout
        } else if error.is_connect() {
            ModelFailureKind::Connection
        } else {
            ModelFailureKind::Failed
        }
    }
    pub(crate) fn opencode_response(
        base: &str,
        status: reqwest::StatusCode,
        body: &str,
    ) -> Option<miette::Report> {
        if status.is_success() || !super::is_opencode_gateway_base_url(base) {
            return None;
        }
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        let error = value.get("error")?.as_object()?;
        let kind = error.get("type")?.as_str()?;
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let kind = match kind {
            "RegionError" => ModelFailureKind::RegionUnavailable,
            "server_error"
                if matches!(
                    message.trim().to_ascii_lowercase().as_str(),
                    "upstream request failed: model is unavailable." | "model is unavailable."
                ) =>
            {
                ModelFailureKind::ModelUnavailable
            }
            "server_error"
                if status.is_server_error()
                    || message
                        .trim()
                        .to_ascii_lowercase()
                        .starts_with("upstream request failed:") =>
            {
                ModelFailureKind::Unavailable
            }
            "upstream_error" => ModelFailureKind::Unavailable,
            _ => return None,
        };
        Some(Self::report(
            kind,
            format!(
                "OpenCode API returned HTTP {status}: {}",
                super::truncate_for_error(body)
            ),
        ))
    }
}
fn certificate_failure(error: &(dyn Error + 'static)) -> Option<ModelFailureKind> {
    let mut cause = Some(error);
    while let Some(error) = cause {
        let certificate = match error.downcast_ref::<rustls::Error>() {
            Some(rustls::Error::InvalidCertificate(certificate)) => Some(certificate),
            _ => None,
        };
        if let Some(certificate) = certificate {
            return Some(
                if matches!(certificate, rustls::CertificateError::Revoked) {
                    ModelFailureKind::CertificateRevoked
                } else {
                    ModelFailureKind::CertificateInvalid
                },
            );
        }
        // Some connector implementations erase the Rustls type. Inspect only
        // the actual transport cause, never stored/rendered message diagnostics.
        let description = error.to_string().to_ascii_lowercase();
        if description.contains("invalid peer certificate") {
            return Some(if description.contains("revoked") {
                ModelFailureKind::CertificateRevoked
            } else {
                ModelFailureKind::CertificateInvalid
            });
        }
        cause = error.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gateway_classification_preserves_unknown_errors_and_other_providers() {
        for (base, status, body) in [
            (
                "https://notopencode.ai/go/v1",
                400,
                r#"{"error":{"type":"server_error","message":"Model is unavailable."}}"#,
            ),
            (
                "https://opencode.ai/zen/v1",
                400,
                r#"{"error":{"type":"invalid_request_error","message":"unsupported parameter"}}"#,
            ),
            (
                "https://opencode.ai/go/v1",
                200,
                r#"{"error":{"type":"RegionError"}}"#,
            ),
            ("https://opencode.ai/go/v1", 400, "invalid JSON"),
            (
                "https://opencode.ai/go/v1",
                400,
                r#"{"error":{"type":"server_error","message":"unsupported parameter"}}"#,
            ),
        ] {
            assert!(
                ModelRequestFailure::opencode_response(
                    base,
                    reqwest::StatusCode::from_u16(status).unwrap(),
                    body
                )
                .is_none()
            );
        }
    }
    #[test]
    fn revoked_certificate_is_not_an_expired_api_credential() {
        let certificate = rustls::Error::InvalidCertificate(rustls::CertificateError::Revoked);
        assert_eq!(
            certificate_failure(&certificate),
            Some(ModelFailureKind::CertificateRevoked)
        );
        let error = ModelRequestFailure::report(
            ModelFailureKind::CertificateRevoked,
            "private transport diagnostics".into(),
        )
        .wrap_err("model request failed");
        let fact = error.downcast_ref::<ModelRequestFailure>().unwrap();
        assert_eq!(fact.code(), "model_certificate_revoked");
        assert!(!fact.retryable());
        assert_ne!(fact.code(), "model_authentication_failed");
    }
}
