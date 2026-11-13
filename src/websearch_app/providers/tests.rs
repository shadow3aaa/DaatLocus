use super::*;

#[tokio::test]
async fn provider_requests_and_results_follow_each_wire_contract() {
    let cases = [
        (
            Provider::Opencode,
            json!({"providerID":"opencode","results":[{"url":"https://example.com/a","title":"Hosted","content":"Excerpt","time":{"published":0}}]}),
            "Hosted",
            "Excerpt",
        ),
        (
            Provider::Tavily,
            json!({"results":[{"url":"https://example.com/a","title":"Tavily","content":"Evidence"}]}),
            "Tavily",
            "Evidence",
        ),
        (
            Provider::Exa,
            json!({"results":[{"url":"https://example.com/a","title":"Exa","text":"Page text","publishedDate":"2026-11-08"}]}),
            "Exa",
            "Page text",
        ),
        (
            Provider::Brave,
            json!({"type":"search","web":{"results":[{"url":"https://example.com/a","title":"Brave","description":"Description"}]}}),
            "Brave",
            "Description",
        ),
        (
            Provider::Serper,
            json!({"organic":[{"link":"https://example.com/a","title":"Google","snippet":"Snippet"}]}),
            "Google",
            "Snippet",
        ),
        (
            Provider::Firecrawl,
            json!({"success":true,"data":{"web":[{"url":"https://example.com/a","title":"Firecrawl","description":"Crawl"}]}}),
            "Firecrawl",
            "Crawl",
        ),
        (
            Provider::Parallel,
            json!({"results":[{"url":"https://example.com/a","title":"Parallel","excerpts":["First","Second"],"publish_date":"2026-11-08"}]}),
            "Parallel",
            "First\n\nSecond",
        ),
    ];
    for (provider, fixture, title, content) in cases {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/search", listener.local_addr().unwrap());
        let route = axum::Router::new().route(
            "/search",
            axum::routing::any(move |request: axum::extract::Request| {
                let fixture = fixture.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    assert!(!parts.uri.to_string().contains("fixture-key"));
                    let bytes = axum::body::to_bytes(body, 16384).await.unwrap();
                    let auth_header = match provider {
                        Provider::Brave => "x-subscription-token",
                        Provider::Exa | Provider::Serper | Provider::Parallel => "x-api-key",
                        _ => "authorization",
                    };
                    assert_eq!(
                        parts.headers[auth_header],
                        if auth_header == "authorization" {
                            "Bearer fixture-key"
                        } else {
                            "fixture-key"
                        }
                    );
                    if provider == Provider::Brave {
                        assert_eq!(parts.method, Method::GET);
                        let url =
                            url::Url::parse(&format!("http://localhost{}", parts.uri)).unwrap();
                        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
                        assert_eq!(pairs["q"], "中文 & rust");
                        assert_eq!(pairs["count"], "3");
                    } else {
                        assert_eq!(parts.method, Method::POST);
                        let body: Value = serde_json::from_slice(&bytes).unwrap();
                        match provider {
                            Provider::Opencode => {
                                assert_eq!(
                                    body,
                                    json!({"query":"中文 & rust","providerID":"opencode"})
                                );
                                assert_eq!(parts.headers["x-org-id"], "wrk_fixture");
                            }
                            Provider::Tavily => {
                                assert_eq!(body["query"], "中文 & rust");
                                assert_eq!(body["max_results"], 3);
                                assert_eq!(body["search_depth"], "basic");
                            }
                            Provider::Exa => {
                                assert_eq!(body["query"], "中文 & rust");
                                assert_eq!(body["numResults"], 3);
                                assert_eq!(body["contents"]["text"]["maxCharacters"], 2000);
                            }
                            Provider::Serper => {
                                assert_eq!(body["q"], "中文 & rust");
                                assert_eq!(body["num"], 3);
                            }
                            Provider::Firecrawl => {
                                assert_eq!(body["query"], "中文 & rust");
                                assert_eq!(body["limit"], 3);
                                assert_eq!(body["sources"], json!(["web"]));
                            }
                            Provider::Parallel => {
                                assert_eq!(body["search_queries"], json!(["中文 & rust"]));
                                assert_eq!(body["advanced_settings"]["max_results"], 3);
                            }
                            _ => unreachable!(),
                        }
                    }
                    axum::Json(fixture)
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, route).await.unwrap();
        });
        let client = crate::http_client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let request = request(
            &client,
            &endpoint,
            provider,
            "fixture-key",
            Some("wrk_fixture"),
            "中文 & rust",
            3,
        )
        .unwrap();
        let body = read_response(client.execute(request).await.unwrap())
            .await
            .unwrap();
        let results = decode(provider, body).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/a");
        assert_eq!(results[0].title, title);
        assert_eq!(results[0].content, content);
        if provider == Provider::Opencode {
            assert_eq!(
                results[0].published_at.as_deref(),
                Some("1970-01-01T00:00:00+00:00")
            );
        }
        server.abort();
        let _ = server.await;
    }
}
#[test]
fn invalid_provider_envelopes_are_not_successful_empty_searches() {
    for provider in Provider::ALL {
        assert!(decode(provider, json!({"error":"private diagnostics"})).is_err());
    }
    assert!(
        decode(
            Provider::Opencode,
            json!({"providerID":"other","results":[]})
        )
        .is_err()
    );
    assert!(
        decode(
            Provider::Firecrawl,
            json!({"success":false,"data":{"web":[]}})
        )
        .is_err()
    );
    assert!(
        decode(
            Provider::Tavily,
            json!({"results":[{"title":"Missing URL"}]})
        )
        .is_err()
    );
    assert!(
        decode(Provider::Tavily, json!({"results":[]}))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn provider_errors_and_oversized_bodies_are_bounded_and_hide_private_diagnostics() {
    for (status, body) in [
        (
            StatusCode::TOO_MANY_REQUESTS,
            "private-key private diagnostic".to_owned(),
        ),
        (StatusCode::OK, "not JSON".to_owned()),
        (StatusCode::OK, "x".repeat(MAX_RESPONSE_BYTES + 1)),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let route = axum::Router::new().route(
            "/",
            axum::routing::get(move || {
                let body = body.clone();
                async move { (status, body) }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, route).await.unwrap();
        });
        let client = crate::http_client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let error = read_response(client.get(endpoint).send().await.unwrap())
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(!error.contains("private-key"));
        assert!(!error.contains("private diagnostic"));
        server.abort();
        let _ = server.await;
    }
}
#[test]
fn redirect_credentials_stay_on_their_original_origin() {
    let client = crate::http_client::builder().build().unwrap();
    let make = || {
        request(
            &client,
            Provider::Opencode.endpoint(),
            Provider::Opencode,
            "secret",
            Some("wrk_private"),
            "query",
            8,
        )
        .unwrap()
    };
    let mut headers = HeaderMap::new();
    headers.insert("location", HeaderValue::from_static("/console/gateway"));
    let same = redirected(make(), StatusCode::TEMPORARY_REDIRECT, &headers).unwrap();
    assert_eq!(same.headers()["authorization"], "Bearer secret");
    headers.insert(
        "location",
        HeaderValue::from_static("https://gateway.example.com/search"),
    );
    let other = redirected(make(), StatusCode::TEMPORARY_REDIRECT, &headers).unwrap();
    assert!(!other.headers().contains_key("authorization"));
    assert!(!other.headers().contains_key("x-org-id"));
    assert_eq!(other.method(), Method::POST);
    let get = redirected(make(), StatusCode::SEE_OTHER, &headers).unwrap();
    assert_eq!(get.method(), Method::GET);
    assert!(get.body().is_none());
    for target in [
        "http://example.com",
        "https://user:password@example.com",
        "file:///private",
    ] {
        headers.insert("location", HeaderValue::from_str(target).unwrap());
        assert!(redirected(make(), StatusCode::TEMPORARY_REDIRECT, &headers).is_err());
    }
}
#[tokio::test]
async fn transport_rejects_nonpublic_targets_before_sending_credentials() {
    let client = crate::http_client::builder().build().unwrap();
    for target in [
        "https://127.0.0.1/search",
        "https://169.254.169.254/metadata",
        "https://router.local/search",
    ] {
        let request = request(
            &client,
            target,
            Provider::Tavily,
            "private-key",
            None,
            "query",
            8,
        )
        .unwrap();
        assert!(send(request).await.is_err());
    }
}

#[tokio::test]
async fn native_probe_only_falls_back_on_confirmed_unsupported_capabilities() {
    for (status, body, unsupported) in [
        (
            StatusCode::BAD_REQUEST,
            json!({"error":{"message":"Unsupported tool type: web_search; private-key"}}),
            true,
        ),
        (
            StatusCode::BAD_REQUEST,
            json!({"error":{"message":"Tools are not supported by this gateway"}}),
            true,
        ),
        (
            StatusCode::NOT_FOUND,
            json!({"error":{"message":"Not Found"}}),
            true,
        ),
        (
            StatusCode::NOT_FOUND,
            json!({"error":{"code":"model_not_found","message":"Model missing"}}),
            false,
        ),
        (
            StatusCode::BAD_REQUEST,
            json!({"error":{"message":"Query too long"}}),
            false,
        ),
        (
            StatusCode::UNAUTHORIZED,
            json!({"error":{"message":"Bad API key"}}),
            false,
        ),
        (
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error":{"message":"Rate limited"}}),
            false,
        ),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/search", listener.local_addr().unwrap());
        let route = axum::Router::new().route(
            "/search",
            axum::routing::get(move || async move { (status, axum::Json(body)) }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, route).await.unwrap();
        });
        let client = crate::http_client::builder().no_proxy().build().unwrap();
        let response = client.get(endpoint).send().await.unwrap();
        let error = read_response_mode(response, true).await.unwrap_err();
        assert_eq!(
            native_search_unsupported(&error),
            unsupported,
            "HTTP {status}"
        );
        assert!(!error.to_string().contains("private-key"));
        server.abort();
        let _ = server.await;
    }
}
