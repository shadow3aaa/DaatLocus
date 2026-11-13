use super::*;

#[tokio::test]
async fn model_wait_has_no_total_deadline_and_keeps_stop_and_cancel_responsive() {
    for action in ["complete", "cancel", "stop", "buffer"] {
        let (_directory, system, provider) = paused_runtime().await;
        let gate = Arc::new(tokio::sync::Notify::new());
        *provider.inference_gate.lock() = Some(gate.clone());
        provider
            .replies
            .lock()
            .push_back(MockReply::Answer("Long inference completed"));
        let input = system
            .test_actor_input("root", "Think slowly".into(), vec![], None)
            .unwrap();
        let input = system.store().claim_next(&input.actor_id).unwrap().unwrap();
        let actor = system.execution_actor(&input).unwrap();
        let model = InferenceModel::load(&system, &actor).unwrap();
        let control = ActorControl::default();
        let active = control.clone();
        let running = system.clone();
        let input_id = input.id.clone();
        let task = tokio::spawn(async move {
            generate(
                &running,
                &actor,
                &input,
                &active,
                model.provider.as_ref(),
                AgentTurnRequest {
                    messages: vec![AgentMessage::user("Think slowly")],
                    tools: vec![],
                },
                &mut 0,
                &model.binding,
                &model.usage,
                &TokenEstimateBaseline::default(),
                false,
                true,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), provider.started.notified())
            .await
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(3600)).await;
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "a live inference must survive the former total deadline"
        );
        match action {
            "complete" => gate.notify_one(),
            "cancel" => {
                system.cancel("root", &input_id).unwrap();
            }
            "stop" => control.stop.store(true, Ordering::Release),
            "buffer" => {
                system
                    .test_actor_input("root", "New direction".into(), vec![], None)
                    .unwrap();
                tokio::task::yield_now().await;
                assert!(!task.is_finished());
                gate.notify_one();
            }
            _ => unreachable!(),
        }
        tokio::time::resume();
        let outcome = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        if action == "complete" || action == "buffer" {
            assert_eq!(
                outcome.unwrap().unwrap().last_assistant_message.as_deref(),
                Some("Long inference completed")
            );
        } else if action == "stop" {
            assert!(outcome.is_err());
        } else {
            assert!(outcome.unwrap().is_none());
        }
        assert_eq!(
            provider.requests.lock().len(),
            1,
            "waiting must not resend inference"
        );
        system.shutdown().await;
    }
}
