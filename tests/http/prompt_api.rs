//! Static source inspection is authenticated, bounded and detached from execution.
use crate::support::Fixture;
use reqwest::{Method, StatusCode};
use serde_json::Value;

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
