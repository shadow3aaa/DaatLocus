//! Account HTTP contracts.
use crate::support::Fixture;
use daat_locus_client_protocol::BootstrapRequest;
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn persistent_sessions_restore_after_restart_but_logout_and_password_changes_stay_revoked() {
    let mut fixture = Fixture::new().await;
    let cookie = fixture.cookie.read().clone();
    let access = fixture
        .owner(reqwest::Method::GET, "/v1/access")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let persisted = std::fs::read_to_string(
        fixture
            .system
            .account_credentials_dir()
            .join("browser-sessions.json"),
    )
    .unwrap();
    assert!(
        !persisted.contains(cookie.split_once('=').unwrap().1),
        "never persist a usable cookie secret"
    );
    assert!(
        !persisted.contains(fixture.password.read().as_str()),
        "never persist the login password"
    );
    fixture.restart_service().await;
    let restored = fixture
        .owner(reqwest::Method::GET, "/v1/access")
        .send()
        .await
        .unwrap();
    assert_eq!(restored.status(), StatusCode::OK);
    assert_eq!(
        restored.json::<Value>().await.unwrap()["session_id"],
        access["session_id"]
    );
    for (origin, marker) in [
        (None, true),
        (Some("https://attacker.invalid"), true),
        (Some(fixture.url.as_str()), false),
    ] {
        let mut request = fixture
            .client
            .post(format!("{}/v1/account/session", fixture.url))
            .header("cookie", &cookie);
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        if marker {
            request = request.header("x-daat-request", "1");
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    let renewed = fixture
        .owner(reqwest::Method::POST, "/v1/account/session")
        .send()
        .await
        .unwrap();
    assert_eq!(renewed.status(), StatusCode::NO_CONTENT);
    let renewed = renewed.headers()["set-cookie"].to_str().unwrap();
    let max_age = renewed
        .split(';')
        .find_map(|part| part.trim().strip_prefix("Max-Age="))
        .unwrap()
        .parse::<i64>()
        .unwrap();
    assert!(max_age >= 29 * 24 * 60 * 60);
    assert!(renewed.contains("HttpOnly") && renewed.contains("SameSite=Strict"));
    assert_eq!(
        fixture
            .owner(reqwest::Method::DELETE, "/v1/account/session")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    fixture.restart_service().await;
    assert_eq!(
        fixture
            .owner(reqwest::Method::POST, "/v1/account/session")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    fixture.login_cookie().await;
    fixture
        .change_password("changed isolated fixture password")
        .await;
    fixture.restart_service().await;
    assert_eq!(
        fixture
            .owner(reqwest::Method::POST, "/v1/account/session")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    fixture.login_cookie().await;
    assert_eq!(
        fixture
            .owner(reqwest::Method::GET, "/v1/access")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn account_registration_login_replay_and_profile_boundaries_are_enforced() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
    let ready = Fixture::new().await;
    let fresh = Fixture::with_config_and_account(ready.system.config(), false).await;
    assert_eq!(
        fresh
            .owner(reqwest::Method::GET, "/v1/actors")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let client =
        daat_locus_auth::RegistrationClient::start(b"new isolated account password".to_vec())
            .unwrap();
    let body = json!({"name":"Fresh_Account","request":B64.encode(client.request())});
    assert_eq!(
        fresh
            .client
            .post(format!("{}/v1/account/registration/start", fresh.url))
            .header("origin", "https://attacker.invalid")
            .header("x-daat-request", "1")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let response = fresh
        .owner(reqwest::Method::POST, "/v1/account/registration/start")
        .json(&body)
        .send()
        .await
        .unwrap();
    let response: Value = if cfg!(target_os = "macos") {
        // HTTP cannot prove the macOS socket owner. Keep that rejection and
        // exercise the same native OS-owner channel used by the client.
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let native = fresh
            .native_bootstrap(BootstrapRequest::RegisterStart {
                name: "Fresh_Account".into(),
                request: B64.encode(client.request()),
            })
            .await;
        assert_eq!(native.status, 200, "{}", native.body);
        native.body
    } else {
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            response.text().await.unwrap()
        );
        response.json().await.unwrap()
    };
    assert_eq!(response["name"], "fresh_account");
    let upload = client
        .finish(&B64.decode(response["response"].as_str().unwrap()).unwrap())
        .unwrap();
    let finish = json!({"id":response["id"],"message":B64.encode(upload)});
    let status = if cfg!(target_os = "macos") {
        fresh
            .native_bootstrap(BootstrapRequest::RegisterFinish {
                id: finish["id"].as_str().unwrap().into(),
                message: finish["message"].as_str().unwrap().into(),
            })
            .await
            .status
    } else {
        fresh
            .owner(reqwest::Method::POST, "/v1/account/registration/finish")
            .json(&finish)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    };
    assert_eq!(status, 204);
    assert_eq!(
        fresh
            .owner(reqwest::Method::POST, "/v1/account/registration/finish")
            .json(&finish)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for password in [
        b"wrong password".as_slice(),
        b"new isolated account password".as_slice(),
    ] {
        let client = daat_locus_auth::LoginClient::start(password.to_vec()).unwrap();
        let response: Value = fresh
            .owner(reqwest::Method::POST, "/v1/account/login/start")
            .json(&json!({"name":"fresh_account","request":B64.encode(client.request())}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let finish = client.finish(
            &B64.decode(response["response"].as_str().unwrap()).unwrap(),
            &B64.decode(response["context"].as_str().unwrap()).unwrap(),
        );
        if password == b"wrong password" {
            assert!(finish.is_err());
            continue;
        }
        let message = json!({"id":response["id"],"message":B64.encode(finish.unwrap())});
        let logged = fresh
            .owner(reqwest::Method::POST, "/v1/account/login/finish")
            .json(&message)
            .send()
            .await
            .unwrap();
        assert_eq!(logged.status(), StatusCode::NO_CONTENT);
        let cookie = logged.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        assert_eq!(
            fresh
                .owner(reqwest::Method::POST, "/v1/account/login/finish")
                .json(&message)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let access: Value = fresh
            .client
            .get(format!("{}/v1/access", fresh.url))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(access["account"]["name"], "fresh_account");
        assert_eq!(
            ready
                .client
                .get(format!("{}/v1/actors", ready.url))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    for path in ["/v1/auth/tokens", "/v1/auth/session"] {
        assert_eq!(
            ready
                .owner(reqwest::Method::GET, path)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        ready
            .client
            .get(format!("{}/v1/actors", ready.url))
            .header("authorization", "Bearer retired-credential")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn browser_cache_marker_is_stable_per_login_and_cannot_authenticate() {
    let fixture = Fixture::new().await;
    let cookie = fixture.login_cookie().await;
    let access = fixture
        .client
        .get(format!("{}/v1/access", fixture.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let marker = access["session_id"].as_str().unwrap();
    assert!(!cookie.contains(marker));
    assert_ne!(marker, fixture.password.read().as_str());
    let again = fixture
        .client
        .get(format!("{}/v1/access", fixture.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(again["session_id"], marker);
    assert_eq!(
        fixture
            .client
            .get(format!("{}/v1/access", fixture.url))
            .header("cookie", format!("daat_owner={marker}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn account_browser_sessions_enforce_csrf_logout_and_password_revision() {
    let fixture = Fixture::new().await;
    for (origin, marker, status) in [
        (None, false, StatusCode::FORBIDDEN),
        (
            Some("https://attacker.invalid"),
            true,
            StatusCode::FORBIDDEN,
        ),
        (Some(fixture.url.as_str()), false, StatusCode::FORBIDDEN),
    ] {
        let mut request = fixture
            .client
            .post(format!("{}/v1/account/login/start", fixture.url))
            .json(&json!({"name":"fixture","request":base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD,daat_locus_auth::LoginClient::start(b"isolated fixture password".to_vec()).unwrap().request())}));
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        if marker {
            request = request.header("x-daat-request", "1");
        }
        assert_eq!(request.send().await.unwrap().status(), status);
    }
    let cookie = fixture.login_cookie().await;
    let access = fixture
        .client
        .get(format!("{}/v1/access", fixture.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(access["kind"], "owner");
    for (origin, marker) in [
        (None, true),
        (Some("https://attacker.invalid"), true),
        (Some(fixture.url.as_str()), false),
    ] {
        let mut request = fixture
            .client
            .post(format!("{}/v1/core/messages", fixture.url))
            .header("cookie", &cookie)
            .json(&json!({"content":"browser mutation", "attachments":[]}));
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        if marker {
            request = request.header("x-daat-request", "1");
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        fixture
            .client
            .post(format!("{}/v1/core/messages", fixture.url))
            .header("cookie", &cookie)
            .header("origin", &fixture.url)
            .header("x-daat-request", "1")
            .json(&json!({"content":"browser mutation", "attachments":[]}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        fixture
            .client
            .post(format!("{}/v1/core/messages", fixture.url))
            .header("cookie", &cookie)
            .header("authorization", "Bearer invalid")
            .json(&json!({"content":"invalid Bearer cannot bypass CSRF", "attachments":[]}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let logout = fixture
        .client
        .delete(format!("{}/v1/account/session", fixture.url))
        .header("cookie", &cookie)
        .header("origin", &fixture.url)
        .header("x-daat-request", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    let cleared = logout
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect::<Vec<_>>();
    assert!(
        cleared
            .iter()
            .any(|value| value.starts_with("daat_account_")
                && value.contains("Max-Age=0")
                && value.contains("Path=/v1")
                && value.contains("HttpOnly")
                && value.contains("SameSite=Strict"))
    );
    assert_eq!(
        fixture
            .client
            .get(format!("{}/v1/access", fixture.url))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let cookie = fixture.login_cookie().await;
    fixture
        .change_password("updated isolated fixture password")
        .await;
    assert_eq!(
        fixture
            .client
            .get(format!("{}/v1/access", fixture.url))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn login_errors_distinguish_discovery_failure_and_expired_attempts() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
    let f = Fixture::with_config_and_account(Default::default(), false).await;
    let mut settings = f.stage.host.settings();

    f.stage.host.save_settings(settings.clone()).unwrap();
    let client = daat_locus_auth::LoginClient::start(b"never sent in HTTP".to_vec()).unwrap();
    let response = f
        .native_bootstrap(BootstrapRequest::LoginStart {
            name: "absent-account".into(),
            request: B64.encode(client.request()),
            exclude: vec![],
        })
        .await;
    assert_eq!(response.status, 503);
    let error = response.body;
    assert_eq!(error["code"], "login_connection_required");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    settings.mode = daat_locus::stage::host::Connectivity::Relay;
    settings.relay_origin = Some(format!("wss://{}", listener.local_addr().unwrap()));
    drop(listener);
    f.stage.host.save_settings(settings).unwrap();
    let response = f
        .native_bootstrap(BootstrapRequest::LoginStart {
            name: "absent-account".into(),
            request: B64.encode(client.request()),
            exclude: vec![],
        })
        .await;
    assert_eq!(response.status, 503);
    let error = response.body;
    assert_eq!(error["code"], "login_relay_unavailable");
    for forbidden in [
        "never sent in HTTP",
        "127.0.0.1",
        "wss://",
        "absent-account",
    ] {
        assert!(!error.to_string().contains(forbidden));
    }
    let response = f
        .owner(reqwest::Method::POST, "/v1/account/login/finish")
        .json(&json!({"id":"expired","message":""}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.json::<Value>().await.unwrap()["code"],
        "login_expired"
    );
    f.stage.stop().await;
    f.system.shutdown().await;
}

#[tokio::test]
async fn invalid_login_proof_has_a_credential_error_without_changing_replay_status() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
    let f = Fixture::new().await;
    let client = daat_locus_auth::LoginClient::start(b"wrong password".to_vec()).unwrap();
    let start: Value = f
        .owner(reqwest::Method::POST, "/v1/account/login/start")
        .json(&json!({"name":"fixture","request":B64.encode(client.request())}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let finish = json!({"id":start["id"],"message":B64.encode(b"invalid proof")});
    let response = f
        .owner(reqwest::Method::POST, "/v1/account/login/finish")
        .json(&finish)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.json::<Value>().await.unwrap()["code"],
        "login_invalid_credentials"
    );
    let response = f
        .owner(reqwest::Method::POST, "/v1/account/login/finish")
        .json(&finish)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.json::<Value>().await.unwrap()["code"],
        "login_expired"
    );
}
