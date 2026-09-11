use super::*;
use crate::dota_session_repository::{DotaSessionRepository, DotaSessionRepositoryError};
use serde_json::json;
use std::sync::{Arc, Barrier};

fn fixture() -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    cama_db_core::schema_manager::initialize_or_migrate(file.path()).unwrap();
    let connection = open_runtime_connection(file.path()).unwrap();
    connection
        .execute(
            "INSERT INTO guild_config(guild_id,league_id) VALUES(1,123),(2,123)",
            [],
        )
        .unwrap();
    for player in 1..=30 {
        connection
            .execute(
                "INSERT INTO players(discord_id,guild_id,discord_username) VALUES(?1,1,'player')",
                [player],
            )
            .unwrap();
        connection.execute("INSERT INTO player_steam_ids(discord_id,steam_id,is_primary,added_at) VALUES(?1,?1,1,100)",[player]).unwrap();
    }
    file
}
fn config(repo: &PendingMatchRepository) {
    repo.configure_dota_host_routing(Some(DotaHostRouting {
        account_key: "99".into(),
        guild_ids: vec![1],
    }))
    .unwrap();
}
fn state(offset: i64) -> PendingMatchState {
    PendingMatchState {
        radiant_team_ids: (offset + 1..=offset + 5).collect(),
        dire_team_ids: (offset + 6..=offset + 10).collect(),
        shuffle_timestamp: Some(100),
        bet_lock_until: Some(1000),
        ..Default::default()
    }
}

#[test]
fn busy_fallback_is_permanent_manual_and_keeps_timed_betting_after_restart() {
    let file = fixture();
    let repo = PendingMatchRepository::new(file.path());
    config(&repo);
    let bot = repo.create_pending_match(1, &state(0)).unwrap();
    assert_eq!(bot.state.extra["dota_host_account_key"], "99");
    assert_eq!(bot.state.extra["dota_hosting"]["hosting"], "bot");
    let manual = repo.create_pending_match(1, &state(10)).unwrap();
    assert_eq!(
        manual.state.extra["dota_hosting_fallback_reason"],
        "bot_busy"
    );
    assert_eq!(manual.state.extra["dota_hosting"]["hosting"], "manual");
    assert_eq!(manual.state.bet_lock_until, Some(1000));
    assert!(manual.state.betting_open(999));
    assert!(!manual.state.betting_open(1000));
    assert!(!manual.state.hosted_betting_managed());
    repo.delete_pending_match(1, bot.pending_match_id).unwrap();
    let restarted = PendingMatchRepository::new(file.path());
    config(&restarted);
    let saved = restarted
        .pending_match(1, manual.pending_match_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        saved.state.extra["dota_hosting_fallback_reason"],
        "bot_busy"
    );
    assert!(!saved.state.extra.contains_key("dota_host_account_key"));
    assert_eq!(
        restarted
            .create_pending_match(1, &state(20))
            .unwrap()
            .state
            .extra["dota_host_account_key"],
        "99"
    );
}

#[test]
fn concurrent_creations_reserve_the_idle_account_exactly_once() {
    let file = fixture();
    config(&PendingMatchRepository::new(file.path()));
    let barrier = Arc::new(Barrier::new(2));
    let workers = (0..2)
        .map(|index| {
            let path = file.path().to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                PendingMatchRepository::new(path)
                    .create_pending_match(1, &state(index * 10))
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let rows = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        rows.iter()
            .filter(|p| p.state.extra.contains_key("dota_host_account_key"))
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(
                |p| p.state.extra.get("dota_hosting_fallback_reason") == Some(&json!("bot_busy"))
            )
            .count(),
        1
    );
}

#[test]
fn active_account_is_busy_before_new_roster_validation_and_scope_is_global() {
    let file = fixture();
    let repo = PendingMatchRepository::new(file.path());
    config(&repo);
    DotaSessionRepository::new(file.path())
        .claim_session(2, 999, "99", json!({}), 100)
        .unwrap();
    let mut invalid = state(0);
    invalid.radiant_team_ids = vec![-1, -2, -3, -4, -5];
    let pending = repo.create_pending_match(1, &invalid).unwrap();
    assert_eq!(
        pending.state.extra["dota_hosting_fallback_reason"],
        "bot_busy"
    );
}

#[test]
fn missing_links_and_manual_guild_defaults_do_not_reserve_the_bot() {
    let file = fixture();
    let repo = PendingMatchRepository::new(file.path());
    config(&repo);
    let connection = open_runtime_connection(file.path()).unwrap();
    connection
        .execute("DELETE FROM player_steam_ids WHERE discord_id=1", [])
        .unwrap();
    let pending = repo.create_pending_match(1, &state(0)).unwrap();
    assert_eq!(
        pending.state.extra["dota_hosting_fallback_reason"],
        "missing_steam_links"
    );
    connection
        .execute(
            "UPDATE guild_config SET dota_hosting_options=?1 WHERE guild_id=1",
            [json!({"hosting":"manual","league_id":777}).to_string()],
        )
        .unwrap();
    let manual = repo.create_pending_match(1, &state(10)).unwrap();
    assert_eq!(
        manual.state.extra["dota_hosting_fallback_reason"],
        "manual_requested"
    );
    assert_eq!(manual.state.extra["dota_hosting"]["league_id"], 777);
    assert!(!manual.state.extra.contains_key("dota_host_account_key"));
}

#[test]
fn old_unmarked_pending_and_cancelled_reservations_cannot_be_adopted() {
    let file = fixture();
    let repo = PendingMatchRepository::new(file.path());
    let old = repo.create_pending_match(1, &state(0)).unwrap();
    config(&repo);
    let sessions = DotaSessionRepository::new(file.path());
    assert!(matches!(
        sessions.claim_reserved_session(1, old.pending_match_id, "99", json!({}), 110),
        Err(DotaSessionRepositoryError::ReservationUnavailable { .. })
    ));
    let fresh = repo.create_pending_match(1, &state(10)).unwrap();
    let mut manual = fresh.state;
    manual.extra.get_mut("dota_hosting").unwrap()["hosting"] = json!("manual");
    repo.update_pending_match(1, fresh.pending_match_id, &manual)
        .unwrap();
    assert!(matches!(
        sessions.claim_reserved_session(1, fresh.pending_match_id, "99", json!({}), 115),
        Err(DotaSessionRepositoryError::ReservationUnavailable { .. })
    ));
}

#[test]
fn manual_handoff_and_prelaunch_cancellation_release_the_unclaimed_reservation() {
    let file = fixture();
    let repo = PendingMatchRepository::new(file.path());
    config(&repo);
    let first = repo.create_pending_match(1, &state(0)).unwrap();
    let handed = repo
        .request_manual_dota_hosting(1, first.pending_match_id)
        .unwrap();
    assert!(!handed.state.extra.contains_key("dota_host_account_key"));
    let second = repo.create_pending_match(1, &state(10)).unwrap();
    assert_eq!(second.state.extra["dota_host_account_key"], "99");
    let cancelled = repo
        .release_hosted_betting(1, second.pending_match_id, 200)
        .unwrap();
    assert_eq!(cancelled.state.extra["dota_hosting"]["hosting"], "manual");
    assert!(!cancelled.state.extra.contains_key("dota_host_account_key"));
    assert_eq!(
        repo.create_pending_match(1, &state(20))
            .unwrap()
            .state
            .extra["dota_host_account_key"],
        "99"
    );
}
