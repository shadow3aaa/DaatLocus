//! Stable, credential-free account login diagnostics shared by device and HTTP paths.
use crate::stage::StageError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LoginError {
    #[error("Configure a public connection service or connect to a device on the local network")]
    ConnectionRequired,
    #[error("Cannot reach the public connection service")]
    RelayUnavailable,
    #[error("No online account device was found")]
    NoOnlineDevice,
    #[error("Cannot connect to the account device")]
    DeviceUnreachable,
    #[error("Account login timed out")]
    TimedOut,
    #[error("Incorrect account name or password")]
    InvalidCredentials,
    #[error("Login attempt expired; sign in again")]
    Expired,
    #[error("This device's account access was revoked")]
    Revoked,
    #[error("Account credentials changed; sign in again")]
    AccountChanged,
    #[error("Account login protocol could not be verified")]
    Protocol,
    #[error("Account device enrollment could not be verified")]
    ImportRejected,
    #[error("Account login is busy; retry shortly")]
    Busy,
    #[error("The local account service is unavailable")]
    Unavailable,
}

impl LoginError {
    pub fn code(self) -> &'static str {
        match self {
            Self::ConnectionRequired => "login_connection_required",
            Self::RelayUnavailable => "login_relay_unavailable",
            Self::NoOnlineDevice => "login_no_online_device",
            Self::DeviceUnreachable => "login_device_unreachable",
            Self::TimedOut => "login_timeout",
            Self::InvalidCredentials => "login_invalid_credentials",
            Self::Expired => "login_expired",
            Self::Revoked => "login_device_revoked",
            Self::AccountChanged => "login_account_changed",
            Self::Protocol => "login_protocol_error",
            Self::ImportRejected => "login_import_rejected",
            Self::Busy => "login_busy",
            Self::Unavailable => "login_service_unavailable",
        }
    }
    /// Connection failures precede password verification. Never label them as bad credentials.
    pub fn connection(error: &StageError) -> Self {
        match error {
            StageError::Invalid(_) | StageError::Forbidden(_) | StageError::Conflict(_) => {
                Self::Protocol
            }
            StageError::Capacity(_) => Self::Busy,
            _ => Self::DeviceUnreachable,
        }
    }
}
