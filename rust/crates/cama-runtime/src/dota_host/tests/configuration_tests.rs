use super::*;

async fn gathering() -> Fixture {
    let f = Fixture::new(false);
    f.update_state(|_, state| state.start_mode = StartMode::Manual);
    f.worker.tick(&f.port, 100).await.unwrap();
    f.worker.tick(&f.port, 101).await.unwrap();
    assert_eq!(f.session().phase, Phase::Gathering);
    f.port.calls.lock().unwrap().clear();
    f
}

fn patch() -> DotaHostingOptions {
    DotaHostingOptions {
        region: Some(27),
        game_mode: Some(22),
        first_pick: Some(FirstPick::Dire),
        start: Some(StartMode::Manual),
        tv_delay: Some(0),
        league_id: Some(456),
        visibility: Some(2),
        ..Default::default()
    }
}

async fn configure(f: &Fixture, patch: DotaHostingOptions) -> Result<String, String> {
    guild_configure_command(f.worker.path.clone(), 1, Some(f.pending), 123, patch).await
}

#[tokio::test]
async fn active_settings_wait_for_confirmation_and_preserve_roster_and_defaults() {
    let f = gathering().await;
    let before = PendingMatchRepository::new(&f.worker.path)
        .pending_match(1, f.pending)
        .unwrap()
        .unwrap();
    configure(&f, patch()).await.unwrap();
    let queued: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert_eq!(queued.settings.server_region, 1);
    assert!(queued.configuration.is_some());
    assert!(
        guild_operator_command(f.worker.path.clone(), "start", 1, Some(f.pending))
            .await
            .unwrap_err()
            .contains("still being applied")
    );
    assert!(
        configure(&f, patch())
            .await
            .unwrap_err()
            .contains("already pending")
    );
    f.worker.tick(&f.port, 110).await.unwrap();
    let sent: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert_eq!(sent.settings.server_region, 1);
    assert!(sent.configuration.is_some());
    assert_eq!(*f.port.calls.lock().unwrap(), vec!["configure"]);
    f.worker.tick(&f.port, 111).await.unwrap();
    let applied: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert!(applied.configuration.is_none());
    assert!(applied.last_configuration.is_some());
    assert_eq!(applied.settings.server_region, 27);
    assert_eq!(applied.settings.game_mode, 22);
    assert_eq!(applied.settings.first_pick_radiant, Some(false));
    assert_eq!(applied.settings.tv_delay, 0);
    assert_eq!(applied.settings.league_id, 456);
    assert_eq!(applied.settings.visibility, 2);
    assert_eq!(applied.start_mode, StartMode::Manual);
    assert!(pending_matches_roster(&before, &applied.roster));
    let after = PendingMatchRepository::new(&f.worker.path)
        .pending_match(1, f.pending)
        .unwrap()
        .unwrap();
    assert_eq!(before.state.radiant_team_ids, after.state.radiant_team_ids);
    assert_eq!(before.state.dire_team_ids, after.state.dire_team_ids);
    assert_eq!(
        before.state.extra.get("dota_hosting"),
        after.state.extra.get("dota_hosting")
    );
    let status = guild_operator_command(f.worker.path.clone(), "status", 1, Some(f.pending))
        .await
        .unwrap();
    assert!(status.contains("region 27"));
    assert!(status.contains("Last settings change confirmed"));
    assert!(!status.contains("change pending"));
    f.worker.tick(&f.port, 120).await.unwrap();
    assert_eq!(f.session().phase, Phase::Gathering);
    guild_operator_command(f.worker.path.clone(), "start", 1, Some(f.pending))
        .await
        .unwrap();
    f.worker.tick(&f.port, 121).await.unwrap();
    assert_eq!(f.session().phase, Phase::Launching);
}

#[tokio::test]
async fn lost_configuration_reply_reconciles_from_saved_intent_without_resending() {
    let f = gathering().await;
    *f.port.configure_behavior.lock().unwrap() = "lost_reply";
    configure(&f, patch()).await.unwrap();
    assert!(
        f.worker
            .tick(&f.port, 110)
            .await
            .unwrap_err()
            .contains("reply lost")
    );
    // Every tick reloads durable state, as a restarted worker would.
    f.worker.tick(&f.port, 120).await.unwrap();
    assert_eq!(*f.port.calls.lock().unwrap(), vec!["configure"]);
    assert!(
        serde_json::from_value::<SessionState>(f.session().payload)
            .unwrap()
            .configuration
            .is_none()
    );
}

#[tokio::test]
async fn unconfirmed_configuration_times_out_holds_launch_and_can_resume() {
    let f = gathering().await;
    *f.port.configure_behavior.lock().unwrap() = "ignore";
    configure(&f, patch()).await.unwrap();
    f.worker.tick(&f.port, 110).await.unwrap();
    f.worker.tick(&f.port, 111).await.unwrap();
    assert_eq!(*f.port.calls.lock().unwrap(), vec!["configure"]);
    f.worker.tick(&f.port, 201).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert_eq!(*f.port.calls.lock().unwrap(), vec!["configure"]);
    *f.port.configure_behavior.lock().unwrap() = "normal";
    guild_operator_command(f.worker.path.clone(), "resume", 1, Some(f.pending))
        .await
        .unwrap();
    f.worker.tick(&f.port, 210).await.unwrap();
    f.worker.tick(&f.port, 211).await.unwrap();
    let applied: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert!(applied.configuration.is_none());
    assert_eq!(applied.settings.server_region, 27);
    assert_eq!(f.session().phase, Phase::Gathering);
}

#[tokio::test]
async fn configuration_rechecks_live_launch_ownership_and_freshness() {
    for changed in ["launch", "owner", "stale", "roster"] {
        let f = gathering().await;
        configure(&f, patch()).await.unwrap();
        match changed {
            "launch" => f.port.lobby.lock().unwrap().as_mut().unwrap().stage = LobbyStage::Running,
            "owner" => {
                f.port
                    .lobby
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .owner_account_id = 42
            }
            "stale" => f.port.observation_stale.store(true, Ordering::SeqCst),
            "roster" => {
                let repo = PendingMatchRepository::new(&f.worker.path);
                let mut pending = repo.pending_match(1, f.pending).unwrap().unwrap();
                pending.state.radiant_team_ids.swap(0, 1);
                pending.state.radiant_team_ids[0] = 42;
                repo.update_pending_match(1, f.pending, &pending.state)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let result = f.worker.tick(&f.port, 110).await;
        if changed == "stale" {
            assert!(result.is_err());
        } else {
            result.unwrap();
        }
        assert!(f.port.calls.lock().unwrap().is_empty(), "{changed}");
    }
}

#[tokio::test]
async fn configuration_admission_rejects_launch_cancel_manual_and_other_guilds() {
    for changed in [
        "launch",
        "start",
        "cancel",
        "manual",
        "incomplete",
        "other_guild",
    ] {
        let f = gathering().await;
        match changed {
            "launch" => f.update_state(|record, _| record.phase = Phase::Launching),
            "start" => f.update_state(|_, state| state.manual_start_requested = true),
            "cancel" => f.update_state(|_, state| state.cancel_requested = true),
            "manual" => {
                guild_operator_command(f.worker.path.clone(), "manual", 1, Some(f.pending))
                    .await
                    .unwrap();
            }
            "incomplete" => {
                let repo = PendingMatchRepository::new(&f.worker.path);
                let mut pending = repo.pending_match(1, f.pending).unwrap().unwrap();
                pending
                    .state
                    .extra
                    .insert("draft_setup_complete".into(), false.into());
                repo.update_pending_match(1, f.pending, &pending.state)
                    .unwrap();
            }
            "other_guild" => {}
            _ => unreachable!(),
        }
        let before = f.session();
        let result = guild_configure_command(
            f.worker.path.clone(),
            if changed == "other_guild" { 2 } else { 1 },
            Some(f.pending),
            123,
            patch(),
        )
        .await;
        assert!(result.is_err(), "{changed}");
        assert_eq!(f.session(), before);
        assert!(f.port.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn start_mode_only_change_is_confirmed_without_reconfiguring_steam() {
    let f = gathering().await;
    configure(
        &f,
        DotaHostingOptions {
            start: Some(StartMode::Automatic),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    f.worker.tick(&f.port, 110).await.unwrap();
    assert!(f.port.calls.lock().unwrap().is_empty());
    f.worker.tick(&f.port, 111).await.unwrap();
    assert_eq!(*f.port.calls.lock().unwrap(), vec!["launch"]);
}

#[tokio::test]
async fn external_launch_during_configuration_keeps_tracking_known_settings() {
    for applied in [false, true] {
        let f = gathering().await;
        configure(
            &f,
            DotaHostingOptions {
                region: Some(27),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        if applied {
            *f.port.configure_behavior.lock().unwrap() = "lost_reply";
            assert!(f.worker.tick(&f.port, 110).await.is_err());
        }
        {
            let mut lobby = f.port.lobby.lock().unwrap();
            let lobby = lobby.as_mut().unwrap();
            lobby.stage = LobbyStage::Running;
            lobby.game_state = Some(4);
            lobby.match_id = Some(888);
            lobby.server_id = Some(999);
        }
        f.port.calls.lock().unwrap().clear();
        f.worker.tick(&f.port, 120).await.unwrap();
        let saved: SessionState = serde_json::from_value(f.session().payload).unwrap();
        assert!(saved.configuration.is_none());
        assert_eq!(saved.settings.server_region, if applied { 27 } else { 1 });
        assert!(f.port.calls.lock().unwrap().is_empty());
        assert!(
            !PendingMatchRepository::new(&f.worker.path)
                .pending_match(1, f.pending)
                .unwrap()
                .unwrap()
                .state
                .betting_open(120)
        );
        // The in-progress match has no result in this fixture yet; tracking
        // reaches the normal result poll rather than getting stuck in review.
        assert_eq!(
            f.worker.tick(&f.port, 121).await.unwrap_err(),
            "unavailable"
        );
        assert_eq!(f.session().phase, Phase::Running);
        f.complete(Some("radiant"));
        f.worker.tick(&f.port, 160).await.unwrap();
        assert_eq!(f.recorder.count.load(Ordering::SeqCst), 1);
        assert!(
            !f.port
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call == "configure" || call == "launch")
        );
    }
}
