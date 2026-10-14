//! Approvals HTTP contracts.
use crate::support::Fixture;
use daat_locus::actors::ApprovalDecision;
use daat_locus::actors::ApprovalMode;
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn ask_approvals_are_owner_only_exactly_scoped_and_immutable() {
    let fixture = Fixture::new().await;
    let a = fixture.child("alpha").await;
    let b = fixture.child("beta").await;
    let approval = fixture.approval(&a, ApprovalMode::Ask).await;
    let cookie = fixture.login_cookie().await;
    let path = format!("/v1/actors/{a}/approvals/{}", approval.id);
    let detail = fixture
        .client
        .get(format!("{}{path}", fixture.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let text = detail.text().await.unwrap();
    assert!(
        !text.contains("fixture-secret-key")
            && text.contains("[redacted]")
            && text.contains("writable_roots")
    );
    assert_eq!(
        fixture
            .owner(
                reqwest::Method::GET,
                &format!("/v1/actors/{b}/approvals/{}", approval.id)
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .owner(reqwest::Method::POST, &path)
            .json(&json!({"decision": "approve", "requested": {"writable_roots": ["C:/"]}}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let resolved = fixture
        .owner(reqwest::Method::POST, &path)
        .json(&json!({"decision": "approve"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resolved.status(), StatusCode::OK);
    let stored = fixture.system.store().approval(&approval.id).unwrap();
    assert_eq!(stored.decision, ApprovalDecision::Approved);
    assert_eq!(stored.requested, approval.requested);
    assert_eq!(stored.call, approval.call);
    assert_eq!(
        fixture
            .owner(reqwest::Method::POST, &path)
            .json(&json!({"decision": "deny"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn auto_review_cannot_be_overridden_and_pending_filter_does_not_starve_ask() {
    let fixture = Fixture::new().await;
    let a = fixture.child("ask-actor").await;
    let b = fixture.child("auto-actor").await;
    let ask = fixture.approval(&a, ApprovalMode::Ask).await;
    let auto = fixture.approval(&b, ApprovalMode::Auto).await;
    // Fill newer history with in-flight reviews; the pending query must filter before LIMIT.
    for _ in 0..110 {
        let mut record = auto.clone();
        record.seq = 0;
        record.id = uuid::Uuid::new_v4().to_string();
        record.call.id = uuid::Uuid::new_v4().to_string();
        fixture.system.store().create_approval(record).unwrap();
    }
    let pending = fixture
        .owner(
            reqwest::Method::GET,
            "/v1/approvals?awaiting_owner=true&limit=1",
        )
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(pending["items"].as_array().unwrap().len(), 1);
    assert_eq!(pending["items"][0]["id"], ask.id);
    assert_eq!(
        pending["event_cursor"],
        fixture.system.store().event_cursor().unwrap()
    );
    assert!(pending["next_cursor"].is_null());
    let resolved = fixture
        .owner(reqwest::Method::POST, &format!("/v1/approvals/{}", auto.id))
        .json(&json!({"decision": "approve"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resolved.status(), StatusCode::CONFLICT);
    assert_eq!(
        fixture.system.store().approval(&auto.id).unwrap().decision,
        ApprovalDecision::Pending
    );
    fixture.system.cancel(&a, &ask.message_id).unwrap();
    assert_eq!(
        fixture
            .owner(reqwest::Method::POST, &format!("/v1/approvals/{}", ask.id))
            .json(&json!({"decision": "approve"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
}
