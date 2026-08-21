//! Daily mana assignment policy and adapter-neutral orchestration.
//!
//! The module keeps the four-AM mana clock, weighted land selection, batch
//! loading, Guardian reconciliation, and presentation model independent from
//! Discord. SQLite integration delegates to `cama-db`, which only opens the
//! existing Python-managed schema.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use cama_db::mana_service_repository::{
    GuildManaRow as DbGuildManaRow, ManaClaim as DbManaClaim,
    ManaRepository as SqliteManaRepository, ManaRow as DbManaRow,
};
use chrono::{DateTime, Duration, FixedOffset, Timelike};
use thiserror::Error;

pub type DiscordId = i64;
pub type GuildId = i64;

pub const RESET_HOUR: u32 = 4;
pub const PLAINS_GUARDIAN_CAPACITY: i64 = 25;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Land {
    Island,
    Mountain,
    Forest,
    Plains,
    Swamp,
}

impl Land {
    pub const ORDERED: [Self; 5] = [
        Self::Island,
        Self::Mountain,
        Self::Forest,
        Self::Plains,
        Self::Swamp,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Island => "Island",
            Self::Mountain => "Mountain",
            Self::Forest => "Forest",
            Self::Plains => "Plains",
            Self::Swamp => "Swamp",
        }
    }

    #[must_use]
    pub const fn color(self) -> &'static str {
        match self {
            Self::Island => "Blue",
            Self::Mountain => "Red",
            Self::Forest => "Green",
            Self::Plains => "White",
            Self::Swamp => "Black",
        }
    }

    #[must_use]
    pub const fn emoji(self) -> &'static str {
        match self {
            Self::Island => "🏝️",
            Self::Mountain => "⛰️",
            Self::Forest => "🌲",
            Self::Plains => "🌾",
            Self::Swamp => "🌿",
        }
    }

    #[must_use]
    pub const fn embed_color(self) -> u32 {
        match self {
            Self::Island => 0x34_98_DB,
            Self::Mountain => 0xE7_4C_3C,
            Self::Forest => 0x27_AE_60,
            Self::Plains => 0xF5_F5_DC,
            Self::Swamp => 0x2C_3E_50,
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Island" => Some(Self::Island),
            "Mountain" => Some(Self::Mountain),
            "Forest" => Some(Self::Forest),
            "Plains" => Some(Self::Plains),
            "Swamp" => Some(Self::Swamp),
            _ => None,
        }
    }
}

impl fmt::Display for Land {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Resolve the date label for a caller-provided Los Angeles local instant.
///
/// `FixedOffset` makes the offset explicit and deterministic. Runtime wiring
/// is responsible for constructing the instant with the correct LA offset for
/// that date.
#[must_use]
pub fn mana_day_label(now_la: DateTime<FixedOffset>) -> String {
    let effective = if now_la.hour() < RESET_HOUR {
        now_la - Duration::days(1)
    } else {
        now_la
    };
    effective.format("%Y-%m-%d").to_string()
}

/// Return the Unix timestamp of the four-AM boundary containing `now_la`.
#[must_use]
pub fn mana_day_start_timestamp(now_la: DateTime<FixedOffset>) -> i64 {
    let boundary = now_la
        .with_hour(RESET_HOUR)
        .and_then(|value| value.with_minute(0))
        .and_then(|value| value.with_second(0))
        .and_then(|value| value.with_nanosecond(0))
        .expect("four AM is valid for a fixed-offset timestamp");
    if now_la < boundary {
        (boundary - Duration::days(1)).timestamp()
    } else {
        boundary.timestamp()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMana {
    pub land: Land,
    pub assigned_date: String,
    pub consumed_today: bool,
    pub white_shield_remaining: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuildMana {
    pub discord_id: DiscordId,
    pub mana: StoredMana,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedMana {
    pub discord_id: DiscordId,
    pub mana: StoredMana,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManaDetails {
    pub land: Land,
    pub color: &'static str,
    pub emoji: &'static str,
    pub assigned_date: String,
    pub retro_refund: i64,
    pub guardian_remaining: i64,
    pub consumed: bool,
}

impl ManaDetails {
    fn from_stored(stored: &StoredMana, today: &str) -> Self {
        let current = stored.assigned_date == today;
        Self {
            land: stored.land,
            color: stored.land.color(),
            emoji: stored.land.emoji(),
            assigned_date: stored.assigned_date.clone(),
            retro_refund: 0,
            guardian_remaining: if current && stored.land == Land::Plains {
                stored.white_shield_remaining
            } else {
                0
            },
            consumed: current && stored.consumed_today,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DailyAssignment {
    pub discord_id: DiscordId,
    pub details: ManaDetails,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardRow {
    pub discord_id: DiscordId,
    pub land: Land,
    pub assigned_date: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssignmentBoard {
    pub assignments: Vec<DailyAssignment>,
    pub board: Vec<BoardRow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManaEmbed {
    pub title: String,
    pub color: u32,
    pub fields: Vec<EmbedField>,
}

/// Build the Discord-neutral payload used by the single-player mana embed.
#[must_use]
pub fn build_single_embed(display_name: &str, mana: &ManaDetails, today: &str) -> ManaEmbed {
    let mut fields = vec![EmbedField {
        name: "Land".to_owned(),
        value: format!("{} **{}** · {} Mana", mana.emoji, mana.land, mana.color),
        inline: false,
    }];
    if mana.land == Land::Plains && mana.assigned_date == today && !mana.consumed {
        let retro = if mana.retro_refund > 0 {
            format!(
                " Recovered **{} JC** from earlier hostile losses.",
                mana.retro_refund
            )
        } else {
            String::new()
        };
        fields.push(EmbedField {
            name: "🌾 Guardian Aura".to_owned(),
            value: format!(
                "Absorbs 50% of hostile JC losses until the 4 AM reset \
                 (**{}/25 JC** capacity remaining).{}",
                mana.guardian_remaining, retro
            ),
            inline: false,
        });
    }
    ManaEmbed {
        title: format!("🔮 Daily Mana — {display_name}"),
        color: mana.land.embed_color(),
        fields,
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerProfile {
    pub discord_id: Option<DiscordId>,
    pub balance: i64,
    pub wins: i64,
    pub losses: i64,
    pub glicko_rating: Option<f64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DegenScore {
    pub total: i64,
    pub max_leverage_score: i64,
    pub loss_chase_score: i64,
    pub bet_size_score: i64,
    pub negative_loan_bonus: i64,
    pub bankruptcy_score: i64,
    pub debt_depth_score: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BankruptcyState {
    pub penalty_games_remaining: i64,
    pub last_bankruptcy_at: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TipStats {
    pub total_sent: i64,
    pub tips_sent_count: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BetOutcome {
    Won,
    Lost,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WeightInputs {
    pub player: PlayerProfile,
    pub lowest_balance: Option<i64>,
    pub degen: DegenScore,
    pub bankruptcy: BankruptcyState,
    pub tips: TipStats,
    pub current_streak: i64,
    pub is_ash_fan: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LandWeights {
    values: BTreeMap<Land, f64>,
}

impl LandWeights {
    fn baseline() -> Self {
        Self {
            values: BTreeMap::from([
                (Land::Island, 1.0),
                (Land::Mountain, 1.0),
                (Land::Forest, 2.0),
                (Land::Plains, 1.0),
                (Land::Swamp, 1.0),
            ]),
        }
    }

    fn add(&mut self, land: Land, amount: f64) {
        *self.values.entry(land).or_default() += amount;
    }

    #[must_use]
    pub fn get(&self, land: Land) -> f64 {
        self.values.get(&land).copied().unwrap_or_default()
    }

    #[must_use]
    pub fn max(&self) -> f64 {
        self.values.values().copied().fold(0.0, f64::max)
    }

    #[must_use]
    pub fn all_positive(&self) -> bool {
        self.values.values().all(|value| *value > 0.0)
    }

    fn total(&self) -> f64 {
        self.values.values().sum()
    }
}

/// Calculate the five unnormalised Python-compatible land weights.
#[must_use]
pub fn calculate_land_weights(inputs: &WeightInputs) -> LandWeights {
    let mut weights = LandWeights::baseline();
    let player = &inputs.player;
    let total_games = player.wins + player.losses;
    let win_rate = if total_games > 0 {
        player.wins as f64 / total_games as f64
    } else {
        0.0
    };

    if player.balance >= 500 {
        weights.add(Land::Island, 6.0);
    } else if player.balance >= 200 {
        weights.add(Land::Island, 4.0);
    } else if player.balance >= 100 {
        weights.add(Land::Island, 2.0);
    }
    if let Some(glicko) = player.glicko_rating {
        if glicko >= 4_500.0 {
            weights.add(Land::Island, 4.0);
        } else if glicko >= 3_000.0 {
            weights.add(Land::Island, 2.0);
        }
    }
    if win_rate >= 0.65 && total_games >= 10 {
        weights.add(Land::Island, 2.0);
    }
    if inputs.is_ash_fan {
        weights.add(Land::Island, 4.0);
    }
    if player.balance > 0
        && inputs.bankruptcy.last_bankruptcy_at.is_none()
        && inputs.degen.total < 30
    {
        weights.add(Land::Island, 1.5);
    }

    if inputs.degen.total >= 80 {
        weights.add(Land::Mountain, 6.0);
    } else if inputs.degen.total >= 55 {
        weights.add(Land::Mountain, 4.0);
    } else if inputs.degen.total >= 30 {
        weights.add(Land::Mountain, 2.0);
    }
    if inputs.degen.max_leverage_score >= 20 {
        weights.add(Land::Mountain, 2.0);
    }
    if inputs.degen.loss_chase_score >= 4 {
        weights.add(Land::Mountain, 1.5);
    }
    if inputs.degen.bet_size_score >= 20 {
        weights.add(Land::Mountain, 1.5);
    }
    if inputs.degen.negative_loan_bonus > 0 {
        weights.add(Land::Mountain, 2.0);
    }
    if inputs.current_streak < -3 {
        weights.add(Land::Mountain, 1.0);
    }

    if inputs.bankruptcy.penalty_games_remaining > 0 {
        weights.add(Land::Swamp, 7.0);
    }
    if player.balance < 0 {
        weights.add(Land::Swamp, 4.0);
    } else if player.balance < 5 {
        weights.add(Land::Swamp, 2.0);
    }
    if inputs.lowest_balance.is_some_and(|balance| balance <= -300) {
        weights.add(Land::Swamp, 2.0);
    }
    if inputs.degen.bankruptcy_score >= 10 {
        weights.add(Land::Swamp, 2.0);
    }
    if inputs.degen.debt_depth_score >= 15 {
        weights.add(Land::Swamp, 1.5);
    }
    if total_games > 20 && win_rate < 0.30 {
        weights.add(Land::Swamp, 1.5);
    }

    if inputs.tips.total_sent >= 500 {
        weights.add(Land::Plains, 6.0);
    } else if inputs.tips.total_sent >= 200 {
        weights.add(Land::Plains, 4.0);
    } else if inputs.tips.total_sent >= 50 {
        weights.add(Land::Plains, 2.0);
    }
    if inputs.tips.tips_sent_count >= 20 {
        weights.add(Land::Plains, 2.0);
    } else if inputs.tips.tips_sent_count >= 10 {
        weights.add(Land::Plains, 1.0);
    }
    if player.balance > 0 && inputs.degen.total < 20 && inputs.tips.tips_sent_count >= 1 {
        weights.add(Land::Plains, 1.5);
    }
    if (0.45..=0.60).contains(&win_rate) && total_games >= 20 {
        weights.add(Land::Plains, 1.0);
    }

    if total_games >= 50 {
        weights.add(Land::Forest, 2.0);
    } else if total_games >= 20 {
        weights.add(Land::Forest, 1.0);
    }
    if (0.40..=0.60).contains(&win_rate) && total_games >= 10 {
        weights.add(Land::Forest, 2.0);
    }
    if (5..=99).contains(&player.balance) {
        weights.add(Land::Forest, 1.5);
    }
    if (10..=40).contains(&inputs.degen.total) {
        weights.add(Land::Forest, 1.5);
    }
    if player.balance >= 0 && inputs.bankruptcy.last_bankruptcy_at.is_none() && player.balance < 100
    {
        weights.add(Land::Forest, 1.0);
    }
    if (1..=4).contains(&inputs.tips.tips_sent_count) {
        weights.add(Land::Forest, 0.5);
    }
    weights
}

#[must_use]
pub fn current_streak(history: &[BetOutcome]) -> i64 {
    let Some(last) = history.last() else {
        return 0;
    };
    let streak_won = *last == BetOutcome::Won;
    let count = history
        .iter()
        .rev()
        .take_while(|outcome| (**outcome == BetOutcome::Won) == streak_won)
        .count();
    if streak_won {
        count as i64
    } else {
        -(count as i64)
    }
}

pub trait LandEntropy {
    fn choose(&mut self, weights: &LandWeights) -> Land;
}

/// Seedable xorshift entropy with real weighted selection and reproducible
/// output for tests and replay tooling.
#[derive(Clone, Debug)]
pub struct XorShift64Entropy {
    state: u64,
}

impl XorShift64Entropy {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.state = value;
        value
    }
}

impl LandEntropy for XorShift64Entropy {
    fn choose(&mut self, weights: &LandWeights) -> Land {
        let unit = self.next() as f64 / u64::MAX as f64;
        let mut ticket = unit * weights.total();
        for land in Land::ORDERED {
            ticket -= weights.get(land);
            if ticket <= 0.0 {
                return land;
            }
        }
        Land::Swamp
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ManaServiceError {
    #[error("Already assigned today")]
    AlreadyAssigned,
    #[error("unknown mana land: {0}")]
    UnknownLand(String),
    #[error("mana repository failed: {0}")]
    Repository(String),
    #[error("mana data source failed: {0}")]
    Data(String),
}

pub trait ManaAssignmentStore {
    fn get_mana(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<Option<StoredMana>, ManaServiceError>;

    fn get_all_mana(&mut self, guild_id: GuildId) -> Result<Vec<GuildMana>, ManaServiceError>;

    fn claim_mana_atomic(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        land: Land,
        assigned_date: &str,
    ) -> Result<bool, ManaServiceError>;

    fn claim_mana_batch_atomic(
        &mut self,
        assignments: &[(DiscordId, Land)],
        guild_id: GuildId,
        assigned_date: &str,
    ) -> Result<Vec<ClaimedMana>, ManaServiceError>;
}

impl ManaAssignmentStore for SqliteManaRepository {
    fn get_mana(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<Option<StoredMana>, ManaServiceError> {
        SqliteManaRepository::get_mana(self, discord_id, Some(guild_id))
            .map_err(repository_error)?
            .map(stored_from_db)
            .transpose()
    }

    fn get_all_mana(&mut self, guild_id: GuildId) -> Result<Vec<GuildMana>, ManaServiceError> {
        SqliteManaRepository::get_all_mana(self, Some(guild_id))
            .map_err(repository_error)?
            .into_iter()
            .map(guild_from_db)
            .collect()
    }

    fn claim_mana_atomic(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        land: Land,
        assigned_date: &str,
    ) -> Result<bool, ManaServiceError> {
        SqliteManaRepository::claim_mana_atomic(
            self,
            discord_id,
            Some(guild_id),
            land.name(),
            assigned_date,
        )
        .map_err(repository_error)
    }

    fn claim_mana_batch_atomic(
        &mut self,
        assignments: &[(DiscordId, Land)],
        guild_id: GuildId,
        assigned_date: &str,
    ) -> Result<Vec<ClaimedMana>, ManaServiceError> {
        let db_assignments = assignments
            .iter()
            .map(|(discord_id, land)| (*discord_id, land.name().to_owned()))
            .collect::<Vec<_>>();
        SqliteManaRepository::claim_mana_batch_atomic(
            self,
            &db_assignments,
            Some(guild_id),
            assigned_date,
        )
        .map_err(repository_error)?
        .into_iter()
        .map(claimed_from_db)
        .collect()
    }
}

fn repository_error(error: impl fmt::Display) -> ManaServiceError {
    ManaServiceError::Repository(error.to_string())
}

fn parse_land(name: String) -> Result<Land, ManaServiceError> {
    Land::from_name(&name).ok_or(ManaServiceError::UnknownLand(name))
}

fn stored_from_db(row: DbManaRow) -> Result<StoredMana, ManaServiceError> {
    Ok(StoredMana {
        land: parse_land(row.current_land)?,
        assigned_date: row.assigned_date,
        consumed_today: row.consumed_today,
        white_shield_remaining: row.white_shield_remaining,
    })
}

fn guild_from_db(row: DbGuildManaRow) -> Result<GuildMana, ManaServiceError> {
    Ok(GuildMana {
        discord_id: row.discord_id,
        mana: stored_from_db(row.mana)?,
    })
}

fn claimed_from_db(row: DbManaClaim) -> Result<ClaimedMana, ManaServiceError> {
    Ok(ClaimedMana {
        discord_id: row.discord_id,
        mana: StoredMana {
            land: parse_land(row.current_land)?,
            assigned_date: row.assigned_date,
            consumed_today: false,
            white_shield_remaining: row.white_shield_remaining,
        },
    })
}

pub trait ManaDataSource {
    fn player(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<Option<PlayerProfile>, ManaServiceError>;

    fn players(&mut self, guild_id: GuildId) -> Result<Vec<PlayerProfile>, ManaServiceError>;

    fn lowest_balance(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<Option<i64>, ManaServiceError>;

    fn bet_history(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<Vec<BetOutcome>, ManaServiceError>;

    fn degen_score(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        history: &[BetOutcome],
    ) -> Result<DegenScore, ManaServiceError>;

    fn bankruptcy_state(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<BankruptcyState, ManaServiceError>;

    fn tip_stats(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<TipStats, ManaServiceError>;

    fn lowest_balances_bulk(
        &mut self,
        discord_ids: &[DiscordId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<DiscordId, Option<i64>>, ManaServiceError>;

    fn degen_scores_bulk(
        &mut self,
        discord_ids: &[DiscordId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<DiscordId, DegenScore>, ManaServiceError>;

    fn bankruptcy_states_bulk(
        &mut self,
        discord_ids: &[DiscordId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<DiscordId, BankruptcyState>, ManaServiceError>;

    fn tip_stats_bulk(
        &mut self,
        discord_ids: &[DiscordId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<DiscordId, TipStats>, ManaServiceError>;

    fn current_streaks_bulk(
        &mut self,
        discord_ids: &[DiscordId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<DiscordId, i64>, ManaServiceError>;
}

pub trait GuardianProtection {
    fn reconcile_guardian(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        since: i64,
    ) -> Result<i64, ManaServiceError>;

    fn reconcile_guardians(
        &mut self,
        discord_ids: &[DiscordId],
        guild_id: GuildId,
        since: i64,
    ) -> Result<BTreeMap<DiscordId, i64>, ManaServiceError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopGuardianProtection;

impl GuardianProtection for NoopGuardianProtection {
    fn reconcile_guardian(
        &mut self,
        _discord_id: DiscordId,
        _guild_id: GuildId,
        _since: i64,
    ) -> Result<i64, ManaServiceError> {
        Ok(0)
    }

    fn reconcile_guardians(
        &mut self,
        _discord_ids: &[DiscordId],
        _guild_id: GuildId,
        _since: i64,
    ) -> Result<BTreeMap<DiscordId, i64>, ManaServiceError> {
        Ok(BTreeMap::new())
    }
}

pub struct ManaService<R, D, E, P> {
    repository: R,
    data: D,
    entropy: E,
    protection: P,
}

impl<R, D, E, P> ManaService<R, D, E, P> {
    #[must_use]
    pub const fn new(repository: R, data: D, entropy: E, protection: P) -> Self {
        Self {
            repository,
            data,
            entropy,
            protection,
        }
    }

    #[must_use]
    pub fn repository(&self) -> &R {
        &self.repository
    }

    #[must_use]
    pub fn data(&self) -> &D {
        &self.data
    }

    #[must_use]
    pub fn protection(&self) -> &P {
        &self.protection
    }

    #[must_use]
    pub fn into_parts(self) -> (R, D, E, P) {
        (self.repository, self.data, self.entropy, self.protection)
    }
}

impl<R, D, E, P> ManaService<R, D, E, P>
where
    R: ManaAssignmentStore,
    D: ManaDataSource,
    E: LandEntropy,
    P: GuardianProtection,
{
    pub fn has_assigned_today(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        today: &str,
    ) -> Result<bool, ManaServiceError> {
        Ok(self
            .repository
            .get_mana(discord_id, guild_id)?
            .is_some_and(|row| row.assigned_date == today))
    }

    pub fn get_current_mana(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        today: &str,
    ) -> Result<Option<ManaDetails>, ManaServiceError> {
        Ok(self
            .repository
            .get_mana(discord_id, guild_id)?
            .as_ref()
            .map(|row| ManaDetails::from_stored(row, today)))
    }

    pub fn assign_daily_mana(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        today: &str,
        day_start_timestamp: i64,
        is_ash_fan: bool,
    ) -> Result<ManaDetails, ManaServiceError> {
        let history = self.data.bet_history(discord_id, guild_id)?;
        let inputs = WeightInputs {
            player: self.data.player(discord_id, guild_id)?.unwrap_or_default(),
            lowest_balance: self.data.lowest_balance(discord_id, guild_id)?,
            degen: self.data.degen_score(discord_id, guild_id, &history)?,
            bankruptcy: self.data.bankruptcy_state(discord_id, guild_id)?,
            tips: self.data.tip_stats(discord_id, guild_id)?,
            current_streak: current_streak(&history),
            is_ash_fan,
        };
        let land = self.entropy.choose(&calculate_land_weights(&inputs));
        if !self
            .repository
            .claim_mana_atomic(discord_id, guild_id, land, today)?
        {
            return Err(ManaServiceError::AlreadyAssigned);
        }

        // Matching Python, post-claim reconciliation is best effort: a
        // protection failure never invalidates the committed daily claim.
        let retro_refund = if land == Land::Plains {
            self.protection
                .reconcile_guardian(discord_id, guild_id, day_start_timestamp)
                .unwrap_or_default()
        } else {
            0
        };
        let guardian_remaining = self
            .repository
            .get_mana(discord_id, guild_id)?
            .map_or(0, |row| row.white_shield_remaining);
        Ok(ManaDetails {
            land,
            color: land.color(),
            emoji: land.emoji(),
            assigned_date: today.to_owned(),
            retro_refund,
            guardian_remaining: if land == Land::Plains {
                guardian_remaining
            } else {
                0
            },
            consumed: false,
        })
    }

    pub fn assign_all_daily_mana_with_board(
        &mut self,
        guild_id: GuildId,
        today: &str,
        day_start_timestamp: i64,
        ash_fan_ids: &BTreeSet<DiscordId>,
    ) -> Result<AssignmentBoard, ManaServiceError> {
        let players = self.data.players(guild_id)?;
        let current = self.repository.get_all_mana(guild_id)?;
        let mut board = current
            .into_iter()
            .map(|row| {
                (
                    row.discord_id,
                    BoardRow {
                        discord_id: row.discord_id,
                        land: row.mana.land,
                        assigned_date: row.mana.assigned_date,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let candidates = players
            .into_iter()
            .filter_map(|player| {
                let discord_id = player.discord_id?;
                let assigned = board
                    .get(&discord_id)
                    .is_some_and(|row| row.assigned_date == today);
                (!assigned).then_some((discord_id, player))
            })
            .collect::<Vec<_>>();
        let ids = candidates.iter().map(|(id, _)| *id).collect::<Vec<_>>();

        // Python treats optional bulk-stat failures as empty mappings so a
        // daily guild roll can continue with neutral defaults.
        let lowest = self
            .data
            .lowest_balances_bulk(&ids, guild_id)
            .unwrap_or_default();
        let degen = self
            .data
            .degen_scores_bulk(&ids, guild_id)
            .unwrap_or_default();
        let bankruptcies = self
            .data
            .bankruptcy_states_bulk(&ids, guild_id)
            .unwrap_or_default();
        let tips = self.data.tip_stats_bulk(&ids, guild_id).unwrap_or_default();
        let streaks = self
            .data
            .current_streaks_bulk(&ids, guild_id)
            .unwrap_or_default();

        let assignments = candidates
            .into_iter()
            .map(|(discord_id, player)| {
                let inputs = WeightInputs {
                    player,
                    lowest_balance: lowest.get(&discord_id).copied().flatten(),
                    degen: degen.get(&discord_id).cloned().unwrap_or_default(),
                    bankruptcy: bankruptcies.get(&discord_id).cloned().unwrap_or_default(),
                    tips: tips.get(&discord_id).cloned().unwrap_or_default(),
                    current_streak: streaks.get(&discord_id).copied().unwrap_or_default(),
                    is_ash_fan: ash_fan_ids.contains(&discord_id),
                };
                (
                    discord_id,
                    self.entropy.choose(&calculate_land_weights(&inputs)),
                )
            })
            .collect::<Vec<_>>();
        let claimed = self
            .repository
            .claim_mana_batch_atomic(&assignments, guild_id, today)?;
        let plains_ids = claimed
            .iter()
            .filter(|row| row.mana.land == Land::Plains)
            .map(|row| row.discord_id)
            .collect::<Vec<_>>();
        let refunds = if plains_ids.is_empty() {
            BTreeMap::new()
        } else {
            self.protection
                .reconcile_guardians(&plains_ids, guild_id, day_start_timestamp)
                .unwrap_or_default()
        };

        let mut fresh = Vec::with_capacity(claimed.len());
        for row in claimed {
            let refund = refunds.get(&row.discord_id).copied().unwrap_or_default();
            let guardian_remaining = (row.mana.white_shield_remaining - refund).max(0);
            let details = ManaDetails {
                land: row.mana.land,
                color: row.mana.land.color(),
                emoji: row.mana.land.emoji(),
                assigned_date: row.mana.assigned_date.clone(),
                retro_refund: refund,
                guardian_remaining,
                consumed: false,
            };
            board.insert(
                row.discord_id,
                BoardRow {
                    discord_id: row.discord_id,
                    land: row.mana.land,
                    assigned_date: row.mana.assigned_date,
                },
            );
            fresh.push(DailyAssignment {
                discord_id: row.discord_id,
                details,
            });
        }
        Ok(AssignmentBoard {
            assignments: fresh,
            board: board.into_values().collect(),
        })
    }
}

#[cfg(test)]
#[path = "mana_service/tests.rs"]
mod tests;
