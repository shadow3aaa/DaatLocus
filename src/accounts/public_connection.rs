//! Volatile account login publication facts, independent of Actor execution.
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationState {
    NotConfigured,
    WaitingForAccount,
    Connecting,
    Online,
    Reconnecting,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicationStatus {
    pub state: PublicationState,
    pub error_code: Option<&'static str>,
    pub changed_at_ms: i64,
    pub last_confirmed_at_ms: Option<i64>,
    pub retry_at_ms: Option<i64>,
}
impl PublicationStatus {
    pub(crate) fn initial(configured: bool, account: bool) -> Self {
        Self {
            state: if !configured {
                PublicationState::NotConfigured
            } else if !account {
                PublicationState::WaitingForAccount
            } else {
                PublicationState::Connecting
            },
            error_code: None,
            changed_at_ms: chrono::Utc::now().timestamp_millis(),
            last_confirmed_at_ms: None,
            retry_at_ms: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublicationError {
    Transport,
    Tls,
    Timeout,
    Rejected,
    Protocol,
    Authority,
    Heartbeat,
}
impl PublicationError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Transport => "public_login_connection_failed",
            Self::Tls => "public_login_tls_failed",
            Self::Timeout => "public_login_timeout",
            Self::Rejected => "public_login_registration_rejected",
            Self::Protocol => "public_login_protocol_error",
            Self::Authority => "public_login_authority_unavailable",
            Self::Heartbeat => "public_login_heartbeat_timeout",
        }
    }
    pub fn stage(self) -> crate::stage::StageError {
        use crate::stage::StageError;
        match self {
            Self::Rejected | Self::Authority => StageError::Forbidden(self.code()),
            Self::Protocol => StageError::Invalid(self.code()),
            _ => StageError::Unavailable(self.code()),
        }
    }
}
