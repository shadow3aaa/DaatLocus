//! Resources HTTP contracts.
use crate::support::Fixture;
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn attachments_are_opaque_scoped_and_active_documents_are_sandboxed() {
    let fixture = Fixture::new().await;
    let b = fixture.child("beta").await;
    let unicode = fixture
        .owner(reqwest::Method::POST, "/v1/core/attachments")
        .header("x-file-name", "%E8%B0%83%E7%A0%94%E6%8A%A5%E5%91%8A.txt")
        .header("x-file-name-encoding", "utf-8-percent")
        .header("content-type", "text/plain")
        .body("中文文件内容")
        .send()
        .await
        .unwrap();
    assert_eq!(unicode.status(), StatusCode::CREATED);
    assert_eq!(
        unicode.json::<Value>().await.unwrap()["name"],
        "调研报告.txt"
    );
    for name in ["%2Fprivate.txt", "%FF.txt", "incomplete%2"] {
        assert_eq!(
            fixture
                .owner(reqwest::Method::POST, "/v1/core/attachments")
                .header("x-file-name", name)
                .header("x-file-name-encoding", "utf-8-percent")
                .body("invalid name")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let upload = fixture
        .owner(reqwest::Method::POST, "/v1/core/attachments")
        .header("x-file-name", "report.html")
        .header("content-type", "text/html")
        .body("<script>fetch('/v1/config')</script>")
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::CREATED);
    let upload = upload.json::<Value>().await.unwrap();
    let id = upload["id"].as_str().unwrap();
    assert!(upload.get("path").is_none());
    let cookie = fixture.login_cookie().await;
    let download = fixture
        .client
        .get(format!(
            "{}{uri}",
            fixture.url,
            uri = upload["uri"].as_str().unwrap()
        ))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    let csp = download.headers()["content-security-policy"]
        .to_str()
        .unwrap();
    assert!(
        csp.contains("sandbox")
            && !csp.contains("allow-same-origin")
            && csp.contains("connect-src 'none'")
    );
    assert_eq!(
        fixture
            .client
            .get(format!("{}/v1/actors/{b}/resources/{id}", fixture.url))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .owner(reqwest::Method::POST, &format!("/v1/actors/{b}/messages"))
            .json(&json!({"content": "file", "attachments": [id]}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        fixture
            .owner(reqwest::Method::POST, "/v1/core/messages")
            .json(&json!({"content": "no", "attachments": [{"path": "C:/private"}]}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let upload = fixture
        .owner(reqwest::Method::POST, "/v1/core/attachments")
        .header("x-file-name", "large.bin")
        .header("content-type", "application/octet-stream")
        .body(vec![7u8; 2 * 1024 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::CREATED);
    let upload = upload.json::<Value>().await.unwrap();
    assert_eq!(upload["size_bytes"], 2 * 1024 * 1024);
    let bytes = fixture
        .client
        .get(format!(
            "{}{}",
            fixture.url,
            upload["uri"].as_str().unwrap()
        ))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(bytes.len(), 2 * 1024 * 1024);
}

#[tokio::test]
async fn historical_tool_queries_return_small_refs_then_scoped_details() {
    let fixture = Fixture::new().await;
    let a = fixture.child("alpha").await;
    let b = fixture.child("beta").await;
    let message = fixture
        .system
        .send_user(&a, "historical tools".into(), vec![], None)
        .unwrap();
    // This paused fixture must claim its input before recording an in-flight call.
    let active = fixture.system.store().claim_next(&a).unwrap().unwrap();
    assert_eq!(active.id, message.id);
    let mut ids = vec![];
    for index in 0..3 {
        let call = daat_locus::model_protocol::AgentToolCall {
            id: format!("call-{index}"),
            name: "files__read_file".into(),
            arguments: json!({"path": "README.md"}),
        };
        let id = fixture
            .system
            .store()
            .tool_finished(
                &a,
                &message.id,
                &call,
                json!({"content": "x".repeat(256 * 1024)}),
                "file content",
                "read file",
                None,
            )
            .unwrap();
        ids.push(id);
    }
    // In-flight records are durable protocol facts, not completed result summaries.
    fixture
        .system
        .store()
        .tool_started(
            &a,
            &message.id,
            &daat_locus::model_protocol::AgentToolCall {
                id: "unfinished-call".into(),
                name: "actors__create".into(),
                arguments: json!({"name":"pending"}),
            },
        )
        .unwrap();
    let path = format!("/v1/actors/{a}/messages/{}/results?limit=2", message.id);
    let page = fixture
        .owner(reqwest::Method::GET, &path)
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let text = page.text().await.unwrap();
    assert!(text.len() < 2048 && !text.contains("payload") && !text.contains("arguments"));
    let page: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["call_id"] != "unfinished-call")
    );
    let before = page["next_cursor"].as_i64().unwrap();
    let older = fixture
        .owner(reqwest::Method::GET, &format!("{path}&before={before}"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(older["items"].as_array().unwrap().len(), 1);
    let cookie = fixture.login_cookie().await;
    assert_eq!(
        fixture
            .client
            .get(format!("{}/v1/actors/{a}/results/{}", fixture.url, ids[0]))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture
            .owner(
                reqwest::Method::GET,
                &format!("/v1/actors/{b}/messages/{}/results", message.id)
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .client
            .get(format!("{}/v1/actors/{b}/results/{}", fixture.url, ids[0]))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn temporary_tool_outputs_use_the_same_authenticated_resource_endpoint() {
    let f = Fixture::new().await;
    let original = "temporary tool output\n".repeat(1000);
    let bounded = daat_locus::resources::bound_output(
        &f.system,
        "core",
        "temporary-http",
        &original,
        256,
        None,
    )
    .unwrap();
    let resource = daat_locus::resources::export_all(&f.system, "core")
        .unwrap()
        .into_iter()
        .find(|resource| resource.expires_at_ms.is_some())
        .unwrap();
    let uri = resource.uri("core");
    assert!(bounded.contains(&uri));
    assert_eq!(
        f.client
            .get(format!("{}{}", f.url, uri))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = f.owner(reqwest::Method::GET, &uri).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), original);
    let mut stored = f.system.store().resource("core", &resource.id).unwrap();
    stored.expires_at_ms = Some(chrono::Utc::now().timestamp_millis() - 1);
    f.system.store().register_resource(stored).unwrap();
    let response = f.owner(reqwest::Method::GET, &uri).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
