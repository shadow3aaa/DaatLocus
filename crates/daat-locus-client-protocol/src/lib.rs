//! Narrow account bootstrap requests over authenticated local OS-owner IPC.
pub mod computer_use;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BootstrapRequest {
    Info {},
    RegisterStart {
        name: String,
        request: String,
    },
    RegisterFinish {
        id: String,
        message: String,
    },
    LoginStart {
        name: String,
        request: String,
        exclude: Vec<String>,
    },
    LoginImport {
        id: String,
        record: String,
    },
    Connection {
        relay_origin: Option<String>,
    },
}
#[derive(Serialize, Deserialize)]
pub struct BootstrapReply {
    pub status: u16,
    pub body: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bootstrap_cannot_describe_actor_admin_session_or_shell_operations() {
        for value in [
            r#"{"kind":"stop"}"#,
            r#"{"kind":"login_finish","id":"x","message":"x"}"#,
            r#"{"kind":"info","path":"/v1/actors/root"}"#,
            r#"{"kind":"connection","relay_origin":null,"owner":true}"#,
        ] {
            assert!(serde_json::from_str::<BootstrapRequest>(value).is_err());
        }
    }
}
