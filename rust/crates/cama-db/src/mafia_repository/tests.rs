use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 42_424;
const OTHER_GUILD: i64 = 42_425;

struct Fixture {
    database: NamedTempFile,
    repository: MafiaRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary Mafia database");
        Connection::open(database.path())
            .expect("open fixture")
            .execute_batch(FIXTURE_SCHEMA)
            .expect("create disposable Python-compatible schema");
        let repository = MafiaRepository::new(database.path());
        Self {
            database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.database.path()).expect("open fixture")
    }

    fn seed_player(&self, discord_id: i64, guild_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance,lowest_balance_ever)
                 VALUES (?1,?2,?3,?4,?4)
                 ON CONFLICT(discord_id,guild_id) DO UPDATE SET jopacoin_balance=excluded.jopacoin_balance",
                params![discord_id, guild_id, format!("user_{discord_id}"), balance],
            )
            .expect("seed player");
    }

    fn seed_fund(&self, guild_id: i64, amount: i64) {
        self.connection()
            .execute(
                "INSERT INTO nonprofit_fund(guild_id,total_collected)
                 VALUES (?1,?2) ON CONFLICT(guild_id) DO UPDATE SET
                 total_collected=total_collected+excluded.total_collected",
                params![guild_id, amount],
            )
            .expect("seed fund");
    }

    fn game(&self, phase: MafiaPhase) -> i64 {
        self.repository
            .create_game(Some(GUILD), "2026-04-24", phase, 0, 5, None)
            .expect("create game")
    }
}

fn player(game_id: i64, discord_id: i64, role: MafiaRole) -> MafiaPlayer {
    MafiaPlayer::new(game_id, discord_id, GUILD, role)
}

fn action(
    game_id: i64,
    actor_id: i64,
    target_id: i64,
    action_type: MafiaActionType,
) -> MafiaAction {
    MafiaAction {
        game_id,
        guild_id: GUILD,
        actor_id,
        target_id: Some(target_id),
        action_type,
        phase: MafiaPhase::Night,
        day_number: 1,
        result: None,
    }
}

fn signup_roster(fixture: &Fixture, ids: &[i64]) -> Vec<MafiaPlayer> {
    ids.iter()
        .map(|discord_id| {
            fixture.seed_player(*discord_id, GUILD, 100);
            fixture
                .repository
                .add_signup(Some(GUILD), *discord_id)
                .expect("signup");
            player(0, *discord_id, MafiaRole::Townie)
        })
        .collect()
}

fn create_from_signups(
    fixture: &Fixture,
    roster: &[MafiaPlayer],
) -> Result<i64, MafiaRepositoryError> {
    fixture
        .repository
        .create_game_with_players_and_entry_fees(MafiaGameStart {
            guild_id: Some(GUILD),
            game_date: "2026-04-24",
            phase: MafiaPhase::Night,
            started_at: 1_000,
            roster_size: i64::try_from(roster.len()).expect("small roster"),
            twist_event: None,
            entry_fee: 8,
            players: roster,
            max_debt: 500,
        })
}

#[test]
fn test_create_and_fetch_game() {
    let fixture = Fixture::new();
    let game_id = fixture
        .repository
        .create_game(
            Some(GUILD),
            "2026-04-24",
            MafiaPhase::Night,
            1_000,
            8,
            Some(MafiaTwist::BloodMoon),
        )
        .expect("create");
    let game = fixture
        .repository
        .get_game_by_id(game_id)
        .expect("fetch")
        .expect("game");
    assert_eq!(game.phase, MafiaPhase::Night);
    assert_eq!(game.twist_event, Some(MafiaTwist::BloodMoon));
    assert_eq!(game.roster_size, 8);
    assert_eq!(
        fixture
            .repository
            .get_game_for_date(Some(GUILD), "2026-04-24")
            .expect("date")
            .expect("same")
            .game_id,
        game_id
    );
}

#[test]
fn test_multiple_games_per_date_allowed() {
    let fixture = Fixture::new();
    let first = fixture.game(MafiaPhase::Night);
    let second = fixture.game(MafiaPhase::Night);
    assert_ne!(first, second);
}

#[test]
fn test_atomic_game_start_consumes_selected_signups() {
    let fixture = Fixture::new();
    let roster = signup_roster(&fixture, &[100, 101, 102, 103, 104]);
    create_from_signups(&fixture, &roster).expect("atomic start");
    assert!(
        fixture
            .repository
            .get_signups(Some(GUILD))
            .expect("signups")
            .is_empty()
    );
}

#[test]
fn test_atomic_game_start_rejects_roster_invalidated_by_optout() {
    let fixture = Fixture::new();
    let roster = signup_roster(&fixture, &[100, 101, 102, 103, 104]);
    fixture
        .repository
        .set_optout(Some(GUILD), 104, true)
        .expect("opt out");
    assert!(matches!(
        create_from_signups(&fixture, &roster),
        Err(MafiaRepositoryError::Invalid("signup_claim_failed"))
    ));
    assert!(
        fixture
            .repository
            .get_active_game(Some(GUILD))
            .expect("active")
            .is_none()
    );
    for id in 100..105 {
        assert_eq!(
            fixture
                .repository
                .player_balance(id, Some(GUILD))
                .expect("balance"),
            100
        );
    }
}

#[test]
fn test_atomic_game_start_rejects_second_active_game_without_debit() {
    let fixture = Fixture::new();
    let roster = signup_roster(&fixture, &[100, 101, 102, 103, 104]);
    let active = fixture.game(MafiaPhase::Night);
    assert!(matches!(
        create_from_signups(&fixture, &roster),
        Err(MafiaRepositoryError::Invalid("game_already_active"))
    ));
    assert_eq!(
        fixture
            .repository
            .get_active_game(Some(GUILD))
            .expect("active")
            .expect("game")
            .game_id,
        active
    );
    for id in 100..105 {
        assert_eq!(
            fixture
                .repository
                .player_balance(id, Some(GUILD))
                .expect("balance"),
            100
        );
    }
}

#[test]
fn test_phase_reminder_claim_is_durable_and_releasable() {
    let fixture = Fixture::new();
    let game_id = fixture.game(MafiaPhase::Night);
    let second = MafiaRepository::new(fixture.database.path());
    assert!(
        fixture
            .repository
            .claim_phase_reminder(Some(GUILD), game_id, 1, MafiaPhase::Night)
            .expect("claim")
    );
    assert!(
        !second
            .claim_phase_reminder(Some(GUILD), game_id, 1, MafiaPhase::Night)
            .expect("duplicate")
    );
    fixture
        .repository
        .release_phase_reminder(Some(GUILD), game_id, 1, MafiaPhase::Night)
        .expect("release");
    assert!(
        second
            .claim_phase_reminder(Some(GUILD), game_id, 1, MafiaPhase::Night)
            .expect("reclaim")
    );
}

#[test]
fn test_get_active_game_excludes_resolved() {
    let fixture = Fixture::new();
    let resolved = fixture.game(MafiaPhase::Resolved);
    fixture
        .repository
        .finalize_game(resolved, MafiaWinner::Town, 50, None)
        .expect("finalize");
    let active = fixture.game(MafiaPhase::Night);
    assert_eq!(
        fixture
            .repository
            .get_active_game(Some(GUILD))
            .expect("active")
            .expect("game")
            .game_id,
        active
    );
}

#[test]
fn test_set_phase_and_thread_ids() {
    let fixture = Fixture::new();
    let game_id = fixture.game(MafiaPhase::Night);
    fixture
        .repository
        .set_phase(game_id, MafiaPhase::Day, Some(999), None)
        .expect("phase");
    fixture
        .repository
        .set_thread_ids(
            game_id,
            ThreadIds {
                mafia_thread_id: Some(111),
                discussion_thread_id: Some(222),
                setup_message_id: Some(333),
                ..ThreadIds::default()
            },
        )
        .expect("threads");
    let game = fixture
        .repository
        .get_game_by_id(game_id)
        .expect("fetch")
        .expect("game");
    assert_eq!(
        (game.phase, game.night_ended_at),
        (MafiaPhase::Day, Some(999))
    );
    assert_eq!(
        (
            game.mafia_thread_id,
            game.discussion_thread_id,
            game.setup_message_id
        ),
        (Some(111), Some(222), Some(333))
    );
}

#[test]
fn test_add_and_get_players() {
    let fixture = Fixture::new();
    let game_id = fixture.game(MafiaPhase::Night);
    let mut mafia = player(game_id, 1_001, MafiaRole::Mafia);
    mafia.is_godfather = true;
    mafia.hero_name = Some("Pudge".into());
    let mut town = player(game_id, 1_002, MafiaRole::Townie);
    town.hero_name = Some("CM".into());
    fixture
        .repository
        .add_players(game_id, &[mafia, town])
        .expect("players");
    let fetched = fixture.repository.get_players(game_id).expect("fetch");
    assert_eq!(fetched.len(), 2);
    assert!(fetched[0].is_godfather);
    assert_eq!(fetched[0].role, MafiaRole::Mafia);
    assert_eq!(fetched[1].hero_name.as_deref(), Some("CM"));
}

#[test]
fn test_get_alive_players_filters() {
    let fixture = Fixture::new();
    let game_id = fixture.game(MafiaPhase::Night);
    fixture
        .repository
        .add_players(
            game_id,
            &[
                player(game_id, 1, MafiaRole::Mafia),
                player(game_id, 2, MafiaRole::Townie),
                player(game_id, 3, MafiaRole::Townie),
            ],
        )
        .expect("players");
    fixture
        .repository
        .set_player_alive(game_id, 2, false, Some(MafiaPhase::Night))
        .expect("kill");
    let alive = fixture
        .repository
        .get_alive_players(game_id, None)
        .expect("alive");
    assert_eq!(
        alive.iter().map(|p| p.discord_id).collect::<BTreeSet<_>>(),
        BTreeSet::from([1, 3])
    );
    let town = fixture
        .repository
        .get_alive_players(game_id, Some(MafiaRole::Townie))
        .expect("town");
    assert_eq!(town.iter().map(|p| p.discord_id).collect::<Vec<_>>(), [3]);
}

#[test]
fn test_record_action_upsert() {
    let fixture = Fixture::new();
    let game_id = fixture.game(MafiaPhase::Night);
    fixture
        .repository
        .add_players(
            game_id,
            &[
                player(game_id, 1, MafiaRole::Mafia),
                player(game_id, 2, MafiaRole::Townie),
                player(game_id, 3, MafiaRole::Townie),
            ],
        )
        .expect("players");
    fixture
        .repository
        .record_action(&action(game_id, 1, 2, MafiaActionType::Kill))
        .expect("first");
    fixture
        .repository
        .record_action(&action(game_id, 1, 3, MafiaActionType::Kill))
        .expect("upsert");
    let actions = fixture
        .repository
        .get_actions(
            game_id,
            Some(MafiaActionType::Kill),
            Some(MafiaPhase::Night),
        )
        .expect("actions");
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].target_id, Some(3));
    assert!(
        fixture
            .repository
            .get_player(game_id, 1)
            .expect("actor")
            .expect("player")
            .acted
    );
}

#[test]
fn test_get_actions_filters_by_type_and_phase() {
    let fixture = Fixture::new();
    let game_id = fixture.game(MafiaPhase::Night);
    fixture
        .repository
        .add_players(
            game_id,
            &[
                player(game_id, 1, MafiaRole::Mafia),
                player(game_id, 2, MafiaRole::Detective),
            ],
        )
        .expect("players");
    fixture
        .repository
        .record_action(&action(game_id, 1, 2, MafiaActionType::Kill))
        .expect("kill");
    let mut investigate = action(game_id, 2, 1, MafiaActionType::Investigate);
    investigate.result = Some("Mafia".into());
    fixture
        .repository
        .record_action(&investigate)
        .expect("investigate");
    assert_eq!(
        fixture
            .repository
            .get_actions(game_id, Some(MafiaActionType::Kill), None)
            .expect("kills")
            .len(),
        1
    );
    assert_eq!(
        fixture
            .repository
            .get_actions(
                game_id,
                Some(MafiaActionType::Investigate),
                Some(MafiaPhase::Night)
            )
            .expect("investigations")[0]
            .result
            .as_deref(),
        Some("Mafia")
    );
}

#[test]
fn test_eligible_signups_are_opt_in_only_and_guild_scoped() {
    let fixture = Fixture::new();
    for id in [100, 200, 300] {
        fixture.seed_player(id, GUILD, 100);
    }
    fixture.seed_player(400, OTHER_GUILD, 100);
    for id in [100, 200] {
        fixture
            .repository
            .add_signup(Some(GUILD), id)
            .expect("signup");
    }
    fixture
        .repository
        .add_signup(Some(OTHER_GUILD), 400)
        .expect("other");
    assert_eq!(
        fixture
            .repository
            .get_eligible_signups(Some(GUILD), 8, Some(500))
            .expect("eligible"),
        [100, 200]
    );
}

#[test]
fn test_eligible_signups_exclude_unregistered_opted_out_and_over_debt() {
    let fixture = Fixture::new();
    fixture.seed_player(100, GUILD, 100);
    fixture.seed_player(200, GUILD, 100);
    fixture.seed_player(300, GUILD, -495);
    for id in [100, 200, 300, 400] {
        fixture
            .repository
            .add_signup(Some(GUILD), id)
            .expect("signup");
    }
    fixture
        .repository
        .set_optout(Some(GUILD), 200, true)
        .expect("optout");
    assert_eq!(
        fixture
            .repository
            .get_eligible_signups(Some(GUILD), 8, Some(500))
            .expect("eligible"),
        [100]
    );
}

#[test]
fn test_optout_toggle() {
    let fixture = Fixture::new();
    assert!(
        !fixture
            .repository
            .is_opted_out(Some(GUILD), 100)
            .expect("initial")
    );
    fixture
        .repository
        .set_optout(Some(GUILD), 100, true)
        .expect("on");
    assert!(
        fixture
            .repository
            .is_opted_out(Some(GUILD), 100)
            .expect("set")
    );
    assert_eq!(
        fixture
            .repository
            .get_opted_out_player_ids(Some(GUILD))
            .expect("ids"),
        BTreeSet::from([100])
    );
    fixture
        .repository
        .set_optout(Some(GUILD), 100, false)
        .expect("off");
    assert!(
        !fixture
            .repository
            .is_opted_out(Some(GUILD), 100)
            .expect("cleared")
    );
}

#[test]
fn test_optout_cancels_pending_signup() {
    let fixture = Fixture::new();
    fixture
        .repository
        .add_signup(Some(GUILD), 100)
        .expect("signup");
    fixture
        .repository
        .set_optout(Some(GUILD), 100, true)
        .expect("optout");
    assert!(
        fixture
            .repository
            .get_signups(Some(GUILD))
            .expect("signups")
            .is_empty()
    );
}

#[test]
fn test_remove_signups_consumes_only_rostered_players() {
    let fixture = Fixture::new();
    for id in [100, 200, 300] {
        fixture
            .repository
            .add_signup(Some(GUILD), id)
            .expect("signup");
    }
    fixture
        .repository
        .remove_signups(Some(GUILD), &[100, 200])
        .expect("remove");
    assert_eq!(
        fixture
            .repository
            .get_signups(Some(GUILD))
            .expect("remaining"),
        [300]
    );
}

#[test]
fn test_finalize_and_leaderboard() {
    let fixture = Fixture::new();
    let first = fixture.game(MafiaPhase::Night);
    fixture
        .repository
        .add_players(
            first,
            &[
                player(first, 100, MafiaRole::Townie),
                player(first, 200, MafiaRole::Mafia),
            ],
        )
        .expect("players");
    fixture
        .repository
        .finalize_game(first, MafiaWinner::Town, 40, Some(100))
        .expect("first");
    let second = fixture.game(MafiaPhase::Night);
    fixture
        .repository
        .add_players(
            second,
            &[
                player(second, 100, MafiaRole::Townie),
                player(second, 200, MafiaRole::Mafia),
            ],
        )
        .expect("players");
    fixture
        .repository
        .finalize_game(second, MafiaWinner::Mafia, 40, Some(200))
        .expect("second");
    let rows = fixture
        .repository
        .get_leaderboard(Some(GUILD), 10)
        .expect("leaderboard");
    let by_id = rows
        .into_iter()
        .map(|row| (row.discord_id, row))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        (
            by_id[&100].games_played,
            by_id[&100].wins,
            by_id[&100].mvp_count
        ),
        (2, 1, 1)
    );
    assert_eq!(by_id[&200].mafia_wins, 1);
}

#[test]
fn test_compute_player_stats_correct_reads() {
    let fixture = Fixture::new();
    let game_id = fixture.game(MafiaPhase::Night);
    let detective = player(game_id, 100, MafiaRole::Detective);
    let mafia = player(game_id, 200, MafiaRole::Mafia);
    fixture
        .repository
        .add_players(
            game_id,
            &[detective, mafia, player(game_id, 300, MafiaRole::Townie)],
        )
        .expect("players");
    let mut read = action(game_id, 100, 200, MafiaActionType::Investigate);
    read.result = Some("Mafia".into());
    fixture.repository.record_action(&read).expect("read");
    fixture
        .repository
        .set_player_alive(game_id, 200, false, Some(MafiaPhase::Day))
        .expect("lynch");
    fixture
        .repository
        .finalize_game(game_id, MafiaWinner::Town, 40, Some(100))
        .expect("finalize");
    let stats = fixture
        .repository
        .compute_player_stats(Some(GUILD), 100)
        .expect("stats");
    assert_eq!(
        (stats.correct_reads, stats.wins, stats.town_wins),
        (1, 1, 1)
    );
}

fn bounty_fixture(balance: i64) -> (Fixture, i64) {
    let fixture = Fixture::new();
    let game_id = fixture.game(MafiaPhase::Day);
    fixture.seed_player(10, GUILD, balance);
    (fixture, game_id)
}

#[test]
fn test_add_bounty_debits_contributor_and_parks_in_fund() {
    let (fixture, game_id) = bounty_fixture(5);
    assert_eq!(
        fixture
            .repository
            .add_bounty(game_id, Some(GUILD), 1, 99, 10, 500)
            .expect("bounty"),
        AddBountyResult::Added
    );
    assert_eq!(
        fixture
            .repository
            .player_balance(10, Some(GUILD))
            .expect("balance"),
        4
    );
    assert_eq!(
        fixture
            .repository
            .nonprofit_fund(Some(GUILD))
            .expect("fund"),
        1
    );
    assert_eq!(
        fixture.repository.bounty_count(game_id, 1).expect("count"),
        1
    );
}

#[test]
fn test_add_bounty_rejects_duplicate_with_no_double_debit() {
    let (fixture, game_id) = bounty_fixture(5);
    assert_eq!(
        fixture
            .repository
            .add_bounty(game_id, Some(GUILD), 1, 99, 10, 500)
            .expect("first"),
        AddBountyResult::Added
    );
    assert_eq!(
        fixture
            .repository
            .add_bounty(game_id, Some(GUILD), 1, 99, 10, 500)
            .expect("second"),
        AddBountyResult::AlreadyStaked
    );
    assert_eq!(
        (
            fixture
                .repository
                .player_balance(10, Some(GUILD))
                .expect("balance"),
            fixture
                .repository
                .nonprofit_fund(Some(GUILD))
                .expect("fund")
        ),
        (4, 1)
    );
}

#[test]
fn test_add_bounty_respects_debt_floor() {
    let (fixture, game_id) = bounty_fixture(-500);
    assert_eq!(
        fixture
            .repository
            .add_bounty(game_id, Some(GUILD), 1, 99, 10, 500)
            .expect("bounty"),
        AddBountyResult::InsufficientFunds
    );
    assert_eq!(
        fixture
            .repository
            .player_balance(10, Some(GUILD))
            .expect("balance"),
        -500
    );
    assert_eq!(
        fixture
            .repository
            .nonprofit_fund(Some(GUILD))
            .expect("fund"),
        0
    );
    assert_eq!(
        fixture.repository.bounty_count(game_id, 1).expect("count"),
        0
    );
}

fn stake(fixture: &Fixture, game_id: i64, contributor: i64) {
    fixture.seed_player(contributor, GUILD, 5);
    assert_eq!(
        fixture
            .repository
            .add_bounty(game_id, Some(GUILD), 1, 99, contributor, 500)
            .expect("stake"),
        AddBountyResult::Added
    );
}

#[test]
fn test_resolve_day_bounties_pays_correct_read_and_conserves() {
    let fixture = Fixture::new();
    let game_id = fixture.game(MafiaPhase::Day);
    stake(&fixture, game_id, 10);
    stake(&fixture, game_id, 11);
    let before = BTreeMap::from([(10, 4), (11, 4)]);
    let result = fixture
        .repository
        .resolve_day_bounties(game_id, Some(GUILD), 1, Some(99), true, 4)
        .expect("resolve");
    assert_eq!(result.reward, 2);
    assert_eq!(
        result.paid.keys().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([10, 11])
    );
    assert_eq!(result.paid.values().sum::<i64>(), 2);
    assert_eq!(
        fixture
            .repository
            .nonprofit_fund(Some(GUILD))
            .expect("fund"),
        0
    );
    for id in [10, 11] {
        assert_eq!(
            fixture
                .repository
                .player_balance(id, Some(GUILD))
                .expect("balance")
                - before[&id],
            result.paid[&id]
        );
    }
}

#[test]
fn test_resolve_day_bounties_wrong_read_forfeits_stakes() {
    let fixture = Fixture::new();
    let game_id = fixture.game(MafiaPhase::Day);
    stake(&fixture, game_id, 10);
    let result = fixture
        .repository
        .resolve_day_bounties(game_id, Some(GUILD), 1, Some(99), false, 4)
        .expect("resolve");
    assert_eq!(result, BountyResolution::default());
    assert_eq!(
        (
            fixture
                .repository
                .player_balance(10, Some(GUILD))
                .expect("balance"),
            fixture
                .repository
                .nonprofit_fund(Some(GUILD))
                .expect("fund")
        ),
        (4, 1)
    );
}

#[test]
fn test_resolve_day_bounties_caps_reward_at_alive_count() {
    let fixture = Fixture::new();
    let game_id = fixture.game(MafiaPhase::Day);
    for id in 10..15 {
        stake(&fixture, game_id, id);
    }
    let result = fixture
        .repository
        .resolve_day_bounties(game_id, Some(GUILD), 1, Some(99), true, 2)
        .expect("resolve");
    assert_eq!(result.reward, 2);
    assert_eq!(result.paid.values().sum::<i64>(), 2);
    assert_eq!(
        fixture
            .repository
            .nonprofit_fund(Some(GUILD))
            .expect("fund"),
        3
    );
}

#[test]
fn test_resolve_day_bounties_cannot_drain_unrelated_fund_revenue() {
    let fixture = Fixture::new();
    let game_id = fixture.game(MafiaPhase::Day);
    stake(&fixture, game_id, 10);
    fixture.seed_fund(GUILD, 50);
    let result = fixture
        .repository
        .resolve_day_bounties(game_id, Some(GUILD), 1, Some(99), true, 8)
        .expect("resolve");
    assert_eq!(result.reward, 1);
    assert_eq!(result.paid, BTreeMap::from([(10, 1)]));
    assert_eq!(
        fixture
            .repository
            .player_balance(10, Some(GUILD))
            .expect("balance"),
        5
    );
    assert_eq!(
        fixture
            .repository
            .nonprofit_fund(Some(GUILD))
            .expect("fund"),
        50
    );
}

#[test]
fn test_add_bounty_is_guild_isolated() {
    let (fixture, game_id) = bounty_fixture(5);
    fixture.seed_player(10, OTHER_GUILD, 5);
    assert_eq!(
        fixture
            .repository
            .add_bounty(game_id, Some(GUILD), 1, 99, 10, 500)
            .expect("bounty"),
        AddBountyResult::Added
    );
    assert_eq!(
        (
            fixture
                .repository
                .nonprofit_fund(Some(GUILD))
                .expect("fund a"),
            fixture
                .repository
                .nonprofit_fund(Some(OTHER_GUILD))
                .expect("fund b")
        ),
        (1, 0)
    );
    assert_eq!(
        (
            fixture
                .repository
                .player_balance(10, Some(GUILD))
                .expect("balance a"),
            fixture
                .repository
                .player_balance(10, Some(OTHER_GUILD))
                .expect("balance b")
        ),
        (4, 5)
    );
}

/// Manual cross-language smoke: Python creates and seeds a fully migrated
/// disposable database, Rust reads that state and writes through the same
/// existing schema, then Python verifies the resulting rows. Never point this
/// at the live/root database.
#[test]
#[ignore = "requires CAMA_MAFIA_INTEROP_DB pointing at a disposable Python-migrated database"]
fn interop_python_seeded_mafia_repository_roundtrip() {
    let path = std::env::var("CAMA_MAFIA_INTEROP_DB").expect("interop database path");
    let repository = MafiaRepository::new(path);
    let game = repository
        .get_game_for_date(Some(GUILD), "2099-08-11")
        .expect("read Python game")
        .expect("seeded Python game");
    assert_eq!(game.phase, MafiaPhase::Night);
    assert_eq!(game.twist_event, Some(MafiaTwist::BloodMoon));
    assert!(
        repository
            .claim_phase_reminder(Some(GUILD), game.game_id, 1, MafiaPhase::Night)
            .expect("claim reminder")
    );
    repository
        .set_phase(game.game_id, MafiaPhase::Day, Some(9_999), None)
        .expect("advance phase");
    repository
        .set_thread_ids(
            game.game_id,
            ThreadIds {
                mafia_thread_id: Some(111_111),
                discussion_thread_id: Some(222_222),
                setup_message_id: Some(333_333),
                ..ThreadIds::default()
            },
        )
        .expect("write thread ids");
    assert_eq!(
        repository
            .add_bounty(game.game_id, Some(GUILD), 1, 99, 10, 500)
            .expect("stake bounty"),
        AddBountyResult::Added
    );
}

const FIXTURE_SCHEMA: &str = r#"
PRAGMA foreign_keys=OFF;
CREATE TABLE players (
 discord_id INTEGER NOT NULL, guild_id INTEGER NOT NULL,
 discord_username TEXT NOT NULL, jopacoin_balance INTEGER DEFAULT 3,
 lowest_balance_ever INTEGER, updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(discord_id,guild_id)
);
CREATE TABLE nonprofit_fund (
 guild_id INTEGER PRIMARY KEY, total_collected INTEGER NOT NULL DEFAULT 0,
 updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE economy_ledger_context (
 id INTEGER PRIMARY KEY, source TEXT, actor_id INTEGER, related_type TEXT,
 related_id TEXT, reason TEXT, metadata TEXT
);
CREATE TABLE mafia_games (
 game_id INTEGER PRIMARY KEY AUTOINCREMENT, guild_id INTEGER NOT NULL DEFAULT 0,
 game_date TEXT NOT NULL, phase TEXT NOT NULL, started_at INTEGER NOT NULL,
 night_ended_at INTEGER, day_ended_at INTEGER, winner TEXT,
 entry_fee INTEGER NOT NULL DEFAULT 0, payout_per_winner INTEGER NOT NULL DEFAULT 0,
 mvp_id INTEGER, roster_size INTEGER NOT NULL, twist_event TEXT,
 mafia_thread_id INTEGER, discussion_thread_id INTEGER, setup_message_id INTEGER,
 day_number INTEGER NOT NULL DEFAULT 1, phase_started_at INTEGER,
 standings_message_id INTEGER, graveyard_thread_id INTEGER,
 mafia_intro_message_id INTEGER, graveyard_intro_message_id INTEGER,
 resolution_message_id INTEGER,
 resolution_summary TEXT,
 status TEXT NOT NULL DEFAULT 'ACTIVE'
);
CREATE TABLE mafia_players (
 game_id INTEGER NOT NULL, discord_id INTEGER NOT NULL, guild_id INTEGER NOT NULL DEFAULT 0,
 role TEXT NOT NULL, is_godfather INTEGER NOT NULL DEFAULT 0, hero_name TEXT,
 is_alive INTEGER NOT NULL DEFAULT 1, eliminated_phase TEXT, eliminated_at INTEGER,
 acted INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(game_id,discord_id)
);
CREATE TABLE mafia_actions (
 action_id INTEGER PRIMARY KEY AUTOINCREMENT, game_id INTEGER NOT NULL,
 guild_id INTEGER NOT NULL DEFAULT 0, actor_id INTEGER NOT NULL, target_id INTEGER,
 action_type TEXT NOT NULL, phase TEXT NOT NULL, day_number INTEGER NOT NULL DEFAULT 1,
 created_at INTEGER NOT NULL, result TEXT,
 UNIQUE(game_id,actor_id,action_type,phase,day_number)
);
CREATE TABLE mafia_optout (
 discord_id INTEGER NOT NULL, guild_id INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(discord_id,guild_id)
);
CREATE TABLE mafia_signups (
 guild_id INTEGER NOT NULL DEFAULT 0, week_start TEXT NOT NULL,
 discord_id INTEGER NOT NULL, created_at INTEGER NOT NULL,
 PRIMARY KEY(guild_id,week_start,discord_id)
);
CREATE TABLE mafia_phase_reminders (
 guild_id INTEGER NOT NULL DEFAULT 0, game_id INTEGER NOT NULL,
 day_number INTEGER NOT NULL, phase TEXT NOT NULL, claimed_at INTEGER NOT NULL,
 PRIMARY KEY(guild_id,game_id,day_number,phase)
);
CREATE TABLE mafia_bounties (
 game_id INTEGER NOT NULL, guild_id INTEGER NOT NULL DEFAULT 0,
 day_number INTEGER NOT NULL, target_id INTEGER NOT NULL,
 contributor_id INTEGER NOT NULL, amount INTEGER NOT NULL DEFAULT 1,
 created_at INTEGER NOT NULL,
 PRIMARY KEY(game_id,day_number,target_id,contributor_id)
);
"#;
