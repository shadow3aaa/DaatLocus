use super::tests::jwt;
use super::*;
use std::sync::atomic::AtomicUsize;

fn tokens(subject: &str, account: &str, expired: bool, revision: &str) -> CodexOAuthTokens {
    CodexOAuthTokens {
        id_token: jwt(&json!({"sub": subject, "revision": revision})),
        access_token: jwt(&json!({
            "exp": if expired { 1 } else { 4_102_444_800_i64 },
            "revision": revision,
        })),
        refresh_token: format!("refresh-{revision}"),
        account_id: Some(account.into()),
        last_refresh_at_ms: 0,
    }
}

async fn write_source(path: &Path, tokens: &CodexOAuthTokens) -> Vec<u8> {
    let bytes = serde_json::to_vec(&json!({"tokens": tokens})).unwrap();
    tokio::fs::write(path, &bytes).await.unwrap();
    bytes
}

struct RefreshServer {
    endpoint: String,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl RefreshServer {
    async fn rejecting() -> Self {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}/oauth/token", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let captured = requests.clone();
        let router = axum::Router::new().route(
            "/oauth/token",
            axum::routing::post(move || {
                let captured = captured.clone();
                async move {
                    captured.fetch_add(1, Ordering::SeqCst);
                    (
                        axum::http::StatusCode::UNAUTHORIZED,
                        axum::Json(json!({"error": {"code": "refresh_token_reused"}})),
                    )
                }
            }),
        );
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            endpoint,
            requests,
            task,
        }
    }
}

impl Drop for RefreshServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn isolated_client(auth_file: PathBuf, server: &RefreshServer) -> CodexClient {
    let mut client = CodexClient::new(auth_file, None, &crate::config::ModelConfig::default());
    // A regression must fail against loopback rather than contact the real issuer.
    client.auth_client = crate::http_client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(&server.endpoint).unwrap())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    client
}

fn assert_authentication_failure(error: &miette::Report) {
    let failure = error
        .downcast_ref::<super::super::ModelRequestFailure>()
        .unwrap();
    assert_eq!(failure.code(), "model_authentication_failed");
    assert!(!crate::core::should_retry_agent_turn_error(error));
}

#[tokio::test]
async fn imported_auth_follows_source_rotation_even_while_client_access_is_cached() {
    let temp = tempfile::tempdir().unwrap();
    let _profile = crate::daat_locus_paths::DaatLocusHomeOverride::set(temp.path().into()).await;
    let source = temp.path().join("auth.json");
    write_source(&source, &tokens("owner", "account", true, "old")).await;
    let imported = import_codex_cli_oauth_file(&source).await.unwrap();
    let initial_snapshot = tokio::fs::read(&imported).await.unwrap();
    let server = RefreshServer::rejecting().await;
    let client = isolated_client(imported.clone(), &server);

    for revision in ["rotated", "rotated-again"] {
        let current = tokens("owner", "account", false, revision);
        let original = write_source(&source, &current).await;
        client.ensure_auth().await.unwrap();
        assert_eq!(
            client.cached.lock().await.as_ref().unwrap().access_token,
            current.access_token
        );
        assert_eq!(client.inner.lock().await.api_key, current.access_token);
        assert_eq!(tokio::fs::read(&source).await.unwrap(), original);
        assert_eq!(tokio::fs::read(&imported).await.unwrap(), initial_snapshot);
    }
}

#[tokio::test]
async fn expired_imported_credentials_never_consume_the_source_refresh_token() {
    let temp = tempfile::tempdir().unwrap();
    let _profile = crate::daat_locus_paths::DaatLocusHomeOverride::set(temp.path().into()).await;
    let source = temp.path().join("auth.json");
    let original = write_source(&source, &tokens("owner", "account", true, "old")).await;
    let imported = import_codex_cli_oauth_file(&source).await.unwrap();
    let server = RefreshServer::rejecting().await;
    let client = crate::http_client::builder().no_proxy().build().unwrap();

    for source_exists in [true, false] {
        if !source_exists {
            tokio::fs::remove_file(&source).await.unwrap();
        }
        let error = codex_oauth_access_with_refresh_endpoint(&imported, &client, &server.endpoint)
            .await
            .unwrap_err();
        assert_authentication_failure(&error);
        assert_eq!(server.requests.load(Ordering::SeqCst), 0);
        if source_exists {
            assert_eq!(tokio::fs::read(&source).await.unwrap(), original);
        }
    }
}

#[tokio::test]
async fn imported_auth_rejects_source_account_changes_and_logout_despite_cached_access() {
    let temp = tempfile::tempdir().unwrap();
    let _profile = crate::daat_locus_paths::DaatLocusHomeOverride::set(temp.path().into()).await;
    let source = temp.path().join("auth.json");
    write_source(&source, &tokens("owner", "account", false, "initial")).await;
    let imported = import_codex_cli_oauth_file(&source).await.unwrap();
    let server = RefreshServer::rejecting().await;
    let client = isolated_client(imported, &server);
    client.ensure_auth().await.unwrap();

    for changed in [
        tokens("other-owner", "account", false, "other-user"),
        tokens("owner", "other-account", false, "other-account"),
    ] {
        write_source(&source, &changed).await;
        let error = client.ensure_auth().await.unwrap_err();
        assert_authentication_failure(&error);
    }
    tokio::fs::write(&source, br#"{"tokens":null}"#)
        .await
        .unwrap();
    assert_authentication_failure(&client.ensure_auth().await.unwrap_err());
}

#[tokio::test]
async fn reused_legacy_refresh_token_is_an_authentication_failure_and_preserves_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let auth_file = temp.path().join("legacy.json");
    write_codex_oauth_tokens(&auth_file, &tokens("owner", "account", true, "legacy"))
        .await
        .unwrap();
    let original = tokio::fs::read(&auth_file).await.unwrap();
    let server = RefreshServer::rejecting().await;
    let client = crate::http_client::builder().no_proxy().build().unwrap();
    let error = codex_oauth_access_with_refresh_endpoint(&auth_file, &client, &server.endpoint)
        .await
        .unwrap_err();
    assert_authentication_failure(&error);
    assert!(error.to_string().contains("refresh_token_reused"));
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    assert_eq!(tokio::fs::read(&auth_file).await.unwrap(), original);
}
