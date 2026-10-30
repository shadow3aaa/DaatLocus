use crate::support::Fixture;
use daat_locus::{
    actors::{ActorEvent, MessageStatus},
    model_protocol::AgentToolCall,
};
use serde_json::{Value, json};

#[tokio::test]
async fn contacts_report_the_effective_model_after_preferences_and_model_removal() {
    let fixture = Fixture::new().await;
    let child = fixture.child("Athena").await;
    let mut config = fixture.system.config();
    config
        .models
        .insert("specialist".into(), config.models["mock"].clone());
    fixture.system.update_config(config.clone()).await.unwrap();
    let changed = fixture
        .owner(reqwest::Method::PATCH, &format!("/v1/actors/{child}"))
        .json(&json!({"model":"specialist"}))
        .send()
        .await
        .unwrap();
    assert!(changed.status().is_success());
    let read = || fixture.owner(reqwest::Method::GET, "/v1/contacts");
    let page: Value = read().send().await.unwrap().json().await.unwrap();
    let selected = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["actor"]["id"] == child)
        .unwrap();
    assert_eq!(selected["model"], "specialist");
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["model"] == "mock")
    );

    config.models.remove("specialist");
    fixture.system.update_config(config.clone()).await.unwrap();
    let page: Value = read().send().await.unwrap().json().await.unwrap();
    let selected = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["actor"]["id"] == child)
        .unwrap();
    assert_eq!(selected["model"], "mock");

    config.models.clear();
    config.providers.clear();
    config.main_model.clear();
    config.approval_model = None;
    fixture.system.update_config(config).await.unwrap();
    let page: Value = read().send().await.unwrap().json().await.unwrap();
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["model"].is_null())
    );
}

#[tokio::test]
async fn contacts_search_visible_bubbles_and_names_with_bounded_account_scoped_pages() {
    let fixture = Fixture::new().await;
    let alpha = fixture.child("Athena").await;
    let zeta = fixture.child("Zeus").await;
    let first = fixture
        .system
        .send_user(&alpha, "Read the contact document".into(), vec![], None)
        .unwrap();
    fixture
        .system
        .store()
        .finish_message(
            &first.id,
            MessageStatus::Completed,
            Some("Contact layout finished"),
            None,
        )
        .unwrap();
    let input = fixture
        .system
        .send_user(&alpha, "Next task".into(), vec![], None)
        .unwrap();
    fixture.system.store().claim_next(&alpha).unwrap();
    for (channel, text) in [
        ("assistant", "Cross-chunk conta"),
        ("assistant", "ct phrase"),
        ("reasoning", "private-contact-marker"),
    ] {
        fixture
            .system
            .store()
            .append_event(
                &alpha,
                ActorEvent::GenerationDelta {
                    message_id: input.id.clone(),
                    generation: 1,
                    channel: channel.into(),
                    text: text.into(),
                },
            )
            .unwrap();
    }
    fixture.system.store().tool_finished(&alpha,&input.id,&AgentToolCall{id:"sent".into(),name:"actors__message".into(),arguments:json!({})},
        json!({"sent_message":{"conversation_url":format!("/actors/{zeta}"),"content":"A dispatched contact bubble"}}),"compact receipt","Sent",None).unwrap();
    fixture
        .system
        .store()
        .tool_finished(
            &alpha,
            &input.id,
            &AgentToolCall {
                id: "private".into(),
                name: "files__read_file".into(),
                arguments: json!({}),
            },
            json!({"content":"private-contact-marker"}),
            "private-contact-marker",
            "private-contact-marker",
            None,
        )
        .unwrap();

    let get = |query: &str| {
        fixture
            .owner(reqwest::Method::GET, "/v1/contacts")
            .query(&[("q", query)])
    };
    for (query, kind) in [
        ("CONTACT DOCUMENT", "input"),
        ("layout finished", "reply"),
        ("contact phrase", "assistant"),
        ("dispatched contact", "sent"),
    ] {
        let response = get(query).send().await.unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        let page = response.json::<Value>().await.unwrap();
        assert_eq!(page["items"][0]["actor"]["id"], alpha);
        assert_eq!(page["items"][0]["messages"][0]["kind"], kind);
        if kind == "sent" {
            assert!(page["items"][0]["messages"][0]["result_id"].is_string());
        }
    }
    let name = get("aThEnA")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(name["items"][0]["actor"]["id"], alpha);
    for query in ["private-contact-marker"] {
        let page = get(query)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(page["items"], json!([]));
    }
    let mut after = None;
    let mut ids = Vec::new();
    loop {
        let mut request = fixture.owner(reqwest::Method::GET, "/v1/contacts?limit=2");
        if let Some(cursor) = after.as_ref() {
            request = request.query(&[("after", cursor)]);
        }
        let page = request.send().await.unwrap().json::<Value>().await.unwrap();
        let items = page["items"].as_array().unwrap();
        assert!(items.len() <= 2);
        ids.extend(
            items
                .iter()
                .map(|item| item["actor"]["id"].as_str().unwrap().to_owned()),
        );
        after = page["next_cursor"].as_str().map(str::to_owned);
        if after.is_none() {
            break;
        }
    }
    assert!(ids.contains(&alpha) && ids.contains(&zeta));
    let unique = ids.iter().collect::<std::collections::HashSet<_>>();
    assert_eq!(unique.len(), ids.len());
    assert_eq!(
        fixture
            .client
            .get(format!("{}/v1/contacts", fixture.url))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    for path in ["/v1/contacts?limit=33", "/v1/contacts?after=invalid"] {
        assert!(
            fixture
                .owner(reqwest::Method::GET, path)
                .send()
                .await
                .unwrap()
                .status()
                .is_client_error()
        );
    }
}
