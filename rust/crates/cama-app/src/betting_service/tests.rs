use std::collections::{BTreeMap, BTreeSet};

use super::*;

const GUILD: i64 = 4242;

#[derive(Default)]
struct FakeRewardPort {
    balances: BTreeMap<DiscordId, i64>,
    blessings: BTreeMap<DiscordId, i64>,
    sanctuary_bonuses: BTreeMap<DiscordId, i64>,
    sanctuary_errors: BTreeSet<DiscordId>,
    adjusted_reward: Option<i64>,
    pact_targets: BTreeSet<DiscordId>,
    skim_amounts: BTreeMap<DiscordId, i64>,
    credit_many_calls: usize,
    credit_one_calls: usize,
    blessing_lookup_calls: usize,
    pact_lookup_calls: usize,
    consume_calls: usize,
    events: Vec<String>,
}

impl BettingRewardPort for FakeRewardPort {
    fn credit_many(&mut self, _guild_id: GuildId, credits: &[CreditRequest]) -> Result<(), String> {
        self.credit_many_calls += 1;
        self.events.push(format!("credit_many:{}", credits.len()));
        for credit in credits {
            *self.balances.entry(credit.discord_id).or_default() += credit.amount;
        }
        Ok(())
    }

    fn credit_one(&mut self, _guild_id: GuildId, credit: CreditRequest) -> Result<(), String> {
        self.credit_one_calls += 1;
        self.events
            .push(format!("credit:{}:{}", credit.discord_id, credit.amount));
        *self.balances.entry(credit.discord_id).or_default() += credit.amount;
        Ok(())
    }

    fn communion_blessings(
        &mut self,
        _guild_id: GuildId,
        _player_ids: &[DiscordId],
    ) -> Result<BTreeMap<DiscordId, i64>, String> {
        self.blessing_lookup_calls += 1;
        Ok(self.blessings.clone())
    }

    fn consume_blessing_and_credit(
        &mut self,
        _guild_id: GuildId,
        discord_id: DiscordId,
        buff_id: i64,
        amount: i64,
    ) -> Result<bool, String> {
        self.consume_calls += 1;
        let consumed = self.blessings.get(&discord_id).copied() == Some(buff_id);
        if consumed {
            self.blessings.remove(&discord_id);
            *self.balances.entry(discord_id).or_default() += amount;
            self.events.push(format!("blessing:{discord_id}:{amount}"));
        }
        Ok(consumed)
    }

    fn sanctuary_bonus(
        &mut self,
        _guild_id: GuildId,
        discord_id: DiscordId,
        _base_reward: i64,
    ) -> Result<i64, String> {
        if self.sanctuary_errors.contains(&discord_id) {
            return Err("sanctuary unavailable".to_owned());
        }
        Ok(self
            .sanctuary_bonuses
            .get(&discord_id)
            .copied()
            .unwrap_or_default())
    }

    fn adjust_generated_reward(&mut self, _guild_id: GuildId, amount: i64) -> Result<i64, String> {
        Ok(self.adjusted_reward.unwrap_or(amount))
    }

    fn blood_pact_targets(
        &mut self,
        _guild_id: GuildId,
        _player_ids: &[DiscordId],
    ) -> Result<BTreeSet<DiscordId>, String> {
        self.pact_lookup_calls += 1;
        Ok(self.pact_targets.clone())
    }

    fn skim_blood_pact(
        &mut self,
        _guild_id: GuildId,
        discord_id: DiscordId,
        earning: i64,
        _match_id: Option<i64>,
    ) -> Result<i64, String> {
        self.events.push(format!("skim:{discord_id}:{earning}"));
        Ok(self
            .skim_amounts
            .get(&discord_id)
            .copied()
            .unwrap_or_default())
    }
}

#[test]
fn test_award_exclusion_bonus_adds_reward() {
    let mut service = BettingRewardService::new(FakeRewardPort::default());
    let result = service
        .award_exclusion_bonus(GUILD, &[7070])
        .expect("exclusion reward");
    assert_eq!(result[&7070].gross, DEFAULT_EXCLUSION_REWARD);
    assert_eq!(result[&7070].net, DEFAULT_EXCLUSION_REWARD);
    assert_eq!(result[&7070].garnished, 0);
    assert_eq!(
        service.port().balances.get(&7070),
        Some(&DEFAULT_EXCLUSION_REWARD)
    );
}

#[test]
fn test_award_exclusion_bonus_empty_list_noop() {
    let mut service = BettingRewardService::new(FakeRewardPort::default());
    assert!(
        service
            .award_exclusion_bonus(GUILD, &[])
            .expect("empty reward")
            .is_empty()
    );
    assert_eq!(service.port().credit_many_calls, 0);
    assert_eq!(service.port().pact_lookup_calls, 0);
}

#[test]
fn test_retired_conditional_exclusion_path_keeps_one_jc_fallback() {
    let mut service = BettingRewardService::new(FakeRewardPort::default());
    let result = service
        .award_exclusion_bonus_half(GUILD, &[7071])
        .expect("legacy exclusion reward");
    assert_eq!(result[&7071].gross, 1);
    assert_eq!(service.port().balances.get(&7071), Some(&1));
}

#[test]
fn test_shuffle_result_vs_pending_state_keys() {
    let players = (1..=10).collect::<Vec<_>>();
    let (result, pending) = typed_shuffle(&players, 123_456);
    assert_eq!(result.radiant_team.player_ids, [1, 2, 3, 4, 5]);
    assert_eq!(result.dire_team.player_ids, [6, 7, 8, 9, 10]);
    assert_eq!(pending.radiant_team_ids, result.radiant_team.player_ids);
    assert_eq!(pending.dire_team_ids, result.dire_team.player_ids);
    assert_eq!(pending.shuffle_timestamp, 123_456);
}

#[test]
fn test_disabled_top_tier_uses_base_rate_for_all_slots() {
    let candidates = (14_900..14_910)
        .map(|discord_id| WealthSnapshot {
            discord_id,
            balance: 100,
        })
        .collect::<Vec<_>>();
    let plan = plan_automatic_spectator_bets(
        &candidates,
        &BTreeSet::new(),
        AutomaticSpectatorConfig {
            top_basis_points: 0,
            ..AutomaticSpectatorConfig::default()
        },
    );
    assert_eq!(plan.bets.len(), 10);
    assert_eq!(plan.top_count, 0);
    assert!(plan.bets.iter().all(|bet| bet.basis_points == 100));
}

#[test]
fn test_spectator_basis_point_rounding_matches_python_ties_to_even() {
    let candidates = [
        WealthSnapshot {
            discord_id: 15_200,
            balance: 250,
        }, // 2.5 -> 2
        WealthSnapshot {
            discord_id: 15_201,
            balance: 150,
        }, // 1.5 -> 2
        WealthSnapshot {
            discord_id: 15_202,
            balance: 50,
        }, // .5 -> 0, skipped
    ];
    let plan = plan_automatic_spectator_bets(
        &candidates,
        &BTreeSet::new(),
        AutomaticSpectatorConfig {
            enabled: true,
            total_count: 3,
            top_count: 0,
            base_basis_points: 100,
            top_basis_points: 0,
        },
    );
    assert_eq!(
        plan.bets
            .iter()
            .map(|bet| (bet.discord_id, bet.amount))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([(15_200, 2), (15_201, 2)])
    );
}

#[test]
fn test_create_auto_spectator_bets_uses_top_richest_spectators() {
    let participants = (15_000..15_010).collect::<BTreeSet<_>>();
    let mut candidates = participants
        .iter()
        .map(|discord_id| WealthSnapshot {
            discord_id: *discord_id,
            balance: 1_000,
        })
        .collect::<Vec<_>>();
    candidates.extend(
        [1_000, 900, 800, 700, 600, 500, 400, 300, 200, 100, 90, 80]
            .into_iter()
            .enumerate()
            .map(|(index, balance)| WealthSnapshot {
                discord_id: 15_100 + i64::try_from(index).expect("small index"),
                balance,
            }),
    );
    let plan = plan_automatic_spectator_bets(
        &candidates,
        &participants,
        AutomaticSpectatorConfig::default(),
    );
    assert_eq!(plan.bets.len(), 10);
    assert_eq!(plan.total_count, 10);
    assert_eq!(plan.top_count, 5);
    assert_eq!(plan.top_basis_points, 200);
    assert_eq!(plan.base_basis_points, 100);
    assert_eq!(
        plan.bets
            .iter()
            .map(|bet| (bet.discord_id, bet.amount))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([
            (15_100, 20),
            (15_101, 18),
            (15_102, 16),
            (15_103, 14),
            (15_104, 12),
            (15_105, 5),
            (15_106, 4),
            (15_107, 3),
            (15_108, 2),
            (15_109, 1),
        ])
    );
    assert!(plan.total_radiant.saturating_sub(plan.total_dire).abs() <= 2);
    assert!(
        plan.bets
            .iter()
            .all(|bet| !participants.contains(&bet.discord_id))
    );
}

fn pending_state() -> PendingMatchState {
    PendingMatchState {
        radiant_team_ids: vec![1, 2, 3, 4, 5],
        dire_team_ids: vec![6, 7, 8, 9, 10],
        shuffle_timestamp: 100,
        bet_lock_until: 1_000,
        is_draft: false,
        is_bomb_pot: false,
        is_openskill_shuffle: false,
        balancing_rating_system: RatingSystem::Glicko,
    }
}

#[test]
fn test_bomb_pot_flag_persisted_in_pending_match() {
    let mut state = pending_state();
    state.is_bomb_pot = true;
    assert!(state.to_payload_json().contains("\"is_bomb_pot\":true"));
    state.is_bomb_pot = false;
    assert!(state.to_payload_json().contains("\"is_bomb_pot\":false"));
}

#[test]
fn test_openskill_shuffle_flag_persisted_in_pending_match() {
    let mut state = pending_state();
    state.is_openskill_shuffle = true;
    assert!(
        state
            .to_payload_json()
            .contains("\"is_openskill_shuffle\":true")
    );
    state.is_openskill_shuffle = false;
    assert!(
        state
            .to_payload_json()
            .contains("\"is_openskill_shuffle\":false")
    );
}

#[test]
fn test_balancing_rating_system_persisted_in_pending_match() {
    let mut state = pending_state();
    for (rating_system, expected) in [
        (RatingSystem::OpenSkill, "openskill"),
        (RatingSystem::Glicko, "glicko"),
        (RatingSystem::Jopacoin, "jopacoin"),
    ] {
        state.balancing_rating_system = rating_system;
        assert!(
            state
                .to_payload_json()
                .contains(&format!("\"balancing_rating_system\":\"{expected}\""))
        );
    }
}

#[test]
fn test_blessing_bonus_survives_sanctuary_exception() {
    let mut port = FakeRewardPort::default();
    port.blessings.insert(7_171, 1);
    port.sanctuary_errors.insert(7_171);
    port.adjusted_reward = Some(2);
    let mut service = BettingRewardService::new(port);
    let results = service
        .award_win_bonus(GUILD, &[7_171], DEFAULT_WIN_REWARD, &BTreeMap::new())
        .expect("win award");
    assert_eq!(results[&7_171].manashop_bonus, 2);
    assert_eq!(results[&7_171].net, DEFAULT_WIN_REWARD + 2);
    assert_eq!(service.port().balances.get(&7_171), Some(&12));
    assert_eq!(service.port().consume_calls, 1);
}

#[test]
fn test_win_bonus_bulk_loads_communion_blessings() {
    let winners = (7_180..7_185).collect::<Vec<_>>();
    let mut service = BettingRewardService::new(FakeRewardPort::default());
    service
        .award_win_bonus(GUILD, &winners, DEFAULT_WIN_REWARD, &BTreeMap::new())
        .expect("winner batch");
    assert_eq!(service.port().blessing_lookup_calls, 1);
    assert_eq!(service.port().pact_lookup_calls, 1);
    assert_eq!(service.port().consume_calls, 0);
}

#[test]
fn test_participation_snapshots_blood_pact_targets_once() {
    let players = (7_190..7_195).collect::<Vec<_>>();
    let mut service = BettingRewardService::new(FakeRewardPort::default());
    let results = service
        .award_participation(GUILD, &players)
        .expect("participation batch");
    assert_eq!(results.len(), 5);
    assert_eq!(service.port().pact_lookup_calls, 1);
    assert_eq!(service.port().credit_many_calls, 1);
    assert_eq!(service.port().credit_one_calls, 0);
    assert!(
        !service
            .port()
            .events
            .iter()
            .any(|event| event.starts_with("skim:"))
    );
}

#[test]
fn test_participation_with_active_pact_keeps_point_skim_order() {
    let mut port = FakeRewardPort::default();
    port.pact_targets.insert(7_196);
    port.skim_amounts.insert(7_196, 1);
    let mut service = BettingRewardService::new(port);
    let results = service
        .award_participation(GUILD, &[7_196, 7_197])
        .expect("ordered participation");
    assert_eq!(results[&7_196].blood_pact_skimmed, 1);
    assert_eq!(results[&7_196].net, DEFAULT_PARTICIPATION_REWARD - 1);
    assert_eq!(results[&7_197].blood_pact_skimmed, 0);
    assert_eq!(
        service.port().events,
        [
            format!("credit:7196:{DEFAULT_PARTICIPATION_REWARD}"),
            format!("skim:7196:{DEFAULT_PARTICIPATION_REWARD}"),
            format!("credit:7197:{DEFAULT_PARTICIPATION_REWARD}"),
        ]
    );
}

#[test]
fn test_settle_skims_blood_pact_on_net_profit_for_penalized_winner() {
    let mut port = FakeRewardPort::default();
    port.pact_targets.insert(9_550);
    port.skim_amounts.insert(9_550, 5);
    let mut service = BettingRewardService::new(port);
    let mut settlement = SettlementDistribution {
        winners: vec![SettlementBet {
            discord_id: 9_550,
            effective_stake: 20,
            payout: 40,
        }],
        ..SettlementDistribution::default()
    };
    let skims = service
        .apply_settlement_hooks(GUILD, 910, &mut settlement, &BTreeSet::from([9_550]), 5_000)
        .expect("settlement hooks");
    assert_eq!(
        settlement.bankruptcy_penalties,
        BTreeMap::from([(9_550, 10)])
    );
    assert_eq!(skims, BTreeMap::from([(9_550, 5)]));
    assert!(service.port().events.contains(&"skim:9550:10".to_owned()));
}

#[test]
fn test_two_sided_bettor_penalties_use_net_match_profit() {
    let mut port = FakeRewardPort::default();
    port.pact_targets.insert(16_800);
    let mut service = BettingRewardService::new(port);
    let mut settlement = SettlementDistribution {
        winners: vec![SettlementBet {
            discord_id: 16_800,
            effective_stake: 10,
            payout: 20,
        }],
        losers: vec![SettlementBet {
            discord_id: 16_800,
            effective_stake: 10,
            payout: 0,
        }],
        ..SettlementDistribution::default()
    };
    let skims = service
        .apply_settlement_hooks(
            GUILD,
            911,
            &mut settlement,
            &BTreeSet::from([16_800]),
            5_000,
        )
        .expect("hedged settlement");
    assert!(settlement.bankruptcy_penalties.is_empty());
    assert!(skims.is_empty());
    assert!(
        !service
            .port()
            .events
            .iter()
            .any(|event| event.starts_with("skim:"))
    );
}

#[test]
fn test_award_win_bonus_skims_blood_pact_on_net() {
    let mut port = FakeRewardPort::default();
    port.pact_targets.insert(9_650);
    port.skim_amounts.insert(9_650, 1);
    let mut service = BettingRewardService::new(port);
    let policies = BTreeMap::from([(
        9_650,
        AwardPolicy {
            bankruptcy_keep_basis_points: 5_000,
            vanity_tax_basis_points: 0,
        },
    )]);
    let results = service
        .award_win_bonus(GUILD, &[9_650], DEFAULT_WIN_REWARD, &policies)
        .expect("penalized win award");
    assert_eq!(results[&9_650].gross, DEFAULT_WIN_REWARD);
    assert_eq!(results[&9_650].bankruptcy_penalty, 5);
    assert_eq!(results[&9_650].blood_pact_skimmed, 1);
    assert_eq!(results[&9_650].net, 4);
    assert_eq!(service.port().balances.get(&9_650), Some(&5));
    assert!(service.port().events.contains(&"skim:9650:5".to_owned()));
}

#[test]
fn test_award_win_bonus_applies_vanity_tax_before_blood_pact() {
    let mut port = FakeRewardPort::default();
    port.pact_targets.insert(9_651);
    port.skim_amounts.insert(9_651, 1);
    let mut service = BettingRewardService::new(port);
    let policies = BTreeMap::from([(
        9_651,
        AwardPolicy {
            bankruptcy_keep_basis_points: 5_000,
            vanity_tax_basis_points: DEFAULT_VANITY_TAX_BASIS_POINTS,
        },
    )]);
    let results = service
        .award_win_bonus(GUILD, &[9_651], 200, &policies)
        .expect("taxed win award");
    assert_eq!(results[&9_651].gross, 200);
    assert_eq!(results[&9_651].bankruptcy_penalty, 100);
    assert_eq!(results[&9_651].vanity_tax, 20);
    assert_eq!(results[&9_651].blood_pact_skimmed, 1);
    assert_eq!(results[&9_651].net, 79);
    assert_eq!(service.port().balances.get(&9_651), Some(&80));
    assert!(service.port().events.contains(&"skim:9651:80".to_owned()));
}

#[test]
fn test_bankruptcy_debuff_bet_penalty_only_docks_profit_and_protects_stake() {
    let mut service = BettingRewardService::new(FakeRewardPort::default());
    let bettor = 1_001;
    let stake = 5;
    let mut settlement = SettlementDistribution {
        winners: vec![SettlementBet {
            discord_id: bettor,
            effective_stake: stake,
            payout: 10,
        }],
        ..SettlementDistribution::default()
    };
    service
        .apply_settlement_hooks(
            GUILD,
            123,
            &mut settlement,
            &BTreeSet::from([bettor]),
            7_500,
        )
        .expect("penalized house settlement");

    assert_eq!(settlement.net_match_profits()[&bettor], 5);
    assert_eq!(settlement.bankruptcy_penalties[&bettor], 1);
    let starting_balance = 23;
    let post_bet_balance = starting_balance - stake;
    let balance_after =
        post_bet_balance + settlement.winners[0].payout - settlement.bankruptcy_penalties[&bettor];
    assert_eq!(balance_after, 27);
    assert!(balance_after > starting_balance);
}

#[test]
fn test_bankruptcy_debuff_plain_bet_winner_keeps_full_payout() {
    let mut service = BettingRewardService::new(FakeRewardPort::default());
    let bettor = 1_002;
    let mut settlement = SettlementDistribution {
        winners: vec![SettlementBet {
            discord_id: bettor,
            effective_stake: 5,
            payout: 10,
        }],
        ..SettlementDistribution::default()
    };
    service
        .apply_settlement_hooks(GUILD, 124, &mut settlement, &BTreeSet::new(), 7_500)
        .expect("ordinary house settlement");

    assert!(settlement.bankruptcy_penalties.is_empty());
    assert_eq!(23 - 5 + settlement.winners[0].payout, 28);
}

#[test]
fn test_bankruptcy_debuff_vanity_tax_only_docks_bet_profit() {
    let profit = calculate_award(
        100,
        AwardPolicy {
            bankruptcy_keep_basis_points: 10_000,
            vanity_tax_basis_points: 1_000,
        },
    );
    assert_eq!(profit.bankruptcy_penalty, 0);
    assert_eq!(profit.vanity_tax, 10);
    assert_eq!(profit.net, 90);

    let starting_balance = 503;
    let stake = 100;
    let balance_after = starting_balance - stake + stake + profit.net;
    assert_eq!(balance_after, 593);
}
