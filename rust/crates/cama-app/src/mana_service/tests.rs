use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::TimeZone;

use super::*;

const GUILD: i64 = 9_001;
const TODAY: &str = "2025-06-15";

#[derive(Default)]
struct MockStore {
    rows: BTreeMap<(DiscordId, GuildId), StoredMana>,
    accepted_batch_ids: Option<BTreeSet<DiscordId>>,
    get_calls: usize,
    all_calls: usize,
    batch_calls: usize,
    last_batch: Vec<(DiscordId, Land)>,
}

impl MockStore {
    fn seed(&mut self, discord_id: DiscordId, guild_id: GuildId, land: Land, date: &str) {
        self.rows.insert(
            (discord_id, guild_id),
            StoredMana {
                land,
                assigned_date: date.to_owned(),
                consumed_today: false,
                white_shield_remaining: if land == Land::Plains { 25 } else { 0 },
            },
        );
    }
}

impl ManaAssignmentStore for MockStore {
    fn get_mana(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<Option<StoredMana>, ManaServiceError> {
        self.get_calls += 1;
        Ok(self.rows.get(&(discord_id, guild_id)).cloned())
    }

    fn get_all_mana(&mut self, guild_id: GuildId) -> Result<Vec<GuildMana>, ManaServiceError> {
        self.all_calls += 1;
        Ok(self
            .rows
            .iter()
            .filter(|((_, row_guild_id), _)| *row_guild_id == guild_id)
            .map(|((discord_id, _), mana)| GuildMana {
                discord_id: *discord_id,
                mana: mana.clone(),
            })
            .collect())
    }

    fn claim_mana_atomic(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        land: Land,
        assigned_date: &str,
    ) -> Result<bool, ManaServiceError> {
        if self
            .rows
            .get(&(discord_id, guild_id))
            .is_some_and(|row| row.assigned_date == assigned_date)
        {
            return Ok(false);
        }
        self.seed(discord_id, guild_id, land, assigned_date);
        Ok(true)
    }

    fn claim_mana_batch_atomic(
        &mut self,
        assignments: &[(DiscordId, Land)],
        guild_id: GuildId,
        assigned_date: &str,
    ) -> Result<Vec<ClaimedMana>, ManaServiceError> {
        self.batch_calls += 1;
        self.last_batch = assignments.to_vec();
        let mut seen = BTreeSet::new();
        let mut claimed = Vec::new();
        for (discord_id, land) in assignments {
            if !seen.insert(*discord_id)
                || self
                    .accepted_batch_ids
                    .as_ref()
                    .is_some_and(|ids| !ids.contains(discord_id))
                || self
                    .rows
                    .get(&(*discord_id, guild_id))
                    .is_some_and(|row| row.assigned_date == assigned_date)
            {
                continue;
            }
            self.seed(*discord_id, guild_id, *land, assigned_date);
            claimed.push(ClaimedMana {
                discord_id: *discord_id,
                mana: self
                    .rows
                    .get(&(*discord_id, guild_id))
                    .expect("newly seeded row")
                    .clone(),
            });
        }
        Ok(claimed)
    }
}

#[derive(Default)]
struct DataCalls {
    player: usize,
    players: usize,
    lowest: usize,
    history: usize,
    degen: usize,
    bankruptcy: usize,
    tips: usize,
    lowest_bulk: usize,
    degen_bulk: usize,
    bankruptcy_bulk: usize,
    tips_bulk: usize,
    streaks_bulk: usize,
}

struct MockData {
    default_player: PlayerProfile,
    players: Vec<PlayerProfile>,
    history: Vec<BetOutcome>,
    lowest: Option<i64>,
    degen: DegenScore,
    bankruptcy: BankruptcyState,
    tips: TipStats,
    lowest_bulk: BTreeMap<DiscordId, Option<i64>>,
    degen_bulk: BTreeMap<DiscordId, DegenScore>,
    bankruptcy_bulk: BTreeMap<DiscordId, BankruptcyState>,
    tips_bulk: BTreeMap<DiscordId, TipStats>,
    streaks_bulk: BTreeMap<DiscordId, i64>,
    degen_history_seen: Vec<BetOutcome>,
    calls: DataCalls,
}

impl Default for MockData {
    fn default() -> Self {
        Self {
            default_player: player(None, 10),
            players: Vec::new(),
            history: Vec::new(),
            lowest: None,
            degen: DegenScore::default(),
            bankruptcy: BankruptcyState::default(),
            tips: TipStats::default(),
            lowest_bulk: BTreeMap::new(),
            degen_bulk: BTreeMap::new(),
            bankruptcy_bulk: BTreeMap::new(),
            tips_bulk: BTreeMap::new(),
            streaks_bulk: BTreeMap::new(),
            degen_history_seen: Vec::new(),
            calls: DataCalls::default(),
        }
    }
}

impl ManaDataSource for MockData {
    fn player(
        &mut self,
        discord_id: DiscordId,
        _guild_id: GuildId,
    ) -> Result<Option<PlayerProfile>, ManaServiceError> {
        self.calls.player += 1;
        let mut profile = self.default_player.clone();
        profile.discord_id = Some(discord_id);
        Ok(Some(profile))
    }

    fn players(&mut self, _guild_id: GuildId) -> Result<Vec<PlayerProfile>, ManaServiceError> {
        self.calls.players += 1;
        Ok(self.players.clone())
    }

    fn lowest_balance(
        &mut self,
        _discord_id: DiscordId,
        _guild_id: GuildId,
    ) -> Result<Option<i64>, ManaServiceError> {
        self.calls.lowest += 1;
        Ok(self.lowest)
    }

    fn bet_history(
        &mut self,
        _discord_id: DiscordId,
        _guild_id: GuildId,
    ) -> Result<Vec<BetOutcome>, ManaServiceError> {
        self.calls.history += 1;
        Ok(self.history.clone())
    }

    fn degen_score(
        &mut self,
        _discord_id: DiscordId,
        _guild_id: GuildId,
        history: &[BetOutcome],
    ) -> Result<DegenScore, ManaServiceError> {
        self.calls.degen += 1;
        self.degen_history_seen = history.to_vec();
        Ok(self.degen.clone())
    }

    fn bankruptcy_state(
        &mut self,
        _discord_id: DiscordId,
        _guild_id: GuildId,
    ) -> Result<BankruptcyState, ManaServiceError> {
        self.calls.bankruptcy += 1;
        Ok(self.bankruptcy.clone())
    }

    fn tip_stats(
        &mut self,
        _discord_id: DiscordId,
        _guild_id: GuildId,
    ) -> Result<TipStats, ManaServiceError> {
        self.calls.tips += 1;
        Ok(self.tips.clone())
    }

    fn lowest_balances_bulk(
        &mut self,
        _discord_ids: &[DiscordId],
        _guild_id: GuildId,
    ) -> Result<BTreeMap<DiscordId, Option<i64>>, ManaServiceError> {
        self.calls.lowest_bulk += 1;
        Ok(self.lowest_bulk.clone())
    }

    fn degen_scores_bulk(
        &mut self,
        _discord_ids: &[DiscordId],
        _guild_id: GuildId,
    ) -> Result<BTreeMap<DiscordId, DegenScore>, ManaServiceError> {
        self.calls.degen_bulk += 1;
        Ok(self.degen_bulk.clone())
    }

    fn bankruptcy_states_bulk(
        &mut self,
        _discord_ids: &[DiscordId],
        _guild_id: GuildId,
    ) -> Result<BTreeMap<DiscordId, BankruptcyState>, ManaServiceError> {
        self.calls.bankruptcy_bulk += 1;
        Ok(self.bankruptcy_bulk.clone())
    }

    fn tip_stats_bulk(
        &mut self,
        _discord_ids: &[DiscordId],
        _guild_id: GuildId,
    ) -> Result<BTreeMap<DiscordId, TipStats>, ManaServiceError> {
        self.calls.tips_bulk += 1;
        Ok(self.tips_bulk.clone())
    }

    fn current_streaks_bulk(
        &mut self,
        _discord_ids: &[DiscordId],
        _guild_id: GuildId,
    ) -> Result<BTreeMap<DiscordId, i64>, ManaServiceError> {
        self.calls.streaks_bulk += 1;
        Ok(self.streaks_bulk.clone())
    }
}

#[derive(Default)]
struct ScriptedEntropy {
    lands: VecDeque<Land>,
}

impl ScriptedEntropy {
    fn new(lands: impl IntoIterator<Item = Land>) -> Self {
        Self {
            lands: lands.into_iter().collect(),
        }
    }
}

impl LandEntropy for ScriptedEntropy {
    fn choose(&mut self, _weights: &LandWeights) -> Land {
        self.lands.pop_front().unwrap_or(Land::Forest)
    }
}

#[derive(Default)]
struct MockProtection {
    refund: i64,
    batch_refunds: BTreeMap<DiscordId, i64>,
    single_calls: Vec<(DiscordId, GuildId, i64)>,
    batch_calls: Vec<(Vec<DiscordId>, GuildId, i64)>,
}

impl GuardianProtection for MockProtection {
    fn reconcile_guardian(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        since: i64,
    ) -> Result<i64, ManaServiceError> {
        self.single_calls.push((discord_id, guild_id, since));
        Ok(self.refund)
    }

    fn reconcile_guardians(
        &mut self,
        discord_ids: &[DiscordId],
        guild_id: GuildId,
        since: i64,
    ) -> Result<BTreeMap<DiscordId, i64>, ManaServiceError> {
        self.batch_calls
            .push((discord_ids.to_vec(), guild_id, since));
        Ok(self.batch_refunds.clone())
    }
}

fn player(discord_id: Option<DiscordId>, balance: i64) -> PlayerProfile {
    PlayerProfile {
        discord_id,
        balance,
        wins: 5,
        losses: 5,
        glicko_rating: Some(2_000.0),
    }
}

fn service(
    store: MockStore,
    data: MockData,
    entropy: ScriptedEntropy,
    protection: MockProtection,
) -> ManaService<MockStore, MockData, ScriptedEntropy, MockProtection> {
    ManaService::new(store, data, entropy, protection)
}

fn la_time(hour: u32, minute: u32) -> DateTime<FixedOffset> {
    FixedOffset::west_opt(7 * 60 * 60)
        .expect("valid Pacific offset")
        .with_ymd_and_hms(2025, 6, 15, hour, minute, 0)
        .single()
        .expect("valid test timestamp")
}

#[test]
fn test_after_reset_hour_returns_today() {
    assert_eq!(mana_day_label(la_time(10, 0)), TODAY);
}

#[test]
fn test_before_reset_hour_returns_yesterday() {
    assert_eq!(mana_day_label(la_time(2, 0)), "2025-06-14");
}

#[test]
fn test_exactly_at_reset_hour_returns_today() {
    assert_eq!(mana_day_label(la_time(4, 0)), TODAY);
}

#[test]
fn test_mana_day_start_timestamp_observes_four_am_boundary() {
    assert_eq!(
        mana_day_start_timestamp(la_time(2, 30)),
        la_time(4, 0).timestamp() - 86_400
    );
    assert_eq!(
        mana_day_start_timestamp(la_time(10, 30)),
        la_time(4, 0).timestamp()
    );
}

#[test]
fn test_plains_embed_shows_guardian_capacity_and_retro_refund() {
    let embed = build_single_embed(
        "Guardian",
        &ManaDetails {
            land: Land::Plains,
            color: "White",
            emoji: "🌾",
            assigned_date: TODAY.to_owned(),
            retro_refund: 9,
            guardian_remaining: 16,
            consumed: false,
        },
        TODAY,
    );
    let guardian = embed
        .fields
        .iter()
        .find(|field| field.name == "🌾 Guardian Aura")
        .expect("Guardian field");
    assert!(guardian.value.contains("16/25 JC"));
    assert!(guardian.value.contains("Recovered **9 JC**"));
}

#[test]
fn test_has_assigned_today_false_when_no_row() {
    let mut service = service(
        MockStore::default(),
        MockData::default(),
        ScriptedEntropy::default(),
        MockProtection::default(),
    );
    assert!(
        !service
            .has_assigned_today(9_999, GUILD, TODAY)
            .expect("lookup")
    );
}

#[test]
fn test_has_assigned_today_false_when_different_date() {
    let mut store = MockStore::default();
    store.seed(9_999, GUILD, Land::Forest, "2025-06-14");
    let mut service = service(
        store,
        MockData::default(),
        ScriptedEntropy::default(),
        MockProtection::default(),
    );
    assert!(
        !service
            .has_assigned_today(9_999, GUILD, TODAY)
            .expect("lookup")
    );
}

#[test]
fn test_has_assigned_today_true() {
    let mut store = MockStore::default();
    store.seed(9_999, GUILD, Land::Island, TODAY);
    let mut service = service(
        store,
        MockData::default(),
        ScriptedEntropy::default(),
        MockProtection::default(),
    );
    assert!(
        service
            .has_assigned_today(9_999, GUILD, TODAY)
            .expect("lookup")
    );
}

#[test]
fn test_get_current_mana_none_when_never_assigned() {
    let mut service = service(
        MockStore::default(),
        MockData::default(),
        ScriptedEntropy::default(),
        MockProtection::default(),
    );
    assert_eq!(
        service
            .get_current_mana(9_999, GUILD, TODAY)
            .expect("lookup"),
        None
    );
}

#[test]
fn test_get_current_mana_returns_dict() {
    let mut store = MockStore::default();
    store.seed(9_999, GUILD, Land::Plains, TODAY);
    let mut service = service(
        store,
        MockData::default(),
        ScriptedEntropy::default(),
        MockProtection::default(),
    );
    let details = service
        .get_current_mana(9_999, GUILD, TODAY)
        .expect("lookup")
        .expect("mana details");
    assert_eq!(details.land, Land::Plains);
    assert_eq!(details.color, "White");
    assert_eq!(details.emoji, "🌾");
    assert_eq!(details.assigned_date, TODAY);
}

#[test]
fn test_assigns_mana_and_returns_dict() {
    let mut service = service(
        MockStore::default(),
        MockData::default(),
        ScriptedEntropy::new([Land::Island]),
        MockProtection::default(),
    );
    let details = service
        .assign_daily_mana(42, GUILD, TODAY, 1_750_000_000, false)
        .expect("assign mana");
    assert_eq!(
        (details.land, details.color, details.emoji),
        (Land::Island, "Blue", "🏝️")
    );
    let stored = service
        .repository()
        .rows
        .get(&(42, GUILD))
        .expect("stored assignment");
    assert_eq!(
        (stored.land, stored.assigned_date.as_str()),
        (Land::Island, TODAY)
    );
}

#[test]
fn test_raises_if_already_assigned() {
    let mut store = MockStore::default();
    store.seed(42, GUILD, Land::Forest, TODAY);
    let mut service = service(
        store,
        MockData::default(),
        ScriptedEntropy::new([Land::Mountain]),
        MockProtection::default(),
    );
    assert_eq!(
        service.assign_daily_mana(42, GUILD, TODAY, 1_750_000_000, false),
        Err(ManaServiceError::AlreadyAssigned)
    );
    assert_eq!(service.repository().rows[&(42, GUILD)].land, Land::Forest);
}

#[test]
fn test_can_reassign_next_day() {
    let mut store = MockStore::default();
    store.seed(42, GUILD, Land::Mountain, "2025-06-14");
    let mut service = service(
        store,
        MockData::default(),
        ScriptedEntropy::new([Land::Swamp]),
        MockProtection::default(),
    );
    let details = service
        .assign_daily_mana(42, GUILD, TODAY, 1_750_000_000, false)
        .expect("next-day assignment");
    assert_eq!(details.land, Land::Swamp);
    assert_eq!(service.repository().rows[&(42, GUILD)].assigned_date, TODAY);
}

#[test]
fn test_result_is_always_valid_land() {
    let mut service = ManaService::new(
        MockStore::default(),
        MockData::default(),
        XorShift64Entropy::new(0xC0FFEE),
        MockProtection::default(),
    );
    for discord_id in 1_000..1_020 {
        let details = service
            .assign_daily_mana(discord_id, GUILD, TODAY, 1_750_000_000, false)
            .expect("valid assignment");
        assert!(Land::ORDERED.contains(&details.land));
    }
}

#[test]
fn test_plains_assignment_reconciles_guardian_since_daily_reset() {
    let mut service = service(
        MockStore::default(),
        MockData::default(),
        ScriptedEntropy::new([Land::Plains]),
        MockProtection {
            refund: 9,
            ..MockProtection::default()
        },
    );
    let details = service
        .assign_daily_mana(42, GUILD, TODAY, 1_750_000_000, false)
        .expect("Plains assignment");
    assert_eq!(
        service.protection().single_calls,
        [(42, GUILD, 1_750_000_000)]
    );
    assert_eq!(details.retro_refund, 9);
    assert_eq!(details.guardian_remaining, 25);
}

#[test]
fn test_single_assignment_reuses_one_bet_history() {
    let data = MockData {
        history: vec![BetOutcome::Lost, BetOutcome::Won],
        ..MockData::default()
    };
    let mut service = service(
        MockStore::default(),
        data,
        ScriptedEntropy::new([Land::Forest]),
        MockProtection::default(),
    );
    service
        .assign_daily_mana(43, GUILD, TODAY, 1_750_000_000, false)
        .expect("assignment");
    assert_eq!(service.data().calls.history, 1);
    assert_eq!(service.data().calls.degen, 1);
    assert_eq!(
        service.data().degen_history_seen,
        [BetOutcome::Lost, BetOutcome::Won]
    );
    assert_eq!(current_streak(&service.data().degen_history_seen), 1);
}

#[test]
fn test_batches_claims_and_reuses_loaded_players_and_bulk_stats() {
    let mut store = MockStore {
        accepted_batch_ids: Some(BTreeSet::from([2])),
        ..MockStore::default()
    };
    store.seed(1, GUILD, Land::Swamp, TODAY);
    let data = MockData {
        players: vec![
            player(Some(1), 10),
            player(Some(2), -10),
            player(Some(3), 20),
        ],
        lowest_bulk: BTreeMap::from([(2, Some(-10)), (3, Some(0))]),
        degen_bulk: BTreeMap::from([(2, DegenScore::default()), (3, DegenScore::default())]),
        bankruptcy_bulk: BTreeMap::from([
            (2, BankruptcyState::default()),
            (3, BankruptcyState::default()),
        ]),
        tips_bulk: BTreeMap::from([(2, TipStats::default()), (3, TipStats::default())]),
        streaks_bulk: BTreeMap::from([(2, -1), (3, 1)]),
        ..MockData::default()
    };
    let protection = MockProtection {
        batch_refunds: BTreeMap::from([(2, 5)]),
        ..MockProtection::default()
    };
    let mut service = service(
        store,
        data,
        ScriptedEntropy::new([Land::Plains, Land::Forest]),
        protection,
    );
    let result = service
        .assign_all_daily_mana_with_board(GUILD, TODAY, 1_750_000_000, &BTreeSet::from([3]))
        .expect("batch assignment");

    assert_eq!(service.repository().all_calls, 1);
    assert_eq!(service.repository().get_calls, 0);
    assert_eq!(service.repository().batch_calls, 1);
    assert_eq!(
        service.repository().last_batch,
        [(2, Land::Plains), (3, Land::Forest)]
    );
    assert_eq!(service.data().calls.players, 1);
    assert_eq!(service.data().calls.player, 0);
    assert_eq!(service.data().calls.lowest_bulk, 1);
    assert_eq!(service.data().calls.degen_bulk, 1);
    assert_eq!(service.data().calls.bankruptcy_bulk, 1);
    assert_eq!(service.data().calls.tips_bulk, 1);
    assert_eq!(service.data().calls.streaks_bulk, 1);
    assert_eq!(service.data().calls.history, 0);
    assert_eq!(service.data().calls.degen, 0);
    assert_eq!(
        service.protection().batch_calls,
        [(vec![2], GUILD, 1_750_000_000)]
    );
    assert_eq!(result.assignments.len(), 1);
    assert_eq!(result.assignments[0].discord_id, 2);
    assert_eq!(result.assignments[0].details.land, Land::Plains);
    assert_eq!(result.assignments[0].details.retro_refund, 5);
    assert_eq!(result.assignments[0].details.guardian_remaining, 20);
    assert_eq!(
        result
            .board
            .iter()
            .map(|row| row.discord_id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([1, 2])
    );
}

fn baseline_inputs() -> WeightInputs {
    WeightInputs {
        player: player(Some(1), 10),
        ..WeightInputs::default()
    }
}

#[test]
fn test_high_balance_boosts_island() {
    let mut inputs = baseline_inputs();
    inputs.player.balance = 600;
    let weights = calculate_land_weights(&inputs);
    assert!(weights.get(Land::Island) > weights.get(Land::Forest));
}

#[test]
fn test_high_degen_boosts_mountain() {
    let mut inputs = baseline_inputs();
    inputs.degen = DegenScore {
        total: 90,
        max_leverage_score: 22,
        loss_chase_score: 5,
        ..DegenScore::default()
    };
    let weights = calculate_land_weights(&inputs);
    assert_eq!(weights.get(Land::Mountain), weights.max());
}

#[test]
fn test_bankruptcy_boosts_swamp() {
    let mut inputs = baseline_inputs();
    inputs.player.balance = -200;
    inputs.bankruptcy = BankruptcyState {
        penalty_games_remaining: 5,
        last_bankruptcy_at: Some(1_000),
    };
    let weights = calculate_land_weights(&inputs);
    assert_eq!(weights.get(Land::Swamp), weights.max());
}

#[test]
fn test_heavy_tipper_boosts_plains() {
    let mut inputs = baseline_inputs();
    inputs.player.balance = 50;
    inputs.tips = TipStats {
        total_sent: 600,
        tips_sent_count: 25,
    };
    let weights = calculate_land_weights(&inputs);
    assert_eq!(weights.get(Land::Plains), weights.max());
}

#[test]
fn test_average_player_forest_leads() {
    let mut inputs = baseline_inputs();
    inputs.player.wins = 3;
    inputs.player.losses = 3;
    inputs.player.glicko_rating = None;
    inputs.degen.total = 5;
    let weights = calculate_land_weights(&inputs);
    assert!(weights.get(Land::Forest) >= weights.max() - 0.01);
}

#[test]
fn test_ash_fan_role_boosts_island() {
    let mut inputs = baseline_inputs();
    inputs.player.balance = 600;
    let without_ash = calculate_land_weights(&inputs);
    inputs.is_ash_fan = true;
    let with_ash = calculate_land_weights(&inputs);
    assert_eq!(
        with_ash.get(Land::Island),
        without_ash.get(Land::Island) + 4.0
    );
}

#[test]
fn test_losing_streak_boosts_mountain() {
    let mut inputs = baseline_inputs();
    inputs.degen.total = 35;
    let without_streak = calculate_land_weights(&inputs);
    inputs.current_streak = -5;
    let with_streak = calculate_land_weights(&inputs);
    assert_eq!(
        with_streak.get(Land::Mountain),
        without_streak.get(Land::Mountain) + 1.0
    );
}

#[test]
fn test_all_weights_positive() {
    let mut inputs = baseline_inputs();
    inputs.player.balance = -400;
    inputs.lowest_balance = Some(-400);
    inputs.degen = DegenScore {
        total: 120,
        max_leverage_score: 25,
        loss_chase_score: 5,
        bet_size_score: 25,
        negative_loan_bonus: 25,
        bankruptcy_score: 15,
        debt_depth_score: 20,
    };
    inputs.bankruptcy = BankruptcyState {
        penalty_games_remaining: 3,
        last_bankruptcy_at: Some(1),
    };
    assert!(calculate_land_weights(&inputs).all_positive());
}
