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
