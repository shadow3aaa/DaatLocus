//! Sandbox application history is authenticated, bounded and read-only.
use crate::support::Fixture;
use daat_locus::actors::ApprovalDecision;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

#[tokio::test]
async fn application_history_checks_provenance_and_redacts_private_arguments() {
    let fixture = Fixture::new().await;
    let a = fixture.child("alpha").await;
    let b = fixture.child("beta").await;
    let approval = fixture.approval(&a).await;
    let path = format!("/v1/actors/{a}/approvals/{}", approval.id);
    assert_eq!(
        fixture
            .client
            .get(format!("{}{path}", fixture.url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = fixture.owner(Method::GET, &path).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.unwrap();
    assert!(
        !text.contains("fixture-secret-key")
            && text.contains("[redacted]")
            && text.contains("writable_roots")
    );
    let data: Value = serde_json::from_str(&text).unwrap();
    assert!(data.get("actor_id").is_none());
    assert_eq!(
        fixture
            .owner(
                Method::GET,
                &format!("/v1/actors/{b}/approvals/{}", approval.id)
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    for endpoint in [path, format!("/v1/approvals/{}", approval.id)] {
        for decision in ["approve", "deny"] {
            assert_eq!(
                fixture
                    .owner(Method::POST, &endpoint)
                    .json(&json!({"decision":decision}))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::METHOD_NOT_ALLOWED
            );
        }
    }
    let stored = fixture.system.store().approval(&approval.id).unwrap();
    assert_eq!(stored.decision, ApprovalDecision::Pending);
    assert_eq!(stored.call, approval.call);
    assert_eq!(stored.requested, approval.requested);
}

#[tokio::test]
async fn application_history_paginates_with_event_cursor_and_rejects_retired_owner_filter() {
    let fixture = Fixture::new().await;
    let actor = fixture.child("worker").await;
    let original = fixture.approval(&actor).await;
    for _ in 0..35 {
        let mut record = original.clone();
        record.seq = 0;
        record.id = uuid::Uuid::new_v4().to_string();
        record.call.id = uuid::Uuid::new_v4().to_string();
        fixture.system.store().create_approval(record).unwrap();
    }
    let page: Value = fixture
        .owner(Method::GET, "/v1/approvals?limit=10")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 10);
    assert_eq!(
        page["event_cursor"],
        fixture.system.store().event_cursor().unwrap()
    );
    let cursor = page["next_cursor"].as_i64().unwrap();
    let next: Value = fixture
        .owner(
            Method::GET,
            &format!("/v1/approvals?limit=10&before={cursor}"),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for item in next["items"].as_array().unwrap() {
        assert!(
            !page["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|previous| previous["id"] == item["id"])
        );
    }
    assert_eq!(
        fixture
            .owner(Method::GET, "/v1/approvals?awaiting_owner=true")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
}
