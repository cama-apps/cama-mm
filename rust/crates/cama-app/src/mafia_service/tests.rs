use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Barrier};
use std::thread;

use super::*;

const GUILD: GuildId = 111;
const OTHER_GUILD: GuildId = 222;

fn service() -> (MafiaService, MemoryMafiaRepository) {
    let repository = MemoryMafiaRepository::default();
    (MafiaService::new(repository.clone(), 42), repository)
}

fn roster(guild_id: GuildId, roles: &[(PlayerId, Role, bool)]) -> Vec<Player> {
    roles
        .iter()
        .map(|&(id, role, godfather)| {
            let mut player = Player::new(id, guild_id, role);
            player.is_godfather = godfather;
            player
        })
        .collect()
}

fn base_roles() -> Vec<Player> {
    roster(
        GUILD,
        &[
            (1, Role::Mafia, true),
            (2, Role::Townie, false),
            (3, Role::Doctor, false),
            (4, Role::Detective, false),
            (5, Role::Townie, false),
        ],
    )
}

fn undecided_roles() -> Vec<Player> {
    roster(
        GUILD,
        &[
            (1, Role::Mafia, true),
            (2, Role::Townie, false),
            (3, Role::Detective, false),
            (4, Role::Doctor, false),
            (5, Role::Townie, false),
        ],
    )
}

fn seed_accounts(repository: &MemoryMafiaRepository, ids: &[PlayerId], balance: i64) {
    for &id in ids {
        repository.register(GUILD, id, balance);
    }
}

fn signup_accounts(repository: &MemoryMafiaRepository, ids: &[PlayerId], balance: i64) {
    seed_accounts(repository, ids, balance);
    for &id in ids {
        repository.signup(GUILD, id);
    }
}

fn day_game(repository: &MemoryMafiaRepository, players: Vec<Player>) -> GameId {
    repository.seed_game(GUILD, Phase::Day, players)
}

fn vote_for(mafia: &MafiaService, voters: &[PlayerId], target: PlayerId) {
    for &voter in voters {
        assert!(mafia.submit_day_vote(GUILD, voter, target).ok);
    }
}

#[test]
fn test_below_min_returns_none() {
    let (mafia, repository) = service();
    signup_accounts(&repository, &[1, 2, 3], 100);
    for id in 101..110 {
        repository.register(GUILD, id, 100);
    }
    assert_eq!(mafia.start_game(GUILD, "2026-04-24"), None);
    assert!(repository.active_game(GUILD).is_none());
}

#[test]
fn test_start_returns_game_created_by_concurrent_caller() {
    let (mafia, repository) = service();
    let existing_id = repository.seed_game(GUILD, Phase::Night, base_roles());
    let observed = mafia.start_game(GUILD, "2026-04-24").expect("active game");
    assert_eq!(observed.game_id, existing_id);
}

#[test]
fn test_concurrent_starts_share_one_game_and_one_entry_fee() {
    let (mafia, repository) = service();
    let ids: Vec<_> = (101..110).collect();
    signup_accounts(&repository, &ids, 100);
    let barrier = Arc::new(Barrier::new(2));
    let first_service = mafia.clone();
    let second_service = MafiaService::new(repository.clone(), 43);
    let first_barrier = barrier.clone();
    let first = thread::spawn(move || {
        first_barrier.wait();
        first_service.start_game(GUILD, "2026-04-24")
    });
    let second = thread::spawn(move || {
        barrier.wait();
        second_service.start_game(GUILD, "2026-04-24")
    });
    let first = first.join().expect("first thread").expect("first game");
    let second = second.join().expect("second thread").expect("second game");
    assert_eq!(first.game_id, second.game_id);
    assert!(ids.iter().all(|id| repository.balance(GUILD, *id) == 92));
}

#[test]
fn test_start_daily_game_debits_entry_fee_once() {
    let (mafia, repository) = service();
    let ids: Vec<_> = (101..110).collect();
    signup_accounts(&repository, &ids, 100);
    let first = mafia.start_game(GUILD, "2026-04-24").expect("game");
    let balances: Vec<_> = ids
        .iter()
        .map(|id| repository.balance(GUILD, *id))
        .collect();
    let second = mafia.start_game(GUILD, "2026-04-24").expect("same game");
    assert_eq!(first.game_id, second.game_id);
    assert_eq!(
        balances,
        ids.iter()
            .map(|id| repository.balance(GUILD, *id))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_signup_roster_excludes_players_past_entry_fee_debt_floor() {
    let (mafia, repository) = service();
    signup_accounts(&repository, &[101, 102, 103, 104, 105, 106], 100);
    repository.set_balance(GUILD, 106, -495);
    let game = mafia
        .start_game(GUILD, "2026-04-24")
        .expect("five eligible");
    let ids: BTreeSet<_> = repository
        .players(game.game_id)
        .into_iter()
        .map(|player| player.discord_id)
        .collect();
    assert_eq!(ids.len(), 5);
    assert!(!ids.contains(&106));
}

#[test]
fn test_role_assignment_table() {
    let (mafia, _) = service();
    for size in MIN_ROSTER..=MAX_ROSTER {
        let ids: Vec<_> = (100..100 + i64::try_from(size).expect("size")).collect();
        let assigned = mafia.assign_roles_with_wildcards(GUILD, &ids, false, false);
        let expected = role_counts(size);
        assert_eq!(
            assigned.iter().filter(|p| p.role == Role::Mafia).count(),
            expected.mafia
        );
        assert_eq!(
            assigned.iter().filter(|p| p.role == Role::Doctor).count(),
            expected.doctor
        );
        assert_eq!(
            assigned
                .iter()
                .filter(|p| p.role == Role::Detective)
                .count(),
            expected.detective
        );
        assert_eq!(
            assigned
                .iter()
                .filter(|p| p.role == Role::Vigilante)
                .count(),
            expected.vigilante
        );
    }
}

#[test]
fn test_godfather_always_among_mafia() {
    let (mafia, repository) = service();
    let ids: Vec<_> = (101..113).collect();
    signup_accounts(&repository, &ids, 100);
    let game = mafia.start_game(GUILD, "2026-04-24").expect("game");
    let godfathers: Vec<_> = repository
        .players(game.game_id)
        .into_iter()
        .filter(|player| player.is_godfather)
        .collect();
    assert_eq!(godfathers.len(), 1);
    assert_eq!(godfathers[0].role, Role::Mafia);
}

#[test]
fn test_oversized_roster_capped() {
    let (mafia, repository) = service();
    let ids: Vec<_> = (101..131).collect();
    signup_accounts(&repository, &ids, 100);
    assert_eq!(
        mafia
            .start_game(GUILD, "2026-04-24")
            .expect("game")
            .roster_size,
        MAX_ROSTER
    );
}

#[test]
fn test_optout_excludes_player() {
    let (mafia, repository) = service();
    let ids: Vec<_> = (101..110).collect();
    signup_accounts(&repository, &ids, 100);
    mafia.set_optout(GUILD, 105, true);
    let game = mafia.start_game(GUILD, "2026-04-24").expect("game");
    assert!(
        repository
            .players(game.game_id)
            .iter()
            .all(|player| player.discord_id != 105)
    );
}

#[test]
fn test_guild_isolation() {
    let (mafia, repository) = service();
    let ids: Vec<_> = (101..110).collect();
    signup_accounts(&repository, &ids, 100);
    assert!(mafia.start_game(GUILD, "2026-04-24").is_some());
    assert!(mafia.start_game(OTHER_GUILD, "2026-04-24").is_none());
}

#[test]
fn test_night_action_role_validation() {
    let (mafia, repository) = service();
    repository.seed_game(
        GUILD,
        Phase::Night,
        roster(
            GUILD,
            &[
                (1, Role::Mafia, true),
                (2, Role::Mafia, false),
                (3, Role::Townie, false),
            ],
        ),
    );
    assert!(!mafia.submit_night_action(GUILD, 3, 1, ActionType::Kill).ok);
    assert!(!mafia.submit_night_action(GUILD, 1, 2, ActionType::Kill).ok);
}

#[test]
fn test_detective_godfather_reads_as_town() {
    let (mafia, repository) = service();
    repository.seed_game(
        GUILD,
        Phase::Night,
        roster(
            GUILD,
            &[
                (1, Role::Detective, false),
                (2, Role::Mafia, true),
                (3, Role::Mafia, false),
                (4, Role::Detective, false),
            ],
        ),
    );
    assert_eq!(
        mafia
            .submit_night_action(GUILD, 1, 2, ActionType::Investigate)
            .result,
        Some("Town")
    );
    assert_eq!(
        mafia
            .submit_night_action(GUILD, 4, 3, ActionType::Investigate)
            .result,
        Some("Mafia")
    );
}

#[test]
fn test_detective_one_read_per_night() {
    let (mafia, repository) = service();
    let game_id = repository.seed_game(
        GUILD,
        Phase::Night,
        roster(
            GUILD,
            &[
                (1, Role::Detective, false),
                (2, Role::Mafia, true),
                (3, Role::Mafia, false),
                (4, Role::Townie, false),
            ],
        ),
    );
    let first = mafia.submit_night_action(GUILD, 1, 2, ActionType::Investigate);
    let rejected = mafia.submit_night_action(GUILD, 1, 3, ActionType::Investigate);
    let replay = mafia.submit_night_action(GUILD, 1, 2, ActionType::Investigate);
    assert!(first.ok && !rejected.ok && replay.ok);
    let reads: Vec<_> = repository
        .actions(game_id)
        .into_iter()
        .filter(|action| action.action_type == ActionType::Investigate)
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].target_id, 2);
}

#[test]
fn test_vigilante_one_shot() {
    let (mafia, repository) = service();
    repository.seed_game(
        GUILD,
        Phase::Night,
        roster(
            GUILD,
            &[
                (1, Role::Vigilante, false),
                (2, Role::Townie, false),
                (3, Role::Townie, false),
            ],
        ),
    );
    assert!(
        mafia
            .submit_night_action(GUILD, 1, 2, ActionType::VigilanteKill)
            .ok
    );
    assert!(
        !mafia
            .submit_night_action(GUILD, 1, 3, ActionType::VigilanteKill)
            .ok
    );
}

#[test]
fn test_doctor_save_blocks_kill() {
    let (mafia, repository) = service();
    let game_id = repository.seed_game(GUILD, Phase::Night, base_roles());
    assert!(mafia.submit_night_action(GUILD, 1, 2, ActionType::Kill).ok);
    assert!(mafia.submit_night_action(GUILD, 3, 2, ActionType::Save).ok);
    let result = mafia.resolve_night(GUILD);
    assert!(result.resolved && result.killed.is_empty() && result.save_blocked);
    assert!(
        repository
            .players(game_id)
            .iter()
            .find(|player| player.discord_id == 2)
            .is_some_and(|player| player.is_alive)
    );
}

#[test]
fn test_blood_moon_kills_two() {
    let (mafia, repository) = service();
    let game_id = repository.seed_game(
        GUILD,
        Phase::Night,
        roster(
            GUILD,
            &[
                (1, Role::Mafia, true),
                (2, Role::Mafia, false),
                (3, Role::Doctor, false),
                (4, Role::Townie, false),
                (5, Role::Townie, false),
                (6, Role::Townie, false),
                (7, Role::Detective, false),
            ],
        ),
    );
    repository.set_twist(game_id, Some(Twist::BloodMoon));
    assert!(mafia.submit_night_action(GUILD, 1, 4, ActionType::Kill).ok);
    assert!(mafia.submit_night_action(GUILD, 2, 5, ActionType::Kill).ok);
    assert_eq!(mafia.resolve_night(GUILD).killed.len(), 2);
}

#[test]
fn test_plague_kills_extra_unblockable() {
    let (mafia, repository) = service();
    let game_id = repository.seed_game(GUILD, Phase::Night, base_roles());
    repository.set_twist(game_id, Some(Twist::Plague));
    assert!(mafia.submit_night_action(GUILD, 1, 2, ActionType::Kill).ok);
    let result = mafia.resolve_night(GUILD);
    assert_eq!(result.killed.len(), 2);
    assert!(result.killed.iter().any(|death| death.source == "plague"));
}

#[test]
fn test_no_kill_submission_picks_random_alive_non_mafia() {
    let (mafia, repository) = service();
    repository.seed_game(GUILD, Phase::Night, base_roles());
    let result = mafia.resolve_night(GUILD);
    assert_eq!(result.killed.len(), 1);
    assert_ne!(result.killed[0].discord_id, 1);
}

#[test]
fn test_resolve_night_only_advances_once() {
    let (mafia, repository) = service();
    let game_id = repository.seed_game(GUILD, Phase::Night, base_roles());
    let first = mafia.resolve_night(GUILD);
    let dead_after_first: Vec<_> = repository
        .players(game_id)
        .into_iter()
        .filter(|player| !player.is_alive)
        .map(|player| player.discord_id)
        .collect();
    let second = mafia.resolve_night(GUILD);
    let dead_after_second: Vec<_> = repository
        .players(game_id)
        .into_iter()
        .filter(|player| !player.is_alive)
        .map(|player| player.discord_id)
        .collect();
    assert!(first.resolved && !second.resolved);
    assert_eq!(dead_after_first, dead_after_second);
}

#[test]
fn test_lynch_plurality() {
    let (mafia, repository) = service();
    day_game(&repository, base_roles());
    vote_for(&mafia, &[2, 3, 4], 1);
    assert!(mafia.submit_day_vote(GUILD, 5, 2).ok);
    let result = mafia.resolve_day(GUILD).expect("resolution");
    assert_eq!(result.lynched_id, Some(1));
    assert_eq!(result.winner, Some(Winner::Town));
}

#[test]
fn test_lynch_tie_no_lynch() {
    let (mafia, repository) = service();
    day_game(&repository, base_roles());
    assert!(mafia.submit_day_vote(GUILD, 2, 1).ok);
    assert!(mafia.submit_day_vote(GUILD, 3, 4).ok);
    assert_eq!(
        mafia.resolve_day(GUILD).expect("resolution").lynched_id,
        None
    );
}

#[test]
fn test_jester_win() {
    let (mafia, repository) = service();
    day_game(
        &repository,
        roster(
            GUILD,
            &[
                (1, Role::Mafia, true),
                (2, Role::Jester, false),
                (3, Role::Townie, false),
                (4, Role::Doctor, false),
                (5, Role::Detective, false),
            ],
        ),
    );
    vote_for(&mafia, &[1, 3, 4, 5], 2);
    let result = mafia.resolve_day(GUILD).expect("resolution");
    assert_eq!(result.winner, Some(Winner::Jester));
    assert_eq!(result.mvp_id, Some(2));
}

#[test]
fn test_town_hall_blocks_lynch() {
    let (mafia, repository) = service();
    let game_id = day_game(&repository, base_roles());
    repository.set_twist(game_id, Some(Twist::TownHall));
    vote_for(&mafia, &[2, 3, 4, 5], 1);
    let result = mafia.resolve_day(GUILD).expect("resolution");
    assert!(result.resolved);
    assert_eq!(result.lynched_id, None);
}

#[test]
fn test_payout_amount_scales() {
    let (mafia, repository) = service();
    let players = roster(
        GUILD,
        &[
            (101, Role::Mafia, true),
            (102, Role::Townie, false),
            (103, Role::Townie, false),
            (104, Role::Townie, false),
            (105, Role::Doctor, false),
            (106, Role::Detective, false),
            (107, Role::Townie, false),
        ],
    );
    let ids: Vec<_> = players.iter().map(|player| player.discord_id).collect();
    seed_accounts(&repository, &ids, -ENTRY_FEE);
    day_game(&repository, players);
    vote_for(&mafia, &[102, 103, 104, 105, 106, 107], 101);
    let result = mafia.resolve_day(GUILD).expect("resolution");
    assert_eq!(result.pot_total, pot_for_roster(7));
    assert_eq!(result.payout_per_winner, (56 - MVP_BONUS) / 6);
    assert_eq!(result.nonprofit_overflow, 0);
}

#[test]
fn test_payout_cap_overflow() {
    let plan = compute_payouts(15 * 15, &[1, 2, 3, 4], Some(1), None);
    assert!(
        plan.deltas
            .values()
            .all(|amount| *amount <= MAX_WINNER_PAYOUT)
    );
    assert_eq!(plan.deltas.values().copied().max(), Some(MAX_WINNER_PAYOUT));
    assert_eq!(
        plan.deltas.values().sum::<i64>() + plan.nonprofit_overflow,
        225
    );
    assert!(plan.nonprofit_overflow > 0);
}

#[test]
fn test_mvp_bonus_clamped_to_small_faction_pot() {
    let plan = compute_payouts(60, &[1, 2, 3], Some(1), Some(9));
    let mvp_extra = plan.deltas[&1] - plan.payout_per_winner;
    assert_eq!(mvp_extra, 10);
    assert_eq!(
        plan.deltas.values().sum::<i64>() + plan.nonprofit_overflow,
        60
    );
}

#[test]
fn test_multi_cycle_continues_when_undecided() {
    let (mafia, repository) = service();
    let game_id = repository.seed_game(GUILD, Phase::Night, undecided_roles());
    assert!(mafia.submit_night_action(GUILD, 1, 4, ActionType::Kill).ok);
    assert!(mafia.resolve_night(GUILD).resolved);
    let day = mafia.resolve_day(GUILD).expect("resolution");
    let game = repository.game(game_id).expect("game");
    assert!(day.resolved && day.continued);
    assert_eq!((game.phase, game.day_number), (Phase::Night, 2));
}

#[test]
fn test_cycle_cap_forces_tally() {
    let (mafia, repository) = service();
    let game_id = day_game(&repository, undecided_roles());
    repository.set_day_number(game_id, MAX_CYCLES);
    let result = mafia.resolve_day(GUILD).expect("resolution");
    assert!(result.resolved && !result.continued && result.cap_forced);
    assert_eq!(result.winner, Some(Winner::Town));
}

#[test]
fn test_new_game_requires_fresh_signups_after_previous_resolves() {
    let (mafia, repository) = service();
    let ids: Vec<_> = (101..110).collect();
    signup_accounts(&repository, &ids, 100);
    let first = mafia.start_game(GUILD, "2026-04-24").expect("first");
    repository.set_phase(first.game_id, Phase::Resolved);
    assert!(mafia.start_game(GUILD, "2026-04-24").is_none());
    for &id in &ids {
        assert!(mafia.join(GUILD, id).ok);
    }
    let second = mafia.start_game(GUILD, "2026-04-24").expect("second");
    assert_ne!(first.game_id, second.game_id);
    assert_eq!(first.game_date, second.game_date);
}

#[test]
fn test_phase_ready_gating() {
    let (mafia, repository) = service();
    let game_id = repository.seed_game(GUILD, Phase::Night, base_roles());
    let game = repository.game(game_id).expect("game");
    assert!(!mafia.night_ready(&game));
    assert!(mafia.submit_night_action(GUILD, 1, 2, ActionType::Kill).ok);
    assert!(mafia.submit_night_action(GUILD, 3, 1, ActionType::Save).ok);
    assert!(
        mafia
            .submit_night_action(GUILD, 4, 1, ActionType::Investigate)
            .ok
    );
    assert!(mafia.night_ready(&game));
    repository.set_phase(game_id, Phase::Day);
    let day = repository.game(game_id).expect("day");
    assert!(!mafia.day_ready(&day));
    vote_for(&mafia, &[1, 2, 3, 4, 5], 1);
    assert!(mafia.day_ready(&day));
}

#[test]
fn test_active_optout_skips_required_night_action_and_day_vote() {
    let (mafia, repository) = service();
    let game_id = repository.seed_game(GUILD, Phase::Night, base_roles());
    mafia.set_optout(GUILD, 3, true);
    let game = repository.game(game_id).expect("game");
    assert_eq!(mafia.players_needing_night_action(&game), vec![1, 4]);
    assert!(mafia.submit_night_action(GUILD, 1, 2, ActionType::Kill).ok);
    assert!(
        mafia
            .submit_night_action(GUILD, 4, 1, ActionType::Investigate)
            .ok
    );
    assert!(mafia.night_ready(&game));
    repository.set_phase(game_id, Phase::Day);
    mafia.set_optout(GUILD, 5, true);
    let day = repository.game(game_id).expect("day");
    assert_eq!(mafia.players_needing_day_vote(&day), vec![1, 2, 4]);
}

#[test]
fn test_public_status_excludes_opted_out_players_from_vote_progress() {
    let (mafia, repository) = service();
    day_game(&repository, base_roles());
    vote_for(&mafia, &[1, 2, 5], 3);
    mafia.set_optout(GUILD, 5, true);
    let status = mafia.public_status(GUILD);
    assert!(status.active);
    assert_eq!(status.voted_count, 2);
    assert_eq!(status.alive_voters, 4);
}

#[test]
fn test_join_rejects_unregistered_player() {
    let (mafia, repository) = service();
    let result = mafia.join(GUILD, 999);
    assert!(!result.ok);
    assert_eq!(result.error, Some("not_registered"));
    assert!(repository.signups(GUILD).is_empty());
}

#[test]
fn test_town_bounty_pays_on_correct_lynch() {
    let (mafia, repository) = service();
    seed_accounts(&repository, &[1, 2, 3, 4, 5], 50);
    day_game(&repository, undecided_roles());
    assert!(mafia.submit_bounty(GUILD, 2, 1).ok);
    assert!(mafia.submit_bounty(GUILD, 3, 1).ok);
    assert!(!mafia.submit_bounty(GUILD, 2, 1).ok);
    vote_for(&mafia, &[2, 3, 5], 1);
    let result = mafia.resolve_day(GUILD).expect("resolution");
    assert!(result.bounty.reward > 0);
    assert_eq!(
        result.bounty.paid.keys().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([2, 3])
    );
}

#[test]
fn test_bounty_not_paid_when_finalize_loses_race() {
    let (mafia, repository) = service();
    seed_accounts(&repository, &[1, 2, 3, 4, 5], 50);
    day_game(&repository, undecided_roles());
    assert!(mafia.submit_bounty(GUILD, 2, 1).ok);
    assert!(mafia.submit_bounty(GUILD, 3, 1).ok);
    vote_for(&mafia, &[2, 3, 5], 1);
    let fund = repository.nonprofit_fund(GUILD);
    let balances = (repository.balance(GUILD, 2), repository.balance(GUILD, 3));
    repository.lose_next_day_claim();
    let result = mafia.resolve_day(GUILD).expect("lost claim");
    assert!(!result.resolved);
    assert_eq!(repository.nonprofit_fund(GUILD), fund);
    assert_eq!(
        (repository.balance(GUILD, 2), repository.balance(GUILD, 3)),
        balances
    );
}

#[test]
fn test_bounty_not_paid_when_advance_loses_race() {
    let (mafia, repository) = service();
    let players = roster(
        GUILD,
        &[
            (1, Role::Mafia, true),
            (6, Role::Mafia, false),
            (2, Role::Townie, false),
            (3, Role::Detective, false),
            (4, Role::Doctor, false),
            (5, Role::Townie, false),
        ],
    );
    seed_accounts(&repository, &[1, 2, 3, 4, 5, 6], 50);
    day_game(&repository, players);
    assert!(mafia.submit_bounty(GUILD, 2, 1).ok);
    assert!(mafia.submit_bounty(GUILD, 3, 1).ok);
    vote_for(&mafia, &[2, 3, 5], 1);
    let fund = repository.nonprofit_fund(GUILD);
    repository.lose_next_day_claim();
    let result = mafia.resolve_day(GUILD).expect("lost claim");
    assert!(!result.resolved);
    assert_eq!(repository.nonprofit_fund(GUILD), fund);
}

#[test]
fn test_continue_day_commits_bookie_hit_and_bounty_with_phase_claim() {
    let (mafia, repository) = service();
    let players = roster(
        GUILD,
        &[
            (601, Role::Mafia, true),
            (606, Role::Mafia, false),
            (602, Role::Townie, false),
            (603, Role::Detective, false),
            (604, Role::Doctor, false),
            (605, Role::Bookie, false),
        ],
    );
    seed_accounts(&repository, &[601, 602, 603, 604, 605, 606], 10);
    let game_id = repository.seed_game(GUILD, Phase::Night, players);
    assert!(
        mafia
            .submit_night_action(GUILD, 605, 601, ActionType::Wager)
            .ok
    );
    repository.set_phase(game_id, Phase::Day);
    assert!(mafia.submit_bounty(GUILD, 602, 601).ok);
    assert!(mafia.submit_bounty(GUILD, 603, 601).ok);
    vote_for(&mafia, &[602, 603, 604], 601);
    repository.inject_post_commit_fault();
    assert_eq!(mafia.resolve_day(GUILD), Err(ServiceError::PostCommitFault));
    let actions = repository.actions(game_id);
    assert!(
        actions
            .iter()
            .any(|action| action.result.as_deref() == Some("HIT"))
    );
    assert_eq!(repository.nonprofit_fund(GUILD), 0);
    assert_eq!(
        mafia.resolve_day(GUILD).expect("retry").reason,
        Some("no_active_day")
    );
}

#[test]
fn test_final_day_commits_bookie_hit_and_bounty_with_phase_claim() {
    let (mafia, repository) = service();
    let players = roster(
        GUILD,
        &[
            (601, Role::Mafia, true),
            (602, Role::Townie, false),
            (603, Role::Townie, false),
            (604, Role::Doctor, false),
            (605, Role::Bookie, false),
        ],
    );
    seed_accounts(&repository, &[601, 602, 603, 604, 605], 10);
    let game_id = repository.seed_game(GUILD, Phase::Night, players);
    assert!(
        mafia
            .submit_night_action(GUILD, 605, 601, ActionType::Wager)
            .ok
    );
    repository.set_phase(game_id, Phase::Day);
    assert!(mafia.submit_bounty(GUILD, 602, 601).ok);
    assert!(mafia.submit_bounty(GUILD, 603, 601).ok);
    vote_for(&mafia, &[602, 603, 604], 601);
    repository.inject_post_commit_fault();
    assert_eq!(mafia.resolve_day(GUILD), Err(ServiceError::PostCommitFault));
    assert_eq!(
        repository.game(game_id).expect("game").phase,
        Phase::Resolved
    );
    assert!(
        repository
            .actions(game_id)
            .iter()
            .any(|action| action.result.as_deref() == Some("HIT"))
    );
}

#[test]
fn test_pot_for_roster_helper() {
    assert_eq!(pot_for_roster(5), 5 * ENTRY_FEE);
    assert_eq!(pot_for_roster(10), 10 * ENTRY_FEE);
    assert_eq!(pot_for_roster(15), 15 * ENTRY_FEE);
}

#[test]
fn test_zero_sum_across_outcomes() {
    for (players, target, winner) in [
        (base_roles(), 1, Winner::Town),
        (
            roster(
                GUILD,
                &[
                    (11, Role::Mafia, true),
                    (12, Role::Mafia, false),
                    (13, Role::Jester, false),
                    (14, Role::Townie, false),
                    (15, Role::Detective, false),
                ],
            ),
            13,
            Winner::Jester,
        ),
    ] {
        let (mafia, repository) = service();
        let ids: Vec<_> = players.iter().map(|player| player.discord_id).collect();
        seed_accounts(&repository, &ids, -ENTRY_FEE);
        day_game(&repository, players);
        let voters: Vec<_> = ids.iter().copied().filter(|id| *id != target).collect();
        vote_for(&mafia, &voters, target);
        let result = mafia.resolve_day(GUILD).expect("resolution");
        assert_eq!(result.winner, Some(winner));
        let total: i64 = ids.iter().map(|id| repository.balance(GUILD, *id)).sum();
        assert_eq!(total + repository.nonprofit_fund(GUILD), 0);
    }
}

#[test]
fn test_mafia_faction_win_pays_whole_pot() {
    let (mafia, repository) = service();
    let players = roster(
        GUILD,
        &[
            (601, Role::Mafia, true),
            (602, Role::Mafia, false),
            (603, Role::Townie, false),
            (604, Role::Detective, false),
            (605, Role::Townie, false),
        ],
    );
    seed_accounts(&repository, &[601, 602, 603, 604, 605], -ENTRY_FEE);
    day_game(&repository, players);
    vote_for(&mafia, &[601, 602, 603], 605);
    assert!(mafia.submit_day_vote(GUILD, 604, 601).ok);
    let result = mafia.resolve_day(GUILD).expect("resolution");
    assert_eq!(result.winner, Some(Winner::Mafia));
    assert_eq!(result.winning_ids, vec![601, 602]);
    let total: i64 = [601, 602, 603, 604, 605]
        .iter()
        .map(|id| repository.balance(GUILD, *id))
        .sum();
    assert_eq!(total, 0);
}

#[test]
fn test_detective_backfill_skipped_when_night_resolution_noops() {
    let (mafia, repository) = service();
    repository.seed_game(GUILD, Phase::Night, base_roles());
    assert!(
        mafia
            .submit_night_action(GUILD, 4, 1, ActionType::Investigate)
            .ok
    );
    repository.lose_next_night_claim();
    let result = mafia.resolve_night(GUILD);
    assert!(!result.resolved);
    assert_eq!(result.reason, Some("night_already_resolved"));
    assert_eq!(repository.detective_backfills(), 0);
}

#[test]
fn test_resolve_day_does_not_double_pay() {
    let (mafia, repository) = service();
    seed_accounts(&repository, &[1, 2, 3, 4, 5], -ENTRY_FEE);
    day_game(&repository, base_roles());
    vote_for(&mafia, &[2, 3, 4, 5], 1);
    assert!(mafia.resolve_day(GUILD).expect("first").resolved);
    let balances: Vec<_> = [1, 2, 3, 4, 5]
        .iter()
        .map(|id| repository.balance(GUILD, *id))
        .collect();
    assert!(!mafia.resolve_day(GUILD).expect("second").resolved);
    assert_eq!(
        balances,
        [1, 2, 3, 4, 5]
            .iter()
            .map(|id| repository.balance(GUILD, *id))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_try_reopen_for_finalize_refuses_resolved_game() {
    let (_, repository) = service();
    let game_id = repository.seed_game(GUILD, Phase::Night, base_roles());
    assert!(repository.try_reopen_for_finalize(game_id));
    repository.set_phase(game_id, Phase::Resolved);
    assert!(!repository.try_reopen_for_finalize(game_id));
    assert_eq!(
        repository.game(game_id).expect("game").phase,
        Phase::Resolved
    );
}

#[test]
fn test_bookie_assignment_swap_and_guard() {
    let (mafia, _) = service();
    let large = mafia.assign_roles_with_wildcards(GUILD, &(1..=8).collect::<Vec<_>>(), true, true);
    assert_eq!(large.iter().filter(|p| p.role == Role::Jester).count(), 1);
    assert_eq!(large.iter().filter(|p| p.role == Role::Bookie).count(), 1);
    let small = mafia.assign_roles_with_wildcards(GUILD, &(1..=5).collect::<Vec<_>>(), true, true);
    assert_eq!(small.iter().filter(|p| p.role == Role::Jester).count(), 1);
    assert_eq!(small.iter().filter(|p| p.role == Role::Bookie).count(), 0);
}

fn bookie_game(
    repository: &MemoryMafiaRepository,
    wager_target: PlayerId,
) -> (MafiaService, GameId) {
    seed_accounts(repository, &[601, 602, 603, 604, 605], 0);
    let mafia = MafiaService::new(repository.clone(), 42);
    let game_id = repository.seed_game(
        GUILD,
        Phase::Night,
        roster(
            GUILD,
            &[
                (601, Role::Mafia, true),
                (602, Role::Townie, false),
                (603, Role::Townie, false),
                (604, Role::Doctor, false),
                (605, Role::Bookie, false),
            ],
        ),
    );
    assert!(
        mafia
            .submit_night_action(GUILD, 605, wager_target, ActionType::Wager)
            .ok
    );
    repository.set_phase(game_id, Phase::Day);
    (mafia, game_id)
}

#[test]
fn test_bookie_hit_cashes_out() {
    let repository = MemoryMafiaRepository::default();
    let (mafia, _) = bookie_game(&repository, 601);
    vote_for(&mafia, &[602, 603, 604], 601);
    let result = mafia.resolve_day(GUILD).expect("resolution");
    assert_eq!(result.winner, Some(Winner::Town));
    assert_eq!(result.bookie_id, Some(605));
    assert_eq!(result.bookie_payout, BOOKIE_PAYOUT.min(pot_for_roster(5)));
    assert_eq!(repository.balance(GUILD, 605), result.bookie_payout);
}

#[test]
fn test_bookie_miss_pays_nothing() {
    let repository = MemoryMafiaRepository::default();
    let (mafia, _) = bookie_game(&repository, 602);
    vote_for(&mafia, &[602, 603, 604], 601);
    let result = mafia.resolve_day(GUILD).expect("resolution");
    assert_eq!(result.bookie_id, None);
    assert_eq!(result.bookie_payout, 0);
    assert_eq!(repository.balance(GUILD, 605), 0);
}

#[test]
fn test_bookie_hit_not_recorded_when_resolution_loses_race() {
    let repository = MemoryMafiaRepository::default();
    let (mafia, game_id) = bookie_game(&repository, 601);
    vote_for(&mafia, &[602, 603, 604], 601);
    repository.lose_next_day_claim();
    assert!(!mafia.resolve_day(GUILD).expect("lost claim").resolved);
    assert!(
        repository
            .actions(game_id)
            .iter()
            .all(|action| action.result.as_deref() != Some("HIT"))
    );
    assert_eq!(repository.balance(GUILD, 605), 0);
}

#[test]
fn test_bookie_hit_carries_over_multi_day_to_finalize() {
    let (mafia, repository) = service();
    let players = roster(
        GUILD,
        &[
            (601, Role::Mafia, true),
            (606, Role::Mafia, false),
            (602, Role::Townie, false),
            (603, Role::Detective, false),
            (604, Role::Doctor, false),
            (605, Role::Bookie, false),
        ],
    );
    seed_accounts(&repository, &[601, 602, 603, 604, 605, 606], 0);
    let game_id = repository.seed_game(GUILD, Phase::Night, players);
    assert!(
        mafia
            .submit_night_action(GUILD, 605, 601, ActionType::Wager)
            .ok
    );
    repository.set_phase(game_id, Phase::Day);
    vote_for(&mafia, &[602, 603, 604], 601);
    assert!(mafia.resolve_day(GUILD).expect("first day").continued);
    repository.set_phase(game_id, Phase::Day);
    vote_for(&mafia, &[602, 603, 604], 606);
    let final_day = mafia.resolve_day(GUILD).expect("final day");
    assert_eq!(final_day.winner, Some(Winner::Town));
    assert_eq!(final_day.bookie_id, Some(605));
    assert!(final_day.bookie_payout > 0);
}

#[test]
fn test_bankruptcy_penalty_sinks_mafia_profit() {
    let (mut mafia, repository) = service();
    mafia.bankruptcy_penalty_rate_basis_points = 7_500;
    seed_accounts(&repository, &[1, 2, 3, 4, 5], -ENTRY_FEE);
    for id in [2, 3, 4, 5] {
        repository.mark_bankrupt(GUILD, id);
    }
    day_game(&repository, base_roles());
    vote_for(&mafia, &[2, 3, 4, 5], 1);
    let result = mafia.resolve_day(GUILD).expect("resolution");
    assert!(!result.bankruptcy_penalties.is_empty());
    assert!(
        result
            .bankruptcy_penalties
            .keys()
            .all(|id| [2, 3, 4, 5].contains(id))
    );
    let player_total: i64 = [1, 2, 3, 4, 5]
        .iter()
        .map(|id| repository.balance(GUILD, *id))
        .sum();
    assert_eq!(player_total + repository.nonprofit_fund(GUILD), 0);
}

#[test]
fn test_finalize_day_resolution_applies_audited_vanity_tax() {
    let (mut mafia, repository) = service();
    mafia.vanity_tax_basis_points = 100;
    seed_accounts(&repository, &[511, 512, 513, 514, 515], 0);
    let game_id = day_game(
        &repository,
        roster(
            GUILD,
            &[
                (511, Role::Mafia, true),
                (512, Role::Townie, false),
                (513, Role::Doctor, false),
                (514, Role::Detective, false),
                (515, Role::Townie, false),
            ],
        ),
    );
    repository.mark_vanity_taxable(GUILD, 512);
    assert!(mafia.finalize_manual(game_id, Winner::Town, &BTreeMap::from([(512, 225)])));
    assert_eq!(repository.balance(GUILD, 512), 223);
    let ledger = repository.ledger();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].delta, -2);
    assert_eq!(ledger[0].related_id, game_id.to_string());
}

#[test]
fn test_player_role_includes_allies_for_mafia() {
    let (mafia, repository) = service();
    repository.seed_game(
        GUILD,
        Phase::Night,
        roster(
            GUILD,
            &[
                (1, Role::Mafia, true),
                (2, Role::Mafia, false),
                (3, Role::Townie, false),
                (4, Role::Doctor, false),
                (5, Role::Detective, false),
            ],
        ),
    );
    let view = mafia.player_role(GUILD, 1).expect("role view");
    assert_eq!(view.role, Role::Mafia);
    assert!(view.is_godfather);
    assert_eq!(
        view.allies
            .iter()
            .map(|ally| ally.discord_id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([2])
    );
    assert!(mafia.player_role(GUILD, 9999).is_none());
}
