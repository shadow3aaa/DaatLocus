use super::io::read_response_text_with_idle_timeout;
use super::*;

pub(super) fn slow_response(chunks: Vec<Vec<u8>>, gap: Duration) -> reqwest::Response {
    let stream = futures_util::stream::unfold(chunks.into_iter(), move |mut chunks| async move {
        let chunk = chunks.next()?;
        tokio::time::sleep(gap).await;
        Some((Ok::<_, std::io::Error>(chunk), chunks))
    });
    let body = reqwest::Body::wrap_stream(stream);
    let response = axum::http::Response::builder()
        .header("content-type", "text/plain; charset=windows-1252")
        .body(body)
        .unwrap();
    response.into()
}

#[tokio::test(start_paused = true)]
async fn response_body_deadline_tracks_activity_and_preserves_charset_decoding() {
    let response = slow_response(
        vec![b"caf".to_vec(), vec![0xe9], b"!".to_vec()],
        Duration::from_secs(600),
    );
    let body = read_response_text_with_idle_timeout(
        response,
        Some(Duration::from_secs(900)),
        "test",
        "http://fixture.invalid",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(body, "café!");
}

#[tokio::test(start_paused = true)]
async fn silent_body_can_wait_without_limit_or_fail_with_a_typed_idle_timeout() {
    for timeout in [None, Some(Duration::from_secs(900))] {
        let response = slow_response(vec![b"finished".to_vec()], Duration::from_secs(3600));
        let result = read_response_text_with_idle_timeout(
            response,
            timeout,
            "test",
            "http://fixture.invalid",
            &[],
        )
        .await;
        match timeout {
            None => assert_eq!(result.unwrap(), "finished"),
            Some(_) => assert_eq!(
                result
                    .unwrap_err()
                    .downcast_ref::<ModelRequestFailure>()
                    .unwrap()
                    .kind,
                ModelFailureKind::Timeout
            ),
        }
    }
}

#[tokio::test]
async fn response_header_wait_can_be_disabled_without_removing_explicit_deadlines() {
    for timeout in [None, Some(Duration::from_secs(30))] {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let received = entered.clone();
        let gate = release.clone();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new().fallback(axum::routing::post(move || {
            let entered = received.clone();
            let gate = gate.clone();
            async move {
                entered.notify_one();
                gate.notified().await;
                axum::http::StatusCode::NO_CONTENT
            }
        }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = crate::http_client::builder().no_proxy().build().unwrap();
        let task = tokio::spawn(async move {
            send_request_for_streaming_response(client.post(&url), timeout, "test", &url, &[]).await
        });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(3600)).await;
        tokio::task::yield_now().await;
        if timeout.is_none() {
            assert!(!task.is_finished());
        }
        if timeout.is_none() {
            release.notify_one();
        }
        tokio::time::resume();
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        match timeout {
            None => assert_eq!(result.unwrap().status(), reqwest::StatusCode::NO_CONTENT),
            Some(_) => assert_eq!(
                result
                    .unwrap_err()
                    .downcast_ref::<ModelRequestFailure>()
                    .unwrap()
                    .kind,
                ModelFailureKind::Timeout
            ),
        }
        release.notify_one();
        server.abort();
    }
}
