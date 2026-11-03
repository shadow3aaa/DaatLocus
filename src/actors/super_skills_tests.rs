use super::*;
use crate::{
    app::{App, AppToolExecutionResult},
    super_skills_app::SuperSkillsApp,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn publish_skill(workspace: &Path, directory: &str) -> PathBuf {
    let root = workspace.join(".agents/skills").join(directory);
    std::fs::create_dir_all(root.join("references")).unwrap();
    std::fs::write(root.join("SKILL.md"), "---\nname: superskills-fixture\ndescription: Coordinate fixture work.\n---\nRead references/guide.md.\n").unwrap();
    std::fs::write(
        root.join("references/guide.md"),
        "First line\nCoordinate independent Actors\nThird line\n",
    )
    .unwrap();
    root.join("SKILL.md").canonicalize().unwrap()
}

fn core_context(system: &ActorSystem) -> AppToolExecutionContext {
    system.install_local_core().unwrap();
    system
        .send_user("core", "Use the coordination Skill".into(), vec![], None)
        .unwrap();
    let input = system.store().claim_next("core").unwrap().unwrap();
    system
        .execution_context(&system.core_device_actor().unwrap(), &input)
        .unwrap()
}

async fn execute(
    app: &mut SuperSkillsApp,
    context: &AppToolExecutionContext,
    name: &str,
    arguments: Value,
) -> Result<AppToolExecutionResult> {
    app.execute_tool(
        &AgentToolCall {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            arguments,
        },
        context,
    )
    .await
}

fn list_arguments(workspace: &Path) -> Value {
    json!({"stage_id":null,"workspace":workspace,"query":"superskills-fixture","cursor":null,"limit":1})
}

fn location(workspace: &Path, skill: &Path) -> Value {
    json!({"workspace":workspace,"path":skill})
}

#[tokio::test]
async fn superskills_catalog_search_pages_distinct_same_name_sources() {
    let (_directory, system, _) = paused_runtime().await;
    let workspace = system.device_workspace().canonicalize().unwrap();
    let first = publish_skill(&workspace, "first");
    let second = publish_skill(&workspace, "second");
    let context = core_context(&system);
    let mut app = SuperSkillsApp::new(system.clone());
    let first_page = execute(
        &mut app,
        &context,
        "list_skills",
        list_arguments(&workspace),
    )
    .await
    .unwrap();
    assert_eq!(first_page.payload["skills"].as_array().unwrap().len(), 1);
    assert_eq!(first_page.payload["skills"][0]["path"], json!(first));
    let mut next = list_arguments(&workspace);
    next["cursor"] = first_page.payload["next_cursor"].clone();
    let second_page = execute(&mut app, &context, "list_skills", next)
        .await
        .unwrap();
    assert_eq!(second_page.payload["skills"][0]["path"], json!(second));
    assert_eq!(second_page.payload["next_cursor"], Value::Null);
    system.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn superskills_routes_reads_and_searches_to_the_source_device_without_syncing_files() {
    let coordinator = Arc::new(ReplayProvider::new(vec![]));
    let worker = Arc::new(ReplayProvider::new(vec![]));
    let ((_a_dir, a, net_a), (_b_dir, b, net_b), _relay) =
        device_pair(coordinator.clone(), worker.clone()).await;
    let workspace = b.device_workspace().canonicalize().unwrap();
    let skill = publish_skill(&workspace, "remote-only");
    assert!(
        !a.device_workspace()
            .join(".agents/skills/remote-only")
            .exists()
    );
    let selected = location(&workspace, &skill);
    coordinator.replies.lock().extend([
        MockReply::Calls(vec![AgentToolCall { id:"remote-skill-catalog".into(), name:"super_skills__list_skills".into(), arguments:json!({"stage_id":net_b.host.local,"workspace":workspace,"query":"superskills-fixture","cursor":null,"limit":10}) }]),
        MockReply::Calls(vec![AgentToolCall { id:"remote-skill-search".into(), name:"super_skills__search_text".into(), arguments:json!({"stage_id":net_b.host.local,"skill":selected,"search":{"path":".","query":"independent","case_sensitive":true,"depth":3,"include_ignored":false,"limit":1}}) }]),
        MockReply::Calls(vec![AgentToolCall { id:"remote-skill-read".into(), name:"super_skills__read_file".into(), arguments:json!({"stage_id":net_b.host.local,"skill":selected,"file":{"path":"references/guide.md","start_line":2,"line_count":1}}) }]),
        MockReply::Answer("Read the remote coordination guidance"),
    ]);
    let input = a
        .send_user(
            "core",
            "Read that device's coordination Skill".into(),
            vec![],
            None,
        )
        .unwrap();
    let done = wait_completed(&a, &input.id).await;
    assert_eq!(done.status, MessageStatus::Completed, "{:?}", done.error);
    let tools = a
        .store()
        .tool_summaries("core", &input.id, None, 10)
        .unwrap()
        .items;
    assert_eq!(tools.len(), 3);
    for tool in tools {
        assert!(tool.error.is_none(), "{:?}", tool.error);
        let result = a.store().tool_result(&tool.id).unwrap();
        assert_eq!(result.payload["stage_id"], json!(net_b.host.local));
        if tool.call_id == "remote-skill-catalog" {
            assert_eq!(result.payload["skills"][0]["path"], json!(skill));
        }
        if tool.call_id == "remote-skill-read" {
            assert_eq!(result.payload["content"], "Coordinate independent Actors\n");
        }
    }
    assert!(
        worker.requests.lock().is_empty(),
        "reading Skills must not start another Actor"
    );
    assert!(
        !a.device_workspace()
            .join(".agents/skills/remote-only")
            .exists()
    );
    net_a.stop().await;
    net_b.stop().await;
    a.shutdown().await;
    b.shutdown().await;
}
