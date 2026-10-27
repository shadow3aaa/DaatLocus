//! Static source inspection is authenticated, bounded and detached from execution.
use crate::support::Fixture;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

async fn composition(fixture: &Fixture, role: &str) -> Value {
    fixture
        .owner(Method::GET, &format!("/v1/system-prompt?role={role}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn instruction_library_edits_membership_and_source_reads_share_one_assembly() {
    let fixture = Fixture::new().await;
    let before = composition(&fixture, "core").await;
    let create = fixture
        .owner(Method::POST, "/v1/instruction-layers")
        .json(&json!({"name":"shared", "content":"# 中文指令\n\nUse explicit examples."}))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let created: Value = create.json().await.unwrap();
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["name"], "shared.md");
    assert_eq!(created["roles"], json!([]));
    assert_eq!(
        composition(&fixture, "core").await["revision"],
        before["revision"],
        "an unused file does not alter inference"
    );
    for role in ["core", "others"] {
        let current = composition(&fixture, role).await;
        let response = fixture
            .owner(
                Method::PUT,
                &format!("/v1/system-prompt/layers?role={role}"),
            )
            .json(&json!({"revision":current["revision"],"layer_ids":[id]}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            response.text().await.unwrap()
        );
        let enabled = composition(&fixture, role).await;
        let files = enabled["items"].as_array().unwrap();
        let file = files.last().unwrap();
        assert_eq!(file["layer_id"], id);
        assert!(
            files[..files.len() - 1]
                .iter()
                .all(|file| file["layer_id"].is_null())
        );
        let body: Value = fixture
            .owner(
                Method::GET,
                &format!(
                    "/v1/system-prompt/{}?role={role}&revision={}",
                    file["index"],
                    enabled["revision"].as_str().unwrap()
                ),
            )
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["content"], created["content"]);
    }
    let previous_core = composition(&fixture, "core").await;
    let previous_others = composition(&fixture, "others").await;
    let updated = fixture.owner(Method::PATCH, &format!("/v1/instruction-layers/{id}"))
        .json(&json!({"name":"renamed.md","content":"New shared text", "revision":created["revision"]}))
        .send().await.unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let updated: Value = updated.json().await.unwrap();
    assert_eq!(updated["roles"], json!(["core", "others"]));
    let current = composition(&fixture, "core").await;
    assert_ne!(current["revision"], previous_core["revision"]);
    assert_ne!(
        composition(&fixture, "others").await["revision"],
        previous_others["revision"]
    );
    assert_eq!(fixture.owner(Method::PATCH, &format!("/v1/instruction-layers/{id}"))
        .json(&json!({"name":"stale","content":"must not overwrite", "revision":created["revision"]}))
        .send().await.unwrap().status(), StatusCode::CONFLICT);
    assert_eq!(
        fixture
            .owner(Method::PUT, "/v1/system-prompt/layers")
            .json(&json!({"revision":previous_core["revision"],"layer_ids":[]}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    for ids in [json!(["core.md"]), json!([id, id])] {
        assert_eq!(
            fixture
                .owner(Method::PUT, "/v1/system-prompt/layers")
                .json(&json!({"revision":current["revision"],"layer_ids":ids}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        fixture
            .owner(Method::PUT, "/v1/system-prompt/layers")
            .json(&json!({"revision":current["revision"],"layer_ids":[]}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let library: Value = fixture
        .owner(Method::GET, "/v1/instruction-layers?q=shared&limit=1")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(library["items"][0]["id"], id, "search includes the content");
    assert!(library["items"][0].get("content").is_none());
    assert_eq!(
        library["items"][0]["roles"],
        json!(["others"]),
        "removal preserves the library file and other role"
    );
    assert_eq!(
        composition(&fixture, "core").await["revision"],
        before["revision"]
    );
}

#[tokio::test]
async fn instruction_mutations_require_owner_origin_and_valid_file_names() {
    let fixture = Fixture::new().await;
    let body = json!({"name":"safe","content":"private instructions"});
    for method in [Method::GET, Method::POST] {
        assert_eq!(
            fixture
                .client
                .request(method, format!("{}/v1/instruction-layers", fixture.url))
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let cookie = fixture.cookie.read().clone();
    for origin in [None, Some("https://foreign.invalid")] {
        let mut request = fixture
            .client
            .post(format!("{}/v1/instruction-layers", fixture.url))
            .header("cookie", &cookie)
            .header("x-daat-request", "1")
            .json(&body);
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    for name in ["../escape.md", "folder/file.md", "", ".md"] {
        assert_eq!(
            fixture
                .owner(Method::POST, "/v1/instruction-layers")
                .json(&json!({"name":name,"content":"text"}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        fixture
            .owner(Method::POST, "/v1/instruction-layers")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        fixture
            .owner(Method::POST, "/v1/instruction-layers")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn role_prompt_files_match_sources_and_reject_stale_or_invalid_reads() {
    let fixture = Fixture::new().await;
    assert_eq!(
        fixture
            .client
            .get(format!("{}/v1/system-prompt", fixture.url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for (role, expected) in [
        (
            "core",
            include_str!("../../src/prompts/core.md").replace("{{account_name}}", "fixture"),
        ),
        (
            "others",
            include_str!("../../src/prompts/actors.md").to_owned(),
        ),
    ] {
        let list: Value = fixture
            .owner(
                Method::GET,
                &format!("/v1/system-prompt?role={role}&limit=1"),
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(list["items"].as_array().unwrap().len(), 1);
        assert!(list["items"][0].get("content").is_none());
        let base = format!(
            "/v1/system-prompt/0?role={role}&revision={}",
            list["revision"].as_str().unwrap()
        );
        let file: Value = fixture
            .owner(Method::GET, &base)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(file["content"], expected);
        assert!(file["next_offset"].is_null());
        assert_eq!(
            fixture
                .owner(Method::GET, &format!("{base}&offset=999999"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let catalog: Value = fixture
            .owner(
                Method::GET,
                &format!("/v1/system-prompt?role={role}&limit=100"),
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let has_platform_patch = catalog["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["name"].as_str().unwrap().starts_with("os/"));
        assert_eq!(
            has_platform_patch,
            role != "core"
                && !include_str!("../../src/prompts/os/windows-patch.md")
                    .trim()
                    .is_empty()
                && cfg!(target_os = "windows"),
            "the inspected assembly must enforce the same Core/platform boundary as execution"
        );
        if role == "core" {
            let next: Value = fixture
                .owner(
                    Method::GET,
                    &format!("/v1/system-prompt?role={role}&after=1&limit=1"),
                )
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(next["items"][0]["index"], 1);
        }
    }
    for (path, status) in [
        ("/v1/system-prompt?role=invalid", StatusCode::BAD_REQUEST),
        (
            "/v1/system-prompt?limit=101",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("/v1/system-prompt/0?revision=stale", StatusCode::CONFLICT),
    ] {
        assert_eq!(
            fixture
                .owner(Method::GET, path)
                .send()
                .await
                .unwrap()
                .status(),
            status
        );
    }
}
