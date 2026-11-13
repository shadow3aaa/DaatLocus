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
    system
        .send_user("core", "Use the coordination Skill".into(), vec![], None)
        .unwrap();
    let input = system.store().claim_next("core").unwrap().unwrap();
    system
        .execution_context(&system.actor("core").unwrap(), &input)
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

#[tokio::test]
async fn selected_skills_survive_retry_and_reach_the_core_model_as_source_references() {
    let (_directory, system, _) = paused_runtime().await;
    let workspace = system.device_workspace().canonicalize().unwrap();
    let path = publish_skill(&workspace, "selected");
    let selected = crate::actors::SkillReference {
        stage_id: system.store().public_stage_id().unwrap(),
        workspace: workspace.to_string_lossy().into(),
        path: path.to_string_lossy().into(),
        name: "superskills-fixture".into(),
    };
    let input = system
        .send_user_with_skills(
            "core",
            "$superskills-fixture".into(),
            vec![],
            vec![selected.clone()],
            Some("selected".into()),
        )
        .unwrap();
    let retry = system
        .send_user_with_skills(
            "core",
            input.content.clone(),
            vec![],
            vec![selected.clone()],
            Some("selected".into()),
        )
        .unwrap();
    assert_eq!(input.id, retry.id);
    let mut changed = selected.clone();
    changed.path.push_str(".changed");
    assert!(
        system
            .send_user_with_skills(
                "core",
                input.content.clone(),
                vec![],
                vec![changed],
                Some("selected".into())
            )
            .is_err()
    );
    let running = system.store().claim_next("core").unwrap().unwrap();
    assert_eq!(running.skills, vec![selected]);
    let actor = system.actor("core").unwrap();
    let context = system.execution_context(&actor, &running).unwrap();
    let rendered = system.message_content(&actor, &running, &context).unwrap();
    let content = rendered.as_text();
    assert!(content.contains(&serde_json::to_string(&path).unwrap()));
    assert!(
        !content.contains("Coordinate independent Actors"),
        "references must not inject Skill bodies"
    );
    system.shutdown().await;
}
