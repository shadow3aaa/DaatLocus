use crate::support::Fixture;
use reqwest::{Method, StatusCode};
use serde_json::Value;

#[tokio::test]
async fn virtual_display_observation_is_owner_only_and_does_not_initialize_outputs() {
    let fixture = Fixture::new().await;
    const PATH: &str = "/v1/device/virtual-displays";
    for suffix in ["", "/capabilities", "/unknown/frame"] {
        let response = fixture
            .client
            .get(format!("{}{PATH}{suffix}", fixture.url))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let capabilities: Value = fixture
        .owner(Method::GET, &format!("{PATH}/capabilities"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        capabilities["supported"],
        cfg!(any(windows, target_os = "android"))
    );
    for _ in 0..2 {
        let response = fixture.owner(Method::GET, PATH).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let catalog: Value = response.json().await.unwrap();
        assert_eq!(catalog["displays"], serde_json::json!([]));
        assert_ne!(catalog["state"], "ready");
        let response = fixture
            .owner(Method::GET, &format!("{PATH}/unknown/frame"))
            .send()
            .await
            .unwrap();
        assert!(matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::SERVICE_UNAVAILABLE
        ));
    }
    fixture
        .owner(Method::DELETE, "/v1/account/session")
        .send()
        .await
        .unwrap();
    assert_eq!(
        fixture
            .owner(Method::GET, PATH)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}
