//! Conversation import admission: selected bytes, Owner origin and immutable Core resources.
use crate::support::Fixture;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

#[tokio::test]
async fn codex_preview_and_resource_download_preserve_a_conversation_over_32_mib() {
    let fixture = Fixture::new().await;
    let original = "x".repeat(33 * 1024 * 1024);
    let session = "00000000-0000-4000-8000-000000000033";
    let input = format!(
        "{}\n{}",
        json!({"type":"session_meta","payload":{"id":session}}),
        json!({"type":"event_msg","payload":{"type":"user_message","message":original}})
    );
    let directory = fixture._temp.path().join("codex/sessions");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join(format!("rollout-{session}.jsonl")), input).unwrap();
    let listed: Value = fixture
        .owner(Method::GET, "/v1/imports/codex")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let source = listed["items"][0]["id"].as_str().unwrap();
    let response = fixture
        .owner(Method::POST, "/v1/imports/codex/preview")
        .json(&json!({"source_id":source}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let preview: Value = response.json().await.unwrap();
    assert_eq!(preview["message_count"], 1);
    let id = preview["resource_id"].as_str().unwrap();
    let response = fixture
        .owner(Method::GET, &format!("/v1/actors/core/resources/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let evidence: Value = response.json().await.unwrap();
    assert_eq!(
        evidence["messages"],
        json!([{"role":"user","text":original}])
    );
    fixture.system.shutdown().await;
}

#[tokio::test]
async fn codex_preview_checks_owner_origin_and_retains_only_inert_conversation() {
    let fixture = Fixture::new().await;
    let input = [
        json!({"type":"session_meta","payload":{"id":"0d9726a9-b621-403e-ae4d-34e85d0f471e","cwd":"FOREIGN_WORKSPACE","instructions":"FOREIGN_GRANTS"}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Check <layout> 🔵"}]}}),
        json!({"type":"response_item","payload":{"type":"message","role":"assistant","channel":"final","content":[{"type":"output_text","text":"Keep the original text."}]}}),
        json!({"type":"response_item","payload":{"type":"function_call","arguments":"PRIVATE_TOOL_ARGUMENTS"}}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    let home = fixture._temp.path().join("codex");
    let sessions = home.join("sessions/2026/10/17");
    std::fs::create_dir_all(&sessions).unwrap();
    let rollout =
        sessions.join("rollout-2026-10-17T12-00-00-0d9726a9-b621-403e-ae4d-34e85d0f471e.jsonl");
    std::fs::write(&rollout, &input).unwrap();
    std::fs::write(home.join("auth.json"), "DO_NOT_READ_CREDENTIALS").unwrap();
    std::fs::write(
        home.join("session_index.jsonl"),
        json!({"id":"0d9726a9-b621-403e-ae4d-34e85d0f471e","thread_name":"Check layout"})
            .to_string(),
    )
    .unwrap();
    let archive = home.join("archived_sessions");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::write(
        archive.join("rollout-00000000-0000-4000-8000-000000000001.jsonl"),
        "{}\n",
    )
    .unwrap();
    let first: Value = fixture
        .owner(Method::GET, "/v1/imports/codex?limit=1")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let after = first["next_cursor"]
        .as_str()
        .expect("two conversations paginate");
    let second: Value = fixture
        .owner(
            Method::GET,
            &format!("/v1/imports/codex?limit=1&after={after}"),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
    assert!(second["next_cursor"].is_null());
    assert_ne!(
        first["items"][0]["archived"],
        second["items"][0]["archived"]
    );
    assert_eq!(
        fixture
            .client
            .get(format!("{}/v1/imports/codex", fixture.url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let list: Value = fixture
        .owner(Method::GET, "/v1/imports/codex?limit=1&search=layout")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["title"], "Check layout");
    assert!(list["items"][0].get("path").is_none());
    let source_id = list["items"][0]["id"].as_str().unwrap();
    let path = "/v1/imports/codex/preview";
    let selection = json!({"source_id":source_id});
    assert_eq!(
        fixture
            .client
            .post(format!("{}{path}", fixture.url))
            .json(&selection)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let cookie = fixture.cookie.read().clone();
    assert_eq!(
        fixture
            .client
            .post(format!("{}{path}", fixture.url))
            .header("Cookie", cookie)
            .header("Origin", "https://untrusted.example")
            .header("x-daat-request", "1")
            .json(&selection)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let response = fixture
        .owner(Method::POST, path)
        .json(&selection)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
    let preview: Value = response.json().await.unwrap();
    assert_eq!(preview["title"], "Check <layout> 🔵");
    assert_eq!(preview["message_count"], 2);
    let resource_id = preview["resource_id"].as_str().unwrap();
    let evidence = fixture
        .owner(
            Method::GET,
            &format!("/v1/actors/core/resources/{resource_id}"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(evidence.status(), StatusCode::OK);
    let evidence: Value = evidence.json().await.unwrap();
    assert_eq!(
        evidence["messages"],
        json!([{"role":"user","text":"Check <layout> 🔵"},{"role":"assistant","text":"Keep the original text."}])
    );
    for private in [
        "FOREIGN_WORKSPACE",
        "FOREIGN_GRANTS",
        "PRIVATE_TOOL_ARGUMENTS",
    ] {
        assert!(!evidence.to_string().contains(private));
    }
    let again: Value = fixture
        .owner(Method::POST, path)
        .json(&selection)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again["resource_id"], resource_id);
    assert!(
        fixture
            .system
            .store()
            .messages("core", None, 10)
            .unwrap()
            .items
            .is_empty(),
        "Preview never queues an input"
    );
    assert_eq!(
        fixture
            .owner(Method::POST, path)
            .json(&json!({"source_id":"../auth.json"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    std::fs::write(&rollout, "{broken").unwrap();
    let changed = fixture
        .owner(Method::POST, path)
        .json(&selection)
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::CONFLICT);
    let refreshed: Value = fixture
        .owner(Method::GET, "/v1/imports/codex?refresh=true")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let malformed = fixture
        .owner(Method::POST, path)
        .json(&json!({"source_id":refreshed["items"][0]["id"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        malformed.json::<Value>().await.unwrap()["code"],
        "invalid_codex_conversation"
    );
    let unknown = fixture
        .owner(Method::POST, "/v1/imports/codex")
        .json(&json!({"resource_id":"not-owned"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    // The fixture deliberately has no active Core controller: submission must
    // obey the same synchronization gate as an ordinary conversation message.
    let blocked = fixture
        .owner(Method::POST, "/v1/imports/codex")
        .json(&json!({"resource_id":resource_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        blocked.json::<Value>().await.unwrap()["code"],
        "core_not_ready"
    );
    assert!(
        fixture
            .system
            .store()
            .messages("core", None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    fixture.system.shutdown().await;
}
