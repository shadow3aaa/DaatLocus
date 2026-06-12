use serde::Serialize;

#[derive(Clone, Copy)]
pub(super) struct DashboardCommandSpec {
    pub(super) primary_verb: &'static str,
    pub(super) description: &'static str,
    aliases: &'static [&'static str],
    remote_command: Option<&'static str>,
    remote_description: Option<&'static str>,
}

impl DashboardCommandSpec {
    pub(super) fn accepts(self, verb: &str) -> bool {
        self.primary_verb == verb || self.aliases.contains(&verb)
    }

    fn remote_description(self) -> &'static str {
        self.remote_description.unwrap_or(self.description)
    }
}

const NO_ALIASES: &[&str] = &[];
const QUIT_ALIASES: &[&str] = &["q", "exit"];
const APP_STATUS_ALIASES: &[&str] = &["app_status"];

static DASHBOARD_COMMANDS: [DashboardCommandSpec; 9] = [
    DashboardCommandSpec {
        primary_verb: "quit",
        description: "exit the dashboard",
        aliases: QUIT_ALIASES,
        remote_command: None,
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "clear",
        description: "clear runtime conversation history, current plan, and all events",
        aliases: NO_ALIASES,
        remote_command: Some("clear"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "debug",
        description: "debug outputs and internal runtime views",
        aliases: NO_ALIASES,
        remote_command: Some("debug"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "app-status",
        description: "show current structured app state and llm-facing note",
        aliases: APP_STATUS_ALIASES,
        remote_command: Some("app_status"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "status",
        description: "show overall status",
        aliases: NO_ALIASES,
        remote_command: Some("status"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "restart",
        description: "restart the daemon",
        aliases: NO_ALIASES,
        remote_command: Some("restart"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "sleep",
        description: "sleep controls and status",
        aliases: NO_ALIASES,
        remote_command: Some("sleep"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "skills",
        description: "list and manage OpenSkills automatic use",
        aliases: NO_ALIASES,
        remote_command: Some("skills"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "telegram",
        description: "telegram status and access controls",
        aliases: NO_ALIASES,
        remote_command: Some("telegram"),
        remote_description: None,
    },
];

pub(super) fn dashboard_commands() -> &'static [DashboardCommandSpec] {
    &DASHBOARD_COMMANDS
}

fn dashboard_command_spec(primary_verb: &str) -> Option<DashboardCommandSpec> {
    dashboard_commands()
        .iter()
        .copied()
        .find(|command| command.primary_verb == primary_verb)
}

fn dashboard_command_accepts(primary_verb: &str, verb: &str) -> bool {
    dashboard_command_spec(primary_verb).is_some_and(|command| command.accepts(verb))
}

pub(super) fn quit_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("quit", verb)
}

pub(super) fn clear_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("clear", verb)
}

pub(super) fn status_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("status", verb)
}

pub(super) fn restart_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("restart", verb)
}

pub(super) fn debug_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("debug", verb)
}

pub(super) fn app_status_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("app-status", verb)
}

pub(super) fn sleep_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("sleep", verb)
}

pub(super) fn skills_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("skills", verb)
}

pub(super) fn telegram_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("telegram", verb)
}

pub(super) fn dashboard_command_is_known(verb: &str) -> bool {
    dashboard_commands()
        .iter()
        .copied()
        .any(|command| command.accepts(verb))
}

#[derive(Clone, Copy, Serialize)]
pub(crate) struct RemoteDashboardCommand {
    pub command: &'static str,
    pub description: &'static str,
}

pub(crate) fn remote_dashboard_commands() -> Vec<RemoteDashboardCommand> {
    dashboard_commands()
        .iter()
        .filter_map(|command| {
            command
                .remote_command
                .map(|remote_command| RemoteDashboardCommand {
                    command: remote_command,
                    description: command.remote_description(),
                })
        })
        .collect()
}
