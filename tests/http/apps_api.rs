//! Owner-only device App management and immutable installation revisions.
use crate::support::Fixture;
use axum::{Json, Router, http::HeaderMap, routing::post};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

#[tokio::test]
async fn apps_require_owner_origin_and_revision_and_never_return_credentials() {
    let fixture = Fixture::new().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/mcp?key=private-query",
        listener.local_addr().unwrap()
    );
    let router = Router::new().route("/mcp", post(|headers: HeaderMap, Json(request): Json<Value>| async move {
        assert_eq!(headers["authorization"], "Bearer app-test-secret");
        let result = match request["method"].as_str().unwrap() {
            "initialize" => json!({"protocolVersion":request["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1"}}),
            "tools/list" => json!({"tools":[{"name":"search","inputSchema":{"type":"object","properties":{},"required":[],"additionalProperties":false}}]}),
            _ => return (StatusCode::ACCEPTED, Json(Value::Null)),
        };
        (StatusCode::OK, Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})))
    }));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let input = json!({"name":"Example","description":"Find records","source":{"transport":"http","url":url,"bearer_token":"app-test-secret"}});
    for path in ["/v1/apps", "/v1/apps/missing"] {
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
    }
    let cookie = fixture.cookie.read().clone();
    let blocked = fixture
        .client
        .post(format!("{}/v1/apps", fixture.url))
        .header("Cookie", cookie.clone())
        .header("x-daat-request", "1")
        .header("Origin", "https://untrusted.example")
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
    let missing_header = fixture
        .client
        .post(format!("{}/v1/apps", fixture.url))
        .header("Cookie", cookie.clone())
        .header("Origin", &fixture.url)
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(missing_header.status(), StatusCode::FORBIDDEN);
    let installed = fixture
        .owner(Method::POST, "/v1/apps")
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(
        installed.status(),
        StatusCode::OK,
        "{}",
        installed.text().await.unwrap()
    );
    let app: Value = installed.json().await.unwrap();
    assert_eq!(app["description"], "Find records");
    assert!(!app.to_string().contains("app-test-secret"));
    assert!(!app.to_string().contains("private-query"));
    let id = app["id"].as_str().unwrap();
    let list: Value = fixture
        .owner(Method::GET, "/v1/apps?limit=1&search=Example")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["items"][0]["id"], id);
    assert!(list["next_cursor"].is_null());
    let detail: Value = fixture
        .owner(Method::GET, &format!("/v1/apps/{id}?tool=search"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        detail["tool"]["input_schema"]["additionalProperties"],
        false
    );
    let closed = fixture
        .owner(Method::POST, &format!("/v1/apps/{id}/state"))
        .json(&json!({"enabled":false,"expected_revision":app["revision"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(closed.status(), StatusCode::OK);
    assert_eq!(closed.json::<Value>().await.unwrap()["enabled"], false);
    assert_eq!(
        fixture
            .owner(Method::POST, &format!("/v1/apps/{id}/state"))
            .json(&json!({"enabled":true,"expected_revision":app["revision"]}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let enabled: Value = fixture
        .owner(Method::GET, "/v1/apps?enabled=true")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(enabled["items"], json!([]));
    server.abort();
}
