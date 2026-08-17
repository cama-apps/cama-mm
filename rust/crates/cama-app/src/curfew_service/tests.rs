use cama_db::curfew::CurfewRepository;
use cama_db::schema_manager::initialize_or_migrate;
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

        let window = service
            .add_window(1, GUILD, "work", 9, 0, 17, 0, None, None)
            .unwrap();

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

        let window = service
            .add_window(1, GUILD, "  work  ", 9, 0, 17, 0, None, None)
            .unwrap();

        assert_eq!(window.name, "work");
    }

    #[test]
    fn test_add_window_rejects_empty_name() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let error = service
            .add_window(1, GUILD, "   ", 9, 0, 17, 0, None, None)
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::EmptyName));
    }

    #[test]
    fn test_add_window_rejects_too_long_name() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let long_name = "x".repeat(41);
        let error = service
            .add_window(1, GUILD, &long_name, 9, 0, 17, 0, None, None)
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::NameTooLong));
    }

    #[test]
    fn test_add_window_rejects_out_of_range_hour() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let error = service
            .add_window(1, GUILD, "work", 24, 0, 17, 0, None, None)
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::InvalidHour));
    }

    #[test]
    fn test_add_window_rejects_equal_start_and_end() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let error = service
            .add_window(1, GUILD, "work", 9, 0, 9, 0, None, None)
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::EqualStartAndEnd));
    }

    #[test]
    fn test_add_window_rejects_unknown_timezone() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);

        let error = service
            .add_window(1, GUILD, "work", 9, 0, 17, 0, Some("Not/AZone"), None)
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::InvalidTimezone(_)));
    }

    #[test]
    fn test_add_window_unregistered_player_raises() {
        let (_dir, service, _repository) = fixture();

        let error = service
            .add_window(999, GUILD, "work", 9, 0, 17, 0, None, None)
            .unwrap_err();
        assert!(matches!(error, CurfewServiceError::PlayerNotRegistered));
    }

    #[test]
    fn test_add_window_replaces_existing_name() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(1, GUILD, "work", 9, 0, 17, 0, None, None)
            .unwrap();

        service
            .add_window(1, GUILD, "work", 8, 0, 16, 0, None, None)
            .unwrap();

        let windows = repository.list_for_player(1, GUILD).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].start_hour, 8);
    }
}

mod remove_and_list_windows {
    use super::*;

    #[test]
    fn test_remove_window() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(1, GUILD, "work", 9, 0, 17, 0, None, None)
            .unwrap();

        assert!(service.remove_window(1, GUILD, "work").unwrap());
        assert!(service.list_windows(1, GUILD).unwrap().is_empty());
    }

    #[test]
    fn test_remove_missing_window() {
        let (_dir, service, _repository) = fixture();
        assert!(!service.remove_window(1, GUILD, "nope").unwrap());
    }

    #[test]
    fn test_list_windows_multiple() {
        let (dir, service, repository) = fixture();
        insert_player(&repository, &dir, 1);
        service
            .add_window(1, GUILD, "work", 9, 0, 17, 0, None, None)
            .unwrap();
        service
            .add_window(1, GUILD, "sleep", 22, 0, 6, 0, None, None)
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
            .add_window(1, GUILD, "sleep", 22, 0, 6, 0, None, None)
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
            .add_window(1, GUILD, "sleep", 22, 0, 6, 0, None, None)
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
            .add_window(1, GUILD, "sleep", 22, 0, 6, 0, None, None)
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

        assert!(service.sweep(&lobby, &[GUILD], eleven_pm_et()).is_empty());
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
}
