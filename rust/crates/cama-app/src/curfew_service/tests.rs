use crate::test_support::copy_migrated_database as initialize_or_migrate;
use cama_db::curfew::CurfewRepository;
use cama_domain::curfew::CurfewMode;

use chrono::{TimeZone, Utc};

use super::*;
use crate::embeds::LobbyPlayer;
use crate::lobby_service::{LobbyClock, LobbyPlayerPort, PendingMatchPort, PendingMatchState};

const GUILD: i64 = 0;
// 11pm ET, safely inside a 10pm-6am curfew window and outside a 9am-5pm one.
fn eleven_pm_et() -> DateTime<Utc> {
    chrono_tz::America::New_York
        .with_ymd_and_hms(2026, 1, 1, 23, 0, 0)
        .unwrap()
        .with_timezone(&Utc)
}
fn noon_et() -> DateTime<Utc> {
    chrono_tz::America::New_York
        .with_ymd_and_hms(2026, 1, 1, 12, 0, 0)
        .unwrap()
        .with_timezone(&Utc)
}

fn fixture() -> (tempfile::TempDir, CurfewService, CurfewRepository) {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("curfew.db");
    initialize_or_migrate(&path).expect("migrate");
    let repository = CurfewRepository::new(&path);
    (
        directory,
        CurfewService::new(repository.clone()),
        repository,
    )
}

fn test_connection(dir: &tempfile::TempDir) -> rusqlite::Connection {
    let path = dir.path().join("curfew.db");
    let connection = rusqlite::Connection::open(&path).expect("open");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys");
    connection
}

fn insert_player(repository: &CurfewRepository, dir: &tempfile::TempDir, discord_id: i64) {
    test_connection(dir)
        .execute(
            "INSERT INTO players(discord_id, guild_id, discord_username) VALUES (?1, 0, ?2)",
            rusqlite::params![discord_id, format!("player-{discord_id}")],
        )
        .expect("insert player");
    let _ = repository;
}

mod add_window {
    use super::*;

    #[test]
    fn test_add_window_persists() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let change = service
            .add_window(1, GUILD, "work", 9, 0, 17, 0, None, None, None, Utc::now())
            .unwrap();

        let CurfewWindowChange::Applied(window) = change else {
            panic!("expected an immediate apply, got {change:?}");
        };
        assert_eq!(window.name, "work");
        assert_eq!(
            repository.list_for_player(1, GUILD).unwrap()[0].name,
            "work"
        );
    }

    #[test]
    fn test_add_window_strips_name() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let change = service
            .add_window(
                1,
                GUILD,
                "  work  ",
                9,
                0,
                17,
                0,
                None,
                None,
                None,
                Utc::now(),
            )
            .unwrap();

        let CurfewWindowChange::Applied(window) = change else {
            panic!("expected an immediate apply, got {change:?}");
        };
        assert_eq!(window.name, "work");
    }

    #[test]
    fn test_add_window_rejects_empty_name() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let error = service
            .add_window(1, GUILD, "   ", 9, 0, 17, 0, None, None, None, Utc::now())
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::EmptyName));
    }

    #[test]
    fn test_add_window_rejects_too_long_name() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let long_name = "x".repeat(41);
        let error = service
            .add_window(
                1,
                GUILD,
                &long_name,
                9,
                0,
                17,
                0,
                None,
                None,
                None,
                Utc::now(),
            )
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::NameTooLong));
    }

    #[test]
    fn test_add_window_rejects_out_of_range_hour() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let error = service
            .add_window(1, GUILD, "work", 24, 0, 17, 0, None, None, None, Utc::now())
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::InvalidHour));
    }

    #[test]
    fn test_add_window_rejects_equal_start_and_end() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let error = service
            .add_window(1, GUILD, "work", 9, 0, 9, 0, None, None, None, Utc::now())
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::EqualStartAndEnd));
    }

    #[test]
    fn test_add_window_rejects_unknown_timezone() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let error = service
            .add_window(
                1,
                GUILD,
                "work",
                9,
                0,
                17,
                0,
                Some("Not/AZone"),
                None,
                None,
                Utc::now(),
            )
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::InvalidTimezone(_)));
    }

    #[test]
    fn test_add_window_rejects_unknown_mode() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let error = service
            .add_window(
                1,
                GUILD,
                "work",
                9,
                0,
                17,
                0,
                None,
                None,
                Some("chaotic"),
                Utc::now(),
            )
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::InvalidMode(_)));
    }

    #[test]
    fn test_add_window_unregistered_player_raises() {
        let (_dir, service, _repository) = fixture();

        let error = service
            .add_window(
                999,
                GUILD,
                "work",
                9,
                0,
                17,
                0,
                None,
                None,
                None,
                Utc::now(),
            )
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::PlayerNotRegistered));
    }

    #[test]
    fn test_add_window_replaces_existing_name() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(1, GUILD, "work", 9, 0, 17, 0, None, None, None, Utc::now())
            .unwrap();

        service
            .add_window(1, GUILD, "work", 8, 0, 16, 0, None, None, None, Utc::now())
            .unwrap();

        let windows = repository.list_for_player(1, GUILD).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].start_hour, 8);
    }

    #[test]
    fn test_add_window_defaults_to_default_mode() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        service
            .add_window(1, GUILD, "work", 9, 0, 17, 0, None, None, None, Utc::now())
            .unwrap();

        assert_eq!(
            repository.list_for_player(1, GUILD).unwrap()[0].mode,
            CurfewMode::Default
        );
    }

    #[test]
    fn test_add_window_accepts_explicit_mode() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        service
            .add_window(
                1,
                GUILD,
                "work",
                9,
                0,
                17,
                0,
                None,
                None,
                Some("strict"),
                Utc::now(),
            )
            .unwrap();
        assert_eq!(
            repository.list_for_player(1, GUILD).unwrap()[0].mode,
            CurfewMode::Strict
        );
    }

    #[test]
    fn test_editing_a_strict_window_stages_instead_of_applying_immediately() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(
                1,
                GUILD,
                "work",
                9,
                0,
                17,
                0,
                Some("America/New_York"),
                None,
                Some("strict"),
                Utc::now(),
            )
            .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();

        let change = service
            .add_window(
                1,
                GUILD,
                "work",
                8,
                0,
                16,
                0,
                Some("America/New_York"),
                None,
                Some("strict"),
                now,
            )
            .unwrap();

        let CurfewWindowChange::Staged {
            window,
            effective_at,
        } = change
        else {
            panic!("expected the edit to be staged, got {change:?}");
        };
        assert_eq!(window.start_hour, 8);
        assert!(effective_at > now);

        // Today's committed window is untouched.
        let live = repository.get_window(1, GUILD, "work").unwrap().unwrap();
        assert_eq!(live.start_hour, 9);
    }

    #[test]
    fn test_editing_a_default_mode_window_still_applies_immediately() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(1, GUILD, "work", 9, 0, 17, 0, None, None, None, Utc::now())
            .unwrap();

        let change = service
            .add_window(1, GUILD, "work", 8, 0, 16, 0, None, None, None, Utc::now())
            .unwrap();

        assert!(matches!(change, CurfewWindowChange::Applied(_)));
        assert_eq!(
            repository
                .get_window(1, GUILD, "work")
                .unwrap()
                .unwrap()
                .start_hour,
            8
        );
    }

    #[test]
    fn test_creating_a_brand_new_strict_window_applies_immediately() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let change = service
            .add_window(
                1,
                GUILD,
                "work",
                9,
                0,
                17,
                0,
                None,
                None,
                Some("strict"),
                Utc::now(),
            )
            .unwrap();

        assert!(matches!(change, CurfewWindowChange::Applied(_)));
        assert!(repository.get_window(1, GUILD, "work").unwrap().is_some());
    }
}

mod remove_and_list_windows {
    use super::*;

    #[test]
    fn test_remove_window() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(1, GUILD, "work", 9, 0, 17, 0, None, None, None, Utc::now())
            .unwrap();

        assert_eq!(
            service.remove_window(1, GUILD, "work", Utc::now()).unwrap(),
            CurfewRemoveOutcome::Removed
        );
        assert!(service.list_windows(1, GUILD).unwrap().is_empty());
    }

    #[test]
    fn test_remove_missing_window() {
        let (_dir, service, _repository) = fixture();
        assert_eq!(
            service.remove_window(1, GUILD, "nope", Utc::now()).unwrap(),
            CurfewRemoveOutcome::NotFound
        );
    }

    #[test]
    fn test_removing_a_strict_window_stages_the_delete_instead() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(
                1,
                GUILD,
                "work",
                9,
                0,
                17,
                0,
                Some("America/New_York"),
                None,
                Some("strict"),
                Utc::now(),
            )
            .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();

        let outcome = service.remove_window(1, GUILD, "work", now).unwrap();

        let CurfewRemoveOutcome::Staged { effective_at } = outcome else {
            panic!("expected the delete to be staged, got {outcome:?}");
        };
        assert!(effective_at > now);
        // The window is still live today — a strict curfew can't be
        // deleted out from under itself the same day it would fire.
        assert!(repository.get_window(1, GUILD, "work").unwrap().is_some());
    }

    #[test]
    fn test_list_windows_multiple() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(1, GUILD, "work", 9, 0, 17, 0, None, None, None, Utc::now())
            .unwrap();
        service
            .add_window(1, GUILD, "sleep", 22, 0, 6, 0, None, None, None, Utc::now())
            .unwrap();

        let names: std::collections::BTreeSet<String> = service
            .list_windows(1, GUILD)
            .unwrap()
            .into_iter()
            .map(|window| window.name)
            .collect();
        assert_eq!(
            names,
            std::collections::BTreeSet::from(["work".to_owned(), "sleep".to_owned()])
        );
    }
}

mod pending_changes {
    use super::*;

    #[test]
    fn test_apply_due_pending_changes_commits_a_staged_strict_edit() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(
                1,
                GUILD,
                "work",
                9,
                0,
                17,
                0,
                Some("America/New_York"),
                None,
                Some("strict"),
                Utc::now(),
            )
            .unwrap();
        let staged_at = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        service
            .add_window(
                1,
                GUILD,
                "work",
                8,
                0,
                16,
                0,
                Some("America/New_York"),
                None,
                Some("strict"),
                staged_at,
            )
            .unwrap();
        assert!(!service.pending_changes(1, GUILD).unwrap().is_empty());

        // The next local morning for a noon ET "now" is 08:00 ET the
        // following day (13:00 UTC). Just after midnight is still too early.
        let after_midnight = Utc.with_ymd_and_hms(2026, 1, 2, 6, 0, 0).unwrap();
        assert!(
            service
                .apply_due_pending_changes(after_midnight)
                .unwrap()
                .is_empty(),
            "a staged change must not land before the next local morning"
        );
        let next_morning = Utc.with_ymd_and_hms(2026, 1, 2, 13, 0, 0).unwrap();
        let applied = service.apply_due_pending_changes(next_morning).unwrap();

        assert_eq!(applied.len(), 1);
        assert_eq!(
            repository
                .get_window(1, GUILD, "work")
                .unwrap()
                .unwrap()
                .start_hour,
            8
        );
        assert!(service.pending_changes(1, GUILD).unwrap().is_empty());
    }
}

mod active_window {
    use super::*;

    #[test]
    fn test_returns_none_with_no_windows() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        assert!(
            service
                .active_window(1, GUILD, eleven_pm_et())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_returns_matching_window() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(1, GUILD, "sleep", 22, 0, 6, 0, None, None, None, Utc::now())
            .unwrap();

        let matched = service
            .active_window(1, GUILD, eleven_pm_et())
            .unwrap()
            .unwrap();
        assert_eq!(matched.name, "sleep");
    }

    #[test]
    fn test_returns_none_outside_window() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(1, GUILD, "sleep", 22, 0, 6, 0, None, None, None, Utc::now())
            .unwrap();

        assert!(
            service
                .active_window(1, GUILD, noon_et())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_window_without_its_own_timezone_falls_back_to_players_general_timezone() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        test_connection(&dir)
            .execute(
                "UPDATE players SET timezone = 'Asia/Tokyo' WHERE discord_id = 1 AND guild_id = 0",
                [],
            )
            .unwrap();
        service
            .add_window(1, GUILD, "sleep", 22, 0, 6, 0, None, None, None, Utc::now())
            .unwrap();
        // 10:30pm JST is 1:30pm UTC.
        let utc_now = Utc.with_ymd_and_hms(2026, 1, 1, 13, 30, 0).unwrap();

        let matched = service.active_window(1, GUILD, utc_now).unwrap();
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().name, "sleep");
    }
}

mod sweep {
    use std::sync::Mutex;

    use super::*;
    use crate::dedicated_lobby_channel::{GuildId, LobbyScope, UserId};
    use crate::embeds::LobbyKind;
    use crate::lobby_service::LobbyService;

    #[derive(Clone, Debug, Default)]
    struct FakePlayers;

    impl LobbyPlayerPort for FakePlayers {
        fn glicko_rating(&self, _player_id: UserId, _guild_id: GuildId) -> Option<f64> {
            Some(1_200.0)
        }

        fn get_by_ids(&self, player_ids: &[UserId], _guild_id: GuildId) -> Vec<LobbyPlayer> {
            player_ids
                .iter()
                .map(|player_id| LobbyPlayer::new(player_id.0, format!("Player {}", player_id.0)))
                .collect()
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakePendingMatches;

    impl PendingMatchPort for FakePendingMatches {
        fn pending_match_for_player(
            &self,
            _guild_id: GuildId,
            _player_id: UserId,
        ) -> Option<PendingMatchState> {
            None
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeClock {
        next: std::sync::Arc<Mutex<i64>>,
    }

    impl LobbyClock for FakeClock {
        fn now_ns(&self) -> i64 {
            let mut next = self.next.lock().unwrap();
            *next += 1;
            *next
        }
    }

    type TestLobbyService = LobbyService<FakePlayers, FakePendingMatches, FakeClock>;

    fn lobby_service() -> TestLobbyService {
        LobbyService::new(
            FakePlayers,
            Some(FakePendingMatches),
            FakeClock::default(),
            10,
            12,
        )
    }

    fn seat(lobby: &TestLobbyService, guild_id: i64, discord_id: i64, kind: LobbyKind) {
        let scope = LobbyScope::new(GuildId(guild_id), kind);
        lobby
            .get_or_create_lobby(Some(UserId(discord_id)), scope)
            .expect("create lobby");
        let outcome = lobby.join_lobby(UserId(discord_id), scope);
        assert!(outcome.success, "seat failed: {outcome:?}");
    }

    fn enable_curfew(
        repository: &CurfewRepository,
        discord_id: i64,
        guild_id: i64,
        start_hour: u32,
        end_hour: u32,
    ) {
        enable_curfew_with_mode(
            repository,
            discord_id,
            guild_id,
            start_hour,
            end_hour,
            CurfewMode::Default,
        );
    }

    fn enable_curfew_with_mode(
        repository: &CurfewRepository,
        discord_id: i64,
        guild_id: i64,
        start_hour: u32,
        end_hour: u32,
        mode: CurfewMode,
    ) {
        repository
            .add_or_replace(&cama_domain::curfew::CurfewWindow {
                discord_id,
                guild_id,
                name: "sleep".to_owned(),
                start_hour,
                start_minute: 0,
                end_hour,
                end_minute: 0,
                timezone: Some("America/New_York".to_owned()),
                days: None,
                mode,
            })
            .unwrap();
    }

    #[test]
    fn test_sweep_removes_only_players_in_active_windows() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        insert_player(&repository, &dir, 2);
        let lobby = lobby_service();
        seat(&lobby, GUILD, 1, LobbyKind::Open);
        seat(&lobby, GUILD, 2, LobbyKind::Open);
        enable_curfew(&repository, 1, GUILD, 22, 6);

        let kicks = service.sweep(&lobby, &[GUILD], eleven_pm_et());

        assert_eq!(kicks.len(), 1);
        assert_eq!(kicks[0].discord_id, 1);
        assert_eq!(kicks[0].window_name, "sleep");
        assert_eq!(kicks[0].lobby_kind, LobbyKind::Open);
        let scope = LobbyScope::new(GuildId(GUILD), LobbyKind::Open);
        let remaining = lobby.get_lobby(scope).unwrap().players;
        assert!(!remaining.contains(&UserId(1)));
        assert!(remaining.contains(&UserId(2)));
    }

    #[test]
    fn test_sweep_ignores_players_outside_their_window() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        let lobby = lobby_service();
        seat(&lobby, GUILD, 1, LobbyKind::Open);
        enable_curfew(&repository, 1, GUILD, 1, 5); // 1am-5am, not 11pm

        let kicks = service.sweep(&lobby, &[GUILD], eleven_pm_et());

        assert!(kicks.is_empty());
        let scope = LobbyScope::new(GuildId(GUILD), LobbyKind::Open);
        assert!(lobby.get_lobby(scope).unwrap().players.contains(&UserId(1)));
    }

    #[test]
    fn test_sweep_checks_every_lobby_kind() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        let lobby = lobby_service();
        seat(&lobby, GUILD, 1, LobbyKind::LowSkill);
        enable_curfew(&repository, 1, GUILD, 22, 6);

        let kicks = service.sweep(&lobby, &[GUILD], eleven_pm_et());

        assert_eq!(kicks.len(), 1);
        assert_eq!(kicks[0].lobby_kind, LobbyKind::LowSkill);
    }

    #[test]
    fn test_sweep_with_no_windows_set_is_a_noop() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        let lobby = lobby_service();
        seat(&lobby, GUILD, 1, LobbyKind::Open);

        let kicks = service.sweep(&lobby, &[GUILD], eleven_pm_et());
        assert!(kicks.is_empty());
    }

    #[test]
    fn test_sweep_scans_multiple_guilds() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        let lobby = lobby_service();
        seat(&lobby, GUILD, 1, LobbyKind::Open);
        enable_curfew(&repository, 1, GUILD, 22, 6);

        let kicks = service.sweep(&lobby, &[GUILD, 999_999], eleven_pm_et());

        assert_eq!(
            kicks.iter().map(|kick| kick.guild_id).collect::<Vec<_>>(),
            vec![GUILD]
        );
    }

    #[test]
    fn test_sweep_removes_strict_mode_players_like_default() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        let lobby = lobby_service();
        seat(&lobby, GUILD, 1, LobbyKind::Open);
        enable_curfew_with_mode(&repository, 1, GUILD, 22, 6, CurfewMode::Strict);

        let kicks = service.sweep(&lobby, &[GUILD], eleven_pm_et());

        assert_eq!(kicks.len(), 1);
        assert_eq!(kicks[0].discord_id, 1);
        assert_eq!(kicks[0].window_name, "sleep");
        let scope = LobbyScope::new(GuildId(GUILD), LobbyKind::Open);
        assert!(!lobby.get_lobby(scope).unwrap().players.contains(&UserId(1)));
    }
}

mod staged_changes {
    use super::*;

    fn add_named_mode_window(
        service: &CurfewService,
        mode: &str,
        start_hour: u32,
        now: DateTime<Utc>,
    ) {
        service
            .add_window(
                1,
                GUILD,
                "work",
                start_hour,
                0,
                17,
                0,
                Some("America/New_York"),
                None,
                Some(mode),
                now,
            )
            .unwrap();
    }

    #[test]
    fn test_staged_edit_lands_at_eight_the_next_local_morning() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        add_named_mode_window(&service, "strict", 9, Utc::now());

        // 9-17 -> 10-17 frees up 9-10, so it's a reduction.
        let change = service
            .add_window(
                1,
                GUILD,
                "work",
                10,
                0,
                17,
                0,
                Some("America/New_York"),
                None,
                Some("strict"),
                noon_et(),
            )
            .unwrap();

        let CurfewWindowChange::Staged { effective_at, .. } = change else {
            panic!("expected a staged edit, got {change:?}");
        };
        let expected = chrono_tz::America::New_York
            .with_ymd_and_hms(2026, 1, 2, 8, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(effective_at, expected);
    }

    #[test]
    fn test_extending_a_strict_window_applies_immediately() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        add_named_mode_window(&service, "strict", 9, Utc::now());

        // 9-17 -> 8-18 only adds curfewed time, so it can tighten today.
        let change = service
            .add_window(
                1,
                GUILD,
                "work",
                8,
                0,
                18,
                0,
                Some("America/New_York"),
                None,
                Some("strict"),
                noon_et(),
            )
            .unwrap();

        assert!(
            matches!(change, CurfewWindowChange::Applied(_)),
            "an extension must not wait for the morning: {change:?}"
        );
        let live = repository.get_window(1, GUILD, "work").unwrap().unwrap();
        assert_eq!((live.start_hour, live.end_hour), (8, 18));
        assert!(service.pending_changes(1, GUILD).unwrap().is_empty());
    }

    #[test]
    fn test_adding_a_day_applies_immediately_but_dropping_one_is_staged() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(
                1,
                GUILD,
                "work",
                9,
                0,
                17,
                0,
                Some("America/New_York"),
                Some("Mon,Tue"),
                Some("strict"),
                Utc::now(),
            )
            .unwrap();

        let widened = service
            .add_window(
                1,
                GUILD,
                "work",
                9,
                0,
                17,
                0,
                Some("America/New_York"),
                Some("Mon,Tue,Wed"),
                Some("strict"),
                noon_et(),
            )
            .unwrap();
        assert!(
            matches!(widened, CurfewWindowChange::Applied(_)),
            "{widened:?}"
        );

        let narrowed = service
            .add_window(
                1,
                GUILD,
                "work",
                9,
                0,
                17,
                0,
                Some("America/New_York"),
                Some("Mon"),
                Some("strict"),
                noon_et(),
            )
            .unwrap();
        assert!(
            matches!(narrowed, CurfewWindowChange::Staged { .. }),
            "{narrowed:?}"
        );
    }

    #[test]
    fn test_switching_a_strict_window_to_default_mode_is_staged() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        add_named_mode_window(&service, "strict", 9, Utc::now());

        let change = service
            .add_window(
                1,
                GUILD,
                "work",
                9,
                0,
                17,
                0,
                Some("America/New_York"),
                None,
                Some("default"),
                noon_et(),
            )
            .unwrap();
        assert!(
            matches!(change, CurfewWindowChange::Staged { .. }),
            "dropping to default loosens the guard, so it waits: {change:?}"
        );
        assert_eq!(
            repository
                .get_window(1, GUILD, "work")
                .unwrap()
                .unwrap()
                .mode,
            CurfewMode::Strict
        );
    }
}
