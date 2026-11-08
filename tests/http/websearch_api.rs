use crate::support::Fixture;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

#[tokio::test]
async fn websearch_settings_require_owner_origin_and_never_return_credentials() {
    let fixture = Fixture::new().await;
    let path = "/v1/config/websearch";
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
    let response = fixture
        .owner(Method::PATCH, path)
        .json(&json!({"provider":"tavily","api_key":"private-search-key"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        !response
            .text()
            .await
            .unwrap()
            .contains("private-search-key")
    );
    let response = fixture
        .client
        .patch(format!("{}{path}", fixture.url))
        .header("cookie", fixture.cookie.read().clone())
        .header("origin", "https://untrusted.example")
        .header("x-daat-request", "1")
        .json(&json!({"provider":"brave"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let view: Value = fixture
        .owner(Method::GET, path)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view["provider"], "tavily");
    assert_eq!(
        view["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == "tavily")
            .unwrap()["credential_configured"],
        true
    );
    assert!(!view.to_string().contains("private-search-key"));
    let response = fixture
        .owner(Method::PATCH, path)
        .json(&json!({"provider":"exa"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let settings = fixture.system.websearch_settings().get();
    assert_eq!(
        settings.keys[&daat_locus::websearch_app::settings::Provider::Tavily],
        "private-search-key"
    );
    assert_eq!(
        fixture.system.redact_secrets("failure private-search-key"),
        "failure [redacted]"
    );
    assert!(
        !settings
            .keys
            .contains_key(&daat_locus::websearch_app::settings::Provider::Exa)
    );
    let reopened = daat_locus::websearch_app::settings::Store::open(
        fixture.system.infrastructure_dir().join("websearch.json"),
    )
    .unwrap();
    assert_eq!(
        reopened.get().provider,
        Some(daat_locus::websearch_app::settings::Provider::Exa)
    );
}
