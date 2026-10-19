//! Device preferences use Owner HTTP authorization, never model/native commands.
use crate::support::Fixture;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
const PATH: &str = "/v1/device/computer-use";

#[tokio::test]
async fn gui_settings_require_owner_and_trusted_mutation_origin() {
    let fixture = Fixture::new().await;
    let cookie = fixture.cookie.read().clone();
    let capabilities = format!("{PATH}/capabilities");
    assert_eq!(
        fixture
            .client
            .get(format!("{}{}", fixture.url, capabilities))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let value: Value = fixture
        .owner(Method::GET, &capabilities)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(value["supported"], cfg!(windows));
    for method in [Method::GET, Method::PATCH] {
        let response = fixture
            .client
            .request(method, format!("{}{PATH}", fixture.url))
            .json(&json!({"enabled":false}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let response = fixture
        .client
        .patch(format!("{}{PATH}", fixture.url))
        .header("cookie", &cookie)
        .json(&json!({"enabled":false}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = fixture
        .client
        .patch(format!("{}{PATH}", fixture.url))
        .header("cookie", &cookie)
        .header("origin", "https://untrusted.invalid")
        .header("x-daat-request", "1")
        .json(&json!({"enabled":false}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let value: Value = fixture
        .owner(Method::GET, PATH)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(value["supported"], cfg!(windows));
}

#[cfg(windows)]
#[tokio::test]
async fn gui_display_patch_persists_while_disabled_and_rejects_unsupported_modes() {
    let fixture = Fixture::new().await;
    let response = fixture
        .owner(Method::PATCH, PATH)
        .json(&json!({"enabled":false}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = fixture
        .owner(Method::PATCH, PATH)
        .json(&json!({"display":{"width":2560,"height":1440}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let saved: Value = response.json().await.unwrap();
    assert_eq!(saved["enabled"], false);
    assert_eq!(saved["state"], "disabled");
    assert_eq!(saved["display"], json!({"width":2560,"height":1440}));
    let file = fixture
        .system
        .infrastructure_dir()
        .join("computer-use.json");
    let persisted: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(
        persisted,
        json!({"enabled":false,"display":{"width":2560,"height":1440}})
    );
    for patch in [
        json!({"display":{"width":1920,"height":1440}}),
        json!({"display":{"width":0,"height":0}}),
        json!({"display":{"width":1920}}),
        json!({"other":true}),
    ] {
        let response = fixture
            .owner(Method::PATCH, PATH)
            .json(&patch)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
    let actual: Value = fixture
        .owner(Method::GET, PATH)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(actual["display"], saved["display"]);
    assert_eq!(actual["enabled"], false);
}
