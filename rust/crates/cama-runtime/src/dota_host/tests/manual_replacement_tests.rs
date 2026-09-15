use super::*;

async fn replaced() -> Fixture {
    let f = Fixture::new(false);
    f.launch().await;
    f.update_state(|record, state| {
        record.phase = Phase::NeedsReview;
        record.valve_match_id = Some("888".into());
        state.manual_record_override = Some(serde_json::json!({
            "replacement_match_id": 9999, "previous_valve_match_id": "888",
            "actor_id": 123, "requested_at": 120,
        }));
    });
    f.port.lobby.lock().unwrap().as_mut().unwrap().match_id = Some(888);
    f.port.calls.lock().unwrap().clear();
    f
}

#[tokio::test]
async fn replaced_host_never_records_or_launches_old_game_even_after_pending_cleanup() {
    for remove_pending in [false, true] {
        let f = replaced().await;
        if remove_pending {
            PendingMatchRepository::new(&f.worker.path)
                .delete_pending_match(1, f.pending)
                .unwrap();
        }
        for now in [130, 200, 500] {
            f.worker.tick(&f.port, now).await.unwrap();
        }
        assert!(f.port.calls.lock().unwrap().is_empty());
        assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
        assert_eq!(f.session().phase, Phase::NeedsReview);
        // Even a completed old result only permits cleanup, never settlement.
        f.complete(Some("dire"));
        f.worker.tick(&f.port, 510).await.unwrap();
        assert_eq!(*f.port.calls.lock().unwrap(), vec!["destroy"]);
        assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
        f.worker.tick(&f.port, 520).await.unwrap();
        assert_eq!(f.session().phase, Phase::Cancelled);
        assert!(f.session().payload.get("manual_record_override").is_some());
        assert_eq!(
            PendingMatchRepository::new(&f.worker.path)
                .pending_match(1, f.pending)
                .unwrap()
                .is_some(),
            !remove_pending
        );
    }
}

#[tokio::test]
async fn replaced_lobby_cleanup_requires_fresh_owned_matching_old_game() {
    for changed in ["owner", "lobby", "match", "stale"] {
        let f = replaced().await;
        f.complete(Some("radiant"));
        {
            let mut lobby = f.port.lobby.lock().unwrap();
            let lobby = lobby.as_mut().unwrap();
            match changed {
                "owner" => lobby.owner_account_id = 42,
                "lobby" => lobby.id += 1,
                "match" => lobby.match_id = Some(9999),
                "stale" => f.port.observation_stale.store(true, Ordering::SeqCst),
                _ => unreachable!(),
            }
        }
        f.worker.tick(&f.port, 130).await.unwrap();
        assert!(f.port.calls.lock().unwrap().is_empty(), "{changed}");
        assert_eq!(f.session().phase, Phase::NeedsReview);
    }
}

#[tokio::test]
async fn old_host_controls_cannot_resume_or_void_the_manual_replacement() {
    let f = replaced().await;
    for action in ["start", "resume", "cancel"] {
        assert!(
            guild_operator_command(f.worker.path.clone(), action, 1, Some(f.pending))
                .await
                .unwrap_err()
                .contains("replaced")
        );
    }
    for outcome in ["void", "recorded"] {
        assert!(
            guild_resolution_command(
                f.worker.path.clone(),
                1,
                f.pending,
                123,
                outcome.into(),
                888,
                "old lobby stuck".into()
            )
            .await
            .unwrap_err()
            .contains("replaced")
        );
    }
    let status = guild_operator_command(f.worker.path.clone(), "status", 1, Some(f.pending))
        .await
        .unwrap();
    assert!(status.contains("Manual replacement Dota 9999"));
    assert!(status.contains("automatic recording is disabled"));
    assert!(f.port.calls.lock().unwrap().is_empty());
}
