use super::*;

fn fixture(kind: Kind, base_url: &str) -> (ModelConfig, Config) {
    let model = ModelConfig {
        provider: "test".into(),
        model_id: match kind {
            Kind::Openai => "gpt-6",
            Kind::Deepseek => "deepseek-flash",
        }
        .into(),
        ..ModelConfig::default()
    };
    let mut config = Config::default();
    config.providers.insert(
        "test".into(),
        match kind {
            Kind::Openai => ProviderConfig::Openai {
                api_key: "example-secret".into(),
                base_url: Some(base_url.into()),
            },
            Kind::Deepseek => ProviderConfig::OpenaiCompatible {
                api_key: "example-secret".into(),
                base_url: base_url.into(),
            },
        },
    );
    (model, config)
}

#[test]
fn native_search_auto_tries_compatible_gateways_and_obeys_explicit_overrides() {
    for (kind, base) in [
        (Kind::Openai, "https://api.openai.com/v1"),
        (Kind::Deepseek, "https://api.deepseek.com"),
    ] {
        let (mut model, config) = fixture(kind, base);
        assert!(selected(&model, &config).is_some());
        model.native_web_search = Some(false);
        assert!(selected(&model, &config).is_none());
    }
    for (kind, base) in [
        (Kind::Openai, "https://relay.example/v1"),
        (Kind::Deepseek, "https://relay.example/v1"),
        (Kind::Openai, "https://api.openai.com.evil/v1"),
        (Kind::Deepseek, "https://api.deepseek.com.evil"),
    ] {
        let (mut model, config) = fixture(kind, base);
        assert!(selected(&model, &config).is_some(), "auto: {base}");
        model.native_web_search = Some(false);
        assert!(selected(&model, &config).is_none(), "disabled: {base}");
        model.native_web_search = Some(true);
        assert!(
            selected(&model, &config).is_some(),
            "explicit choice: {base}"
        );
    }
    let (mut model, config) = fixture(Kind::Openai, "https://api.openai.com/v1");
    model.model_id = "some-unsupported-model".into();
    assert!(selected(&model, &config).is_none());
    for id in [
        "gpt-5-search-api",
        "gpt-4o-search-preview",
        "gpt-4o-mini-search-preview",
        "o4-mini",
    ] {
        model.model_id = id.into();
        model.native_web_search = Some(true);
        assert!(
            selected(&model, &config).is_none(),
            "{id} does not support Responses web_search"
        );
    }
    let (mut model, config) = fixture(Kind::Deepseek, "https://relay.example/v1");
    model.model_id = "deepseek/deepseek-chat".into();
    assert!(selected(&model, &config).is_some());
    let (mut model, config) = fixture(Kind::Openai, "https://relay.example/v1");
    model.model_id = "openai/gpt-6".into();
    assert!(selected(&model, &config).is_some());
}

#[test]
fn confirmed_unsupported_is_cached_per_endpoint_model_and_credential() {
    let (mut model, mut config) = fixture(Kind::Deepseek, "https://unique-probe.example/v1");
    let native = selected(&model, &config).unwrap();
    assert!(native.remember_unsupported());
    assert!(selected(&model, &config).is_none());
    model.native_web_search = Some(true);
    let forced = selected(&model, &config).unwrap();
    assert!(!forced.remember_unsupported());
    model.native_web_search = None;
    model.model_id = "deepseek-reasoner".into();
    assert!(selected(&model, &config).is_some());
    model.model_id = "deepseek-flash".into();
    config.providers.insert(
        "test".into(),
        ProviderConfig::OpenaiCompatible {
            api_key: "replacement-secret".into(),
            base_url: "https://unique-probe.example/v1".into(),
        },
    );
    assert!(selected(&model, &config).is_some());
}

#[test]
fn native_requests_use_the_providers_real_endpoint_and_never_embed_keys_in_urls() {
    let client = crate::http_client::builder().build().unwrap();
    for (kind, base, endpoint) in [
        (
            Kind::Openai,
            "https://api.openai.com/v1",
            "https://api.openai.com/v1/responses",
        ),
        (
            Kind::Deepseek,
            "https://api.deepseek.com",
            "https://api.deepseek.com/v1/tools/search",
        ),
        (
            Kind::Deepseek,
            "https://api.deepseek.com/v1",
            "https://api.deepseek.com/v1/tools/search",
        ),
    ] {
        let (model, config) = fixture(kind, base);
        let native = selected(&model, &config).unwrap();
        let request = request(&client, &native, "Rust release", 5).unwrap();
        assert_eq!(request.url().as_str(), endpoint);
        assert!(!request.url().as_str().contains("example-secret"));
        assert_eq!(request.headers()["authorization"], "Bearer example-secret");
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        match kind {
            Kind::Openai => {
                assert_eq!(body["model"], "gpt-6");
                assert_eq!(body["tools"], json!([{"type":"web_search"}]));
                assert_eq!(body["tool_choice"], "required");
                assert_eq!(body["include"], json!(["web_search_call.action.sources"]));
                assert_eq!(body["store"], false);
            }
            Kind::Deepseek => assert_eq!(
                body,
                json!({"query":"Rust release","count":5,"need_content":false})
            ),
        }
    }
}

#[test]
fn deepseek_returns_actual_summary_and_publication_date_without_generated_citations() {
    let response = deepseek_response(json!({"results":[{"title":"Release notes","url":"https://example.org/release","snippet":"Short excerpt","summary":"Actual longer summary","date":"2026-11-08T00:00:00Z"}]})).unwrap();
    assert_eq!(response.provider, "deepseek");
    assert_eq!(response.results[0].content, "Actual longer summary");
    assert_eq!(
        response.results[0].published_at.as_deref(),
        Some("2026-11-08T00:00:00Z")
    );
    assert!(response.answer.is_none());
    assert!(response.citations.is_empty());
    assert!(
        deepseek_response(json!({"results":[]}))
            .unwrap()
            .results
            .is_empty()
    );
    assert!(deepseek_response(json!({"error":"rate limited"})).is_err());
    assert!(deepseek_response(json!({"results":[{"title":"Missing URL"}]})).is_err());
}

#[test]
fn openai_uses_only_verified_search_calls_and_links_annotations() {
    let response=openai_response(json!({"status":"completed","output":[
        {"type":"web_search_call","status":"completed","action":{"sources":[{"url":"https://example.org/article","title":"Original title"}]}},
        {"type":"message","content":[{"type":"output_text","text":"New info [1]","annotations":[{"type":"url_citation","url":"https://example.org/article","title":"Original title","start_index":9,"end_index":12}]}]},
    ]})).unwrap();
    assert_eq!(response.answer.as_deref(), Some("New info [1]"));
    assert_eq!(response.citations[0].start_index, 9);
    assert_eq!(response.citations[0].url, "https://example.org/article");
    assert_eq!(response.results[0].url, "https://example.org/article");
    assert!(search_skipped(&openai_response(json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"Unverified reply"}]}]})).err().unwrap()));
    assert!(openai_response(json!({"status":"incomplete","output":[]})).is_err());
    let empty=openai_response(json!({"status":"completed","output":[{"type":"web_search_call","status":"completed"},{"type":"message","content":[{"type":"output_text","text":"Uncited answer"}]}]})).unwrap();
    assert!(empty.results.is_empty());
    assert!(empty.answer.is_none());
}

#[test]
fn filtered_sources_do_not_leave_an_uncited_or_secret_bearing_answer() {
    let body = json!({"status":"completed","output":[
        {"type":"web_search_call","status":"completed","action":{"sources":[{"url":"https://example.org/public","title":"Public"}]}},
        {"type":"message","content":[{"type":"output_text","text":"Answer [1]","annotations":[
            {"type":"url_citation","url":"https://example.org/?key=example-secret","title":"Secret","start_index":7,"end_index":10}]}]}
    ]});
    let mut response = openai_response(body).unwrap();
    assert!(response.answer.is_some());
    sanitize_response(&mut response, "example-secret", 1);
    assert!(response.answer.is_none());
    assert!(response.citations.is_empty());
    assert_eq!(response.results[0].url, "https://example.org/public");

    let mut response = openai_response(json!({"status":"completed","output":[
        {"type":"web_search_call","status":"completed","action":{"sources":[{"url":"https://example.org/first"}]}},
        {"type":"message","content":[{"type":"output_text","text":"Answer [1]","annotations":[
            {"type":"url_citation","url":"https://example.org/second","title":"Title example-secret","start_index":7,"end_index":10}]}]}
    ]})).unwrap();
    sanitize_response(&mut response, "example-secret", 1);
    assert_eq!(response.results.len(), 1);
    assert_eq!(response.citations.len(), 1);
    assert_eq!(response.answer.as_deref(), Some("Answer [1]"));
    assert!(!response.citations[0].title.contains("example-secret"));
    response.answer = Some("Answer example-secret [1]".into());
    sanitize_response(&mut response, "example-secret", 1);
    assert!(response.answer.is_none());
    assert!(response.citations.is_empty());
}
