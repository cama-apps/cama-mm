//! SQL owned by the Dig runtime commit boundary and its read models.
//!
//! `cama-app` orchestrates the Dig transaction (BEGIN IMMEDIATE, CAS checks,
//! error mapping, delivery JSON policy) but every statement text lives here so
//! the canonical schema/coercion authority and the repository write audit
//! cover Dig runtime SQL.  Functions take `&Connection`; a caller-owned
//! `rusqlite::Transaction` dereferences to `Connection`, so the same function
//! serves both the in-transaction (`*_in`-style) and standalone paths.

use cama_domain::pet::PetDigWorkClaim;
use rusqlite::{Connection, OptionalExtension, params};

/// A single migrated inventory row needed by the Dig application aggregate.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DigRuntimeInventoryItem {
    pub id: i64,
    pub item_type: String,
    pub queued: bool,
}

/// A single migrated artifact row needed by the Dig application aggregate.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DigRuntimeArtifact {
    pub id: i64,
    pub artifact_id: String,
    pub is_relic: bool,
    pub equipped: bool,
}

/// A persisted gear row needed to apply pickaxe and combat modifiers before a
/// dig is settled. Keeping this in the application snapshot is important:
/// selecting/equipping gear in a component must not race a concurrent dig.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DigRuntimeGear {
    pub id: i64,
    pub slot: String,
    pub tier: i64,
    pub durability: i64,
    pub equipped: bool,
    pub acquired_at: i64,
    pub source: String,
    pub item_id: Option<String>,
}

/// The tunnel columns touched by a real Dig commit.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DigRuntimeTunnel {
    pub discord_id: i64,
    pub guild_id: i64,
    pub depth: i64,
    pub max_depth: i64,
    pub total_digs: i64,
    pub total_jc_earned: i64,
    pub last_dig_at: Option<i64>,
    pub luminosity: i64,
    pub tunnel_name: String,
    pub prestige_level: i64,
    pub prestige_perks: String,
    pub boss_progress: String,
    pub boss_attempts: String,
    pub route_state: Option<String>,
    pub injury_state: Option<String>,
    pub hard_hat_charges: i64,
    pub reinforced_until: i64,
    pub void_bait_digs: i64,
    pub sonar_skip_pending: bool,
    pub temp_buffs: Option<String>,
    pub temp_curses: Option<String>,
    pub stat_strength: i64,
    pub stat_smarts: i64,
    pub stat_stamina: i64,
    pub stat_points: i64,
    pub paid_digs_today: i64,
    pub paid_dig_date: Option<String>,
    pub pickaxe_tier: i64,
    pub current_run_jc: i64,
    pub current_run_artifacts: i64,
    pub current_run_events: i64,
    pub best_run_score: i64,
    pub total_prestige_score: i64,
    pub streak_days: i64,
    pub streak_last_date: Option<String>,
    pub auto_buy_torch: bool,
    pub auto_buy_hard_hat: bool,
    pub auto_buy_grappling_hook: bool,
    pub trap_active: bool,
    pub trap_free_today: bool,
    pub trap_date: Option<String>,
    pub insured_until: Option<i64>,
    pub revenge_target: Option<i64>,
    pub revenge_type: Option<String>,
    pub revenge_until: Option<i64>,
    pub cheer_data: Option<String>,
    pub grappling_hook_charges: i64,
    pub lantern_stub_date: Option<String>,
    pub thick_skin_date: Option<String>,
    pub mutations: Option<String>,
    pub miner_origin: String,
    pub miner_about: String,
    pub engine_mode: String,
    pub stat_boss_awards: String,
    pub stinger_curse: Option<String>,
    pub last_lum_update_at: Option<i64>,
    pub pinnacle_boss_id: Option<String>,
    pub pinnacle_phase: i64,
    pub pinnacle_hp_remaining: Option<i64>,
    pub pinnacle_last_engaged_at: Option<i64>,
    pub retreat_cooldown_until: Option<i64>,
    pub last_cheer_at: Option<i64>,
    pub cavein_free_streak: i64,
    pub relic_trim_notice: bool,
}

impl DigRuntimeTunnel {
    #[must_use]
    pub fn new(discord_id: i64, guild_id: i64, _now: i64) -> Self {
        Self {
            discord_id,
            guild_id,
            depth: 0,
            max_depth: 0,
            total_digs: 0,
            total_jc_earned: 0,
            last_dig_at: None,
            luminosity: 100,
            tunnel_name: format!("Miner {discord_id}"),
            prestige_level: 0,
            prestige_perks: "[]".to_owned(),
            boss_progress: "{}".to_owned(),
            boss_attempts: "{}".to_owned(),
            route_state: None,
            injury_state: None,
            hard_hat_charges: 0,
            reinforced_until: 0,
            void_bait_digs: 0,
            sonar_skip_pending: false,
            temp_buffs: None,
            temp_curses: None,
            stat_strength: 0,
            stat_smarts: 0,
            stat_stamina: 0,
            stat_points: 5,
            paid_digs_today: 0,
            paid_dig_date: None,
            pickaxe_tier: 0,
            current_run_jc: 0,
            current_run_artifacts: 0,
            current_run_events: 0,
            best_run_score: 0,
            total_prestige_score: 0,
            streak_days: 0,
            streak_last_date: None,
            auto_buy_torch: false,
            auto_buy_hard_hat: false,
            auto_buy_grappling_hook: false,
            trap_active: false,
            trap_free_today: true,
            trap_date: None,
            insured_until: None,
            revenge_target: None,
            revenge_type: None,
            revenge_until: None,
            cheer_data: None,
            grappling_hook_charges: 0,
            lantern_stub_date: None,
            thick_skin_date: None,
            mutations: None,
            miner_origin: String::new(),
            miner_about: String::new(),
            engine_mode: "legacy".to_owned(),
            stat_boss_awards: "[]".to_owned(),
            stinger_curse: None,
            last_lum_update_at: None,
            pinnacle_boss_id: None,
            pinnacle_phase: 0,
            pinnacle_hp_remaining: None,
            pinnacle_last_engaged_at: None,
            retreat_cooldown_until: None,
            last_cheer_at: None,
            cavein_free_streak: 0,
            relic_trim_notice: false,
        }
    }
}

/// One tunnel row projected for the Dig leaderboard read model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigTunnelLeaderboardRow {
    pub discord_id: i64,
    pub guild_id: i64,
    pub tunnel_name: Option<String>,
    pub depth: i64,
    pub total_digs: i64,
    pub total_jc_earned: i64,
    pub prestige_level: i64,
    pub best_run_score: i64,
}

/// One hall-of-fame row: (discord_id, tunnel_name, prestige_level, best_run_score).
pub type DigHallOfFameEntryRow = (i64, Option<String>, i64, i64);

/// One guild stats row: (discord_id, tunnel_name, depth, total_digs, total_jc_earned).
pub type DigGuildTunnelStatRow = (i64, Option<String>, i64, i64, i64);

/// One persisted artifact row projected for the collection read model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigArtifactCollectionRow {
    pub id: i64,
    pub discord_id: i64,
    pub guild_id: i64,
    pub artifact_id: String,
    pub found_at: i64,
    pub is_relic: bool,
    pub equipped: bool,
}

// ---------------------------------------------------------------------------
// players
// ---------------------------------------------------------------------------

/// Read one player's coalesced wallet balance.
pub fn player_balance(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
}

/// True when a registered player row exists.
pub fn player_exists(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Unconditional wallet debit used by the channel-admission penalty.
pub fn debit_player_balance(
    connection: &Connection,
    amount: i64,
    discord_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                updated_at=CURRENT_TIMESTAMP
         WHERE discord_id=?2 AND guild_id=?3",
        params![amount, discord_id, guild_id],
    )
}

/// Wallet CAS keyed on the exact stored balance.
pub fn update_player_balance_cas(
    connection: &Connection,
    new_balance: i64,
    discord_id: i64,
    guild_id: i64,
    expected_balance: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
          WHERE discord_id=?2 AND guild_id=?3 AND jopacoin_balance=?4",
        params![new_balance, discord_id, guild_id, expected_balance],
    )
}

/// Wallet CAS keyed on the coalesced stored balance.
pub fn update_player_balance_coalesce_cas(
    connection: &Connection,
    new_balance: i64,
    discord_id: i64,
    guild_id: i64,
    expected_balance: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
         WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
        params![new_balance, discord_id, guild_id, expected_balance],
    )
}

/// Debit a wallet only when the stored balance covers the cost.
pub fn debit_player_balance_if_sufficient(
    connection: &Connection,
    cost: i64,
    discord_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE players SET jopacoin_balance=jopacoin_balance-?1,
                updated_at=CURRENT_TIMESTAMP
         WHERE discord_id=?2 AND guild_id=?3 AND jopacoin_balance>=?1",
        params![cost, discord_id, guild_id],
    )
}

/// Track the historical low-water mark after a debit.
pub fn refresh_player_lowest_balance(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE players SET lowest_balance_ever=jopacoin_balance
         WHERE discord_id=?1 AND guild_id=?2
           AND (lowest_balance_ever IS NULL OR jopacoin_balance<lowest_balance_ever)",
        params![discord_id, guild_id],
    )
}

// ---------------------------------------------------------------------------
// economy_ledger_context
// ---------------------------------------------------------------------------

/// Install the request-local ledger context row (replacing any prior row).
pub fn set_ledger_context(
    connection: &Connection,
    source: &str,
    actor_id: i64,
    related_type: &str,
    related_id: &str,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    connection.execute("DELETE FROM economy_ledger_context", [])?;
    connection.execute(
        "INSERT INTO economy_ledger_context
            (id,source,actor_id,related_type,related_id,reason,metadata)
         VALUES (1,?1,?2,?3,?4,?5,?6)",
        params![source, actor_id, related_type, related_id, reason, metadata],
    )?;
    Ok(())
}

/// Remove the request-local ledger context row.
pub fn clear_ledger_context(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// tunnels
// ---------------------------------------------------------------------------

const TUNNEL_SELECT: &str = "SELECT depth,max_depth,total_digs,total_jc_earned,last_dig_at,
        luminosity,COALESCE(tunnel_name,?3),prestige_level,
        COALESCE(prestige_perks,'[]'),COALESCE(boss_progress,'{}'),
        COALESCE(boss_attempts,'{}'),route_state,injury_state,
        hard_hat_charges,COALESCE(reinforced_until,0),void_bait_digs,
        sonar_skip_pending,temp_buffs,temp_curses,stat_strength,
        stat_smarts,stat_stamina,stat_points,paid_digs_today,
        paid_dig_date,pickaxe_tier,current_run_jc,current_run_artifacts,
        current_run_events,streak_days,auto_buy_torch,auto_buy_hard_hat,
        best_run_score,total_prestige_score,streak_last_date,
        trap_active,trap_free_today,trap_date,insured_until,
        revenge_target,revenge_type,revenge_until,cheer_data,
        grappling_hook_charges,lantern_stub_date,thick_skin_date,
        mutations,engine_mode,miner_origin,miner_about,stat_boss_awards,
        stinger_curse,last_lum_update_at,pinnacle_boss_id,pinnacle_phase,
        pinnacle_hp_remaining,pinnacle_last_engaged_at,retreat_cooldown_until,
        last_cheer_at,cavein_free_streak,relic_trim_notice,
        auto_buy_grappling_hook
 FROM tunnels WHERE discord_id=?1 AND guild_id=?2";

fn load_tunnel_row(
    row: &rusqlite::Row<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<DigRuntimeTunnel, rusqlite::Error> {
    Ok(DigRuntimeTunnel {
        discord_id,
        guild_id,
        depth: row.get(0)?,
        max_depth: row.get(1)?,
        total_digs: row.get(2)?,
        total_jc_earned: row.get(3)?,
        last_dig_at: row.get(4)?,
        luminosity: row.get(5)?,
        tunnel_name: row.get(6)?,
        prestige_level: row.get(7)?,
        prestige_perks: row.get(8)?,
        boss_progress: row.get(9)?,
        boss_attempts: row.get(10)?,
        route_state: row.get(11)?,
        injury_state: row.get(12)?,
        hard_hat_charges: row.get(13)?,
        reinforced_until: row.get(14)?,
        void_bait_digs: row.get(15)?,
        sonar_skip_pending: row.get::<_, i64>(16)? != 0,
        temp_buffs: row.get(17)?,
        temp_curses: row.get(18)?,
        stat_strength: row.get(19)?,
        stat_smarts: row.get(20)?,
        stat_stamina: row.get(21)?,
        stat_points: row.get(22)?,
        paid_digs_today: row.get(23)?,
        paid_dig_date: row.get(24)?,
        pickaxe_tier: row.get(25)?,
        current_run_jc: row.get(26)?,
        current_run_artifacts: row.get(27)?,
        current_run_events: row.get(28)?,
        streak_days: row.get(29)?,
        auto_buy_torch: row.get::<_, i64>(30)? != 0,
        auto_buy_hard_hat: row.get::<_, i64>(31)? != 0,
        auto_buy_grappling_hook: row.get::<_, i64>(61)? != 0,
        best_run_score: row.get(32)?,
        total_prestige_score: row.get(33)?,
        streak_last_date: row.get(34)?,
        trap_active: row.get::<_, i64>(35)? != 0,
        trap_free_today: row.get::<_, i64>(36)? != 0,
        trap_date: row.get(37)?,
        insured_until: row.get(38)?,
        revenge_target: row.get(39)?,
        revenge_type: row.get(40)?,
        revenge_until: row.get(41)?,
        cheer_data: row.get(42)?,
        grappling_hook_charges: row.get(43)?,
        lantern_stub_date: row.get(44)?,
        thick_skin_date: row.get(45)?,
        mutations: row.get(46)?,
        engine_mode: row.get(47)?,
        miner_origin: row.get(48)?,
        miner_about: row.get(49)?,
        stat_boss_awards: row.get(50)?,
        stinger_curse: row.get(51)?,
        last_lum_update_at: row.get(52)?,
        pinnacle_boss_id: row.get(53)?,
        pinnacle_phase: row.get(54)?,
        pinnacle_hp_remaining: row.get(55)?,
        pinnacle_last_engaged_at: row.get(56)?,
        retreat_cooldown_until: row.get(57)?,
        last_cheer_at: row.get(58)?,
        cavein_free_streak: row.get(59)?,
        relic_trim_notice: row.get::<_, i64>(60)? != 0,
    })
}

/// Load one full tunnel aggregate row.
pub fn tunnel(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<DigRuntimeTunnel>, rusqlite::Error> {
    connection
        .query_row(
            TUNNEL_SELECT,
            params![discord_id, guild_id, format!("Miner {discord_id}")],
            |row| load_tunnel_row(row, discord_id, guild_id),
        )
        .optional()
}

/// Insert a brand-new tunnel aggregate row.
pub fn insert_tunnel(
    connection: &Connection,
    tunnel: &DigRuntimeTunnel,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "INSERT INTO tunnels
         (discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,total_jc_earned,
          luminosity,prestige_level,prestige_perks,boss_progress,boss_attempts,
          route_state,injury_state,hard_hat_charges,reinforced_until,void_bait_digs,
          sonar_skip_pending,temp_buffs,temp_curses,stat_strength,stat_smarts,
          stat_stamina,stat_points,paid_digs_today,paid_dig_date,pickaxe_tier,
          current_run_jc,current_run_artifacts,current_run_events,streak_days,
          auto_buy_torch,auto_buy_hard_hat,last_dig_at,best_run_score,
          total_prestige_score,streak_last_date,trap_active,trap_free_today,
          trap_date,insured_until,revenge_target,revenge_type,revenge_until,
          cheer_data,grappling_hook_charges,lantern_stub_date,thick_skin_date,
          mutations,engine_mode,miner_origin,miner_about,stat_boss_awards,
          stinger_curse,last_lum_update_at,pinnacle_boss_id,pinnacle_phase,
          pinnacle_hp_remaining,pinnacle_last_engaged_at,retreat_cooldown_until,
          last_cheer_at,cavein_free_streak,relic_trim_notice)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,
                 ?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,
                 ?35,?36,?37,?38,?39,?40,?41,?42,?43,?44,?45,?46,?47,?48,?49,?50,
                 ?51,?52,?53,?54,?55,?56,?57,?58,?59,?60,?61,?62,?63)",
        params![
            tunnel.discord_id,
            tunnel.guild_id,
            tunnel.tunnel_name,
            tunnel.depth,
            tunnel.max_depth,
            tunnel.total_digs,
            tunnel.total_jc_earned,
            tunnel.luminosity,
            tunnel.prestige_level,
            tunnel.prestige_perks,
            tunnel.boss_progress,
            tunnel.boss_attempts,
            tunnel.route_state,
            tunnel.injury_state,
            tunnel.hard_hat_charges,
            tunnel.reinforced_until,
            tunnel.void_bait_digs,
            i64::from(tunnel.sonar_skip_pending),
            tunnel.temp_buffs,
            tunnel.temp_curses,
            tunnel.stat_strength,
            tunnel.stat_smarts,
            tunnel.stat_stamina,
            tunnel.stat_points,
            tunnel.paid_digs_today,
            tunnel.paid_dig_date,
            tunnel.pickaxe_tier,
            tunnel.current_run_jc,
            tunnel.current_run_artifacts,
            tunnel.current_run_events,
            tunnel.streak_days,
            i64::from(tunnel.auto_buy_torch),
            i64::from(tunnel.auto_buy_hard_hat),
            tunnel.last_dig_at,
            tunnel.best_run_score,
            tunnel.total_prestige_score,
            tunnel.streak_last_date,
            i64::from(tunnel.trap_active),
            i64::from(tunnel.trap_free_today),
            tunnel.trap_date,
            tunnel.insured_until,
            tunnel.revenge_target,
            tunnel.revenge_type,
            tunnel.revenge_until,
            tunnel.cheer_data,
            tunnel.grappling_hook_charges,
            tunnel.lantern_stub_date,
            tunnel.thick_skin_date,
            tunnel.mutations,
            tunnel.engine_mode,
            tunnel.miner_origin,
            tunnel.miner_about,
            tunnel.stat_boss_awards,
            tunnel.stinger_curse,
            tunnel.last_lum_update_at,
            tunnel.pinnacle_boss_id,
            tunnel.pinnacle_phase,
            tunnel.pinnacle_hp_remaining,
            tunnel.pinnacle_last_engaged_at,
            tunnel.retreat_cooldown_until,
            tunnel.last_cheer_at,
            tunnel.cavein_free_streak,
            i64::from(tunnel.relic_trim_notice),
        ],
    )
}

/// Update the full tunnel aggregate behind the Dig version CAS.
pub fn update_tunnel_cas(
    connection: &Connection,
    tunnel: &DigRuntimeTunnel,
    expected_depth: Option<i64>,
    expected_total_digs: Option<i64>,
    expected_last_dig_at: Option<i64>,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE tunnels SET depth=?1,max_depth=?2,total_digs=?3,total_jc_earned=?4,
            last_dig_at=?5,luminosity=?6,prestige_level=?7,prestige_perks=?8,
            boss_progress=?9,boss_attempts=?10,route_state=?11,injury_state=?12,
            hard_hat_charges=?13,reinforced_until=?14,void_bait_digs=?15,
            sonar_skip_pending=?16,temp_buffs=?17,temp_curses=?18,stat_strength=?19,
            stat_smarts=?20,stat_stamina=?21,stat_points=?22,paid_digs_today=?23,
            paid_dig_date=?24,pickaxe_tier=?25,current_run_jc=?26,
            current_run_artifacts=?27,current_run_events=?28,streak_days=?29,
            auto_buy_torch=?30,auto_buy_hard_hat=?31,tunnel_name=?32,
            best_run_score=?38,total_prestige_score=?39,streak_last_date=?40,
            trap_active=?41,trap_free_today=?42,trap_date=?43,insured_until=?44,
            revenge_target=?45,revenge_type=?46,revenge_until=?47,cheer_data=?48,
            grappling_hook_charges=?49,lantern_stub_date=?50,thick_skin_date=?51,
            mutations=?52,engine_mode=?53,miner_origin=?54,miner_about=?55,
            stat_boss_awards=?56,stinger_curse=?57,last_lum_update_at=?58,
            pinnacle_boss_id=?59,pinnacle_phase=?60,pinnacle_hp_remaining=?61,
            pinnacle_last_engaged_at=?62,retreat_cooldown_until=?63,
            last_cheer_at=?64,cavein_free_streak=?65,relic_trim_notice=?66
         WHERE discord_id=?33 AND guild_id=?34
           AND depth=?35 AND total_digs=?36 AND last_dig_at IS ?37",
        params![
            tunnel.depth,
            tunnel.max_depth,
            tunnel.total_digs,
            tunnel.total_jc_earned,
            tunnel.last_dig_at,
            tunnel.luminosity,
            tunnel.prestige_level,
            tunnel.prestige_perks,
            tunnel.boss_progress,
            tunnel.boss_attempts,
            tunnel.route_state,
            tunnel.injury_state,
            tunnel.hard_hat_charges,
            tunnel.reinforced_until,
            tunnel.void_bait_digs,
            i64::from(tunnel.sonar_skip_pending),
            tunnel.temp_buffs,
            tunnel.temp_curses,
            tunnel.stat_strength,
            tunnel.stat_smarts,
            tunnel.stat_stamina,
            tunnel.stat_points,
            tunnel.paid_digs_today,
            tunnel.paid_dig_date,
            tunnel.pickaxe_tier,
            tunnel.current_run_jc,
            tunnel.current_run_artifacts,
            tunnel.current_run_events,
            tunnel.streak_days,
            i64::from(tunnel.auto_buy_torch),
            i64::from(tunnel.auto_buy_hard_hat),
            tunnel.tunnel_name,
            tunnel.discord_id,
            tunnel.guild_id,
            expected_depth,
            expected_total_digs,
            expected_last_dig_at,
            tunnel.best_run_score,
            tunnel.total_prestige_score,
            tunnel.streak_last_date,
            i64::from(tunnel.trap_active),
            i64::from(tunnel.trap_free_today),
            tunnel.trap_date,
            tunnel.insured_until,
            tunnel.revenge_target,
            tunnel.revenge_type,
            tunnel.revenge_until,
            tunnel.cheer_data,
            tunnel.grappling_hook_charges,
            tunnel.lantern_stub_date,
            tunnel.thick_skin_date,
            tunnel.mutations,
            tunnel.engine_mode,
            tunnel.miner_origin,
            tunnel.miner_about,
            tunnel.stat_boss_awards,
            tunnel.stinger_curse,
            tunnel.last_lum_update_at,
            tunnel.pinnacle_boss_id,
            tunnel.pinnacle_phase,
            tunnel.pinnacle_hp_remaining,
            tunnel.pinnacle_last_engaged_at,
            tunnel.retreat_cooldown_until,
            tunnel.last_cheer_at,
            tunnel.cavein_free_streak,
            i64::from(tunnel.relic_trim_notice),
        ],
    )
}

/// Read one tunnel's depth.
pub fn tunnel_depth(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
}

/// Read one tunnel's depth and max depth.
pub fn tunnel_depth_and_max(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<(i64, i64)>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT depth, max_depth FROM tunnels
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
}

/// Depth-only CAS used by the actor-scoped balance update.
pub fn update_tunnel_depth_cas(
    connection: &Connection,
    depth_after: i64,
    discord_id: i64,
    guild_id: i64,
    expected_depth: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE tunnels SET depth=?1 WHERE discord_id=?2 AND guild_id=?3
          AND depth=?4",
        params![depth_after, discord_id, guild_id, expected_depth],
    )
}

/// Set depth and max depth for the /dig help action.
pub fn set_tunnel_depth_and_max(
    connection: &Connection,
    depth: i64,
    max_depth: i64,
    discord_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE tunnels SET depth=?1, max_depth=?2
         WHERE discord_id=?3 AND guild_id=?4",
        params![depth, max_depth, discord_id, guild_id],
    )
}

/// Clear the dig cooldown timestamp.
pub fn reset_tunnel_cooldown(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE tunnels SET last_dig_at=0
         WHERE discord_id=?1 AND guild_id=?2",
        params![discord_id, guild_id],
    )
}

/// Force a tunnel depth and clear the dig cooldown timestamp.
pub fn set_tunnel_depth_reset_cooldown(
    connection: &Connection,
    depth: i64,
    discord_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE tunnels SET depth=?1, last_dig_at=0
         WHERE discord_id=?2 AND guild_id=?3",
        params![depth, discord_id, guild_id],
    )
}

/// Read the allocated miner stats and the free stat pool.
pub fn tunnel_stat_allocation(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<(i64, i64, i64, i64)>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT stat_strength, stat_smarts, stat_stamina, stat_points
               FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
}

/// Return allocated stats to the free pool.
pub fn respec_tunnel_stats(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE tunnels SET stat_points=stat_points+stat_strength+stat_smarts+stat_stamina,
                stat_strength=0, stat_smarts=0, stat_stamina=0
         WHERE discord_id=?1 AND guild_id=?2",
        params![discord_id, guild_id],
    )
}

/// Toggle the torch auto-buy opt-in.
pub fn set_auto_buy_torch(
    connection: &Connection,
    enabled: i64,
    discord_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE tunnels SET auto_buy_torch=?1 WHERE discord_id=?2 AND guild_id=?3",
        params![enabled, discord_id, guild_id],
    )
}

/// Toggle the hard-hat auto-buy opt-in.
pub fn set_auto_buy_hard_hat(
    connection: &Connection,
    enabled: i64,
    discord_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE tunnels SET auto_buy_hard_hat=?1 WHERE discord_id=?2 AND guild_id=?3",
        params![enabled, discord_id, guild_id],
    )
}

/// Toggle both torch and hard-hat auto-buy opt-ins together.
pub fn set_auto_buy_torch_and_hard_hat(
    connection: &Connection,
    enabled: i64,
    discord_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE tunnels SET auto_buy_torch=?1, auto_buy_hard_hat=?1
         WHERE discord_id=?2 AND guild_id=?3",
        params![enabled, discord_id, guild_id],
    )
}

/// Depth-ordered top-ten tunnels for the runtime leaderboard rendering.
pub fn top_tunnel_depth_rows(
    connection: &Connection,
    guild_id: i64,
) -> Result<Vec<(String, i64)>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT COALESCE(tunnel_name,'Unnamed Tunnel'), depth
         FROM tunnels WHERE guild_id=?1
         ORDER BY depth DESC, total_jc_earned DESC, discord_id ASC LIMIT 10",
    )?;
    statement
        .query_map([guild_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

/// Best-run-ordered top-ten tunnels for the runtime hall of fame rendering.
pub fn hall_of_fame_depth_rows(
    connection: &Connection,
    guild_id: i64,
) -> Result<Vec<(String, i64, i64, i64)>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT COALESCE(tunnel_name,'Unnamed Tunnel'), discord_id,
                best_run_score, prestige_level
         FROM tunnels WHERE guild_id=?1 AND best_run_score > 0
         ORDER BY best_run_score DESC, prestige_level DESC, discord_id ASC LIMIT 10",
    )?;
    statement
        .query_map([guild_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect()
}

/// Prestige-then-depth ordered top-ten tunnels for the leaderboard service.
pub fn leaderboard_tunnel_rows(
    connection: &Connection,
    guild_id: i64,
) -> Result<Vec<DigTunnelLeaderboardRow>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT discord_id,guild_id,tunnel_name,depth,total_digs,total_jc_earned,
                prestige_level,best_run_score
         FROM tunnels
         WHERE guild_id=?1
         ORDER BY prestige_level DESC, depth DESC, discord_id ASC
         LIMIT 10",
    )?;
    statement
        .query_map(params![guild_id], |row| {
            Ok(DigTunnelLeaderboardRow {
                discord_id: row.get(0)?,
                guild_id: row.get(1)?,
                tunnel_name: row.get(2)?,
                depth: row.get(3)?,
                total_digs: row.get(4)?,
                total_jc_earned: row.get(5)?,
                prestige_level: row.get(6)?,
                best_run_score: row.get(7)?,
            })
        })?
        .collect()
}

/// Positive best-run scores in descending score order.
pub fn hall_of_fame_entry_rows(
    connection: &Connection,
    guild_id: i64,
) -> Result<Vec<DigHallOfFameEntryRow>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT discord_id,tunnel_name,prestige_level,best_run_score
         FROM tunnels
         WHERE guild_id=?1 AND best_run_score > 0
         ORDER BY best_run_score DESC
         LIMIT 10",
    )?;
    statement
        .query_map(params![guild_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect()
}

/// All tunnel owners in the exact top-tunnel ordering, for rank lookup.
pub fn tunnel_rank_ids(
    connection: &Connection,
    guild_id: i64,
) -> Result<Vec<i64>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT discord_id FROM tunnels WHERE guild_id=?1
         ORDER BY prestige_level DESC, depth DESC, discord_id ASC",
    )?;
    statement
        .query_map(params![guild_id], |row| row.get::<_, i64>(0))?
        .collect()
}

/// Per-tunnel totals for the guild stats aggregation, in SQLite row order.
pub fn guild_tunnel_stat_rows(
    connection: &Connection,
    guild_id: i64,
) -> Result<Vec<DigGuildTunnelStatRow>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT discord_id,tunnel_name,depth,total_digs,total_jc_earned
         FROM tunnels WHERE guild_id=?1",
    )?;
    statement
        .query_map(params![guild_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect()
}

// ---------------------------------------------------------------------------
// dig_actions
// ---------------------------------------------------------------------------

/// Append one canonical dig action audit row and return its id.
#[allow(clippy::too_many_arguments)]
pub fn insert_dig_action(
    connection: &Connection,
    guild_id: i64,
    actor_id: i64,
    target_id: Option<i64>,
    action_type: &str,
    depth_before: i64,
    depth_after: i64,
    jc_delta: i64,
    detail: &str,
    created_at: i64,
) -> Result<i64, rusqlite::Error> {
    connection.execute(
        "INSERT INTO dig_actions
            (guild_id,actor_id,target_id,action_type,depth_before,depth_after,
             jc_delta,detail,created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            guild_id,
            actor_id,
            target_id,
            action_type,
            depth_before,
            depth_after,
            jc_delta,
            detail,
            created_at,
        ],
    )?;
    Ok(connection.last_insert_rowid())
}

/// Read one dig action's detail column by id.
pub fn dig_action_detail(
    connection: &Connection,
    action_id: i64,
) -> Result<Option<Option<String>>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT detail FROM dig_actions WHERE id=?1",
            params![action_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
}

/// Read one dig action's detail column scoped to the acting player.
pub fn dig_action_detail_for_actor(
    connection: &Connection,
    action_id: i64,
    actor_id: i64,
    guild_id: i64,
) -> Result<Option<Option<String>>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT detail FROM dig_actions WHERE id=?1 AND actor_id=?2 AND guild_id=?3",
            params![action_id, actor_id, guild_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
}

/// True when the identified dig action belongs to the acting player.
pub fn dig_action_exists_for_actor(
    connection: &Connection,
    action_id: i64,
    actor_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM dig_actions
               WHERE id=?1 AND actor_id=?2 AND guild_id=?3",
            params![action_id, actor_id, guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

/// Replace one dig action's detail column by id.
pub fn update_dig_action_detail(
    connection: &Connection,
    detail: &str,
    action_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE dig_actions SET detail=?1 WHERE id=?2",
        params![detail, action_id],
    )
}

/// Replace one dig action's detail column scoped to the acting player.
pub fn update_dig_action_detail_for_actor(
    connection: &Connection,
    detail: &str,
    action_id: i64,
    actor_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE dig_actions SET detail=?1 WHERE id=?2 AND actor_id=?3 AND guild_id=?4",
        params![detail, action_id, actor_id, guild_id],
    )
}

/// Detail payloads of dig actions, oldest first, for delivery recovery.
pub fn dig_action_details_for_delivery(
    connection: &Connection,
    guild_id: Option<i64>,
    actor_id: Option<i64>,
    limit: i64,
) -> Result<Vec<Option<String>>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT detail FROM dig_actions
         WHERE action_type='dig'
           AND (?1 IS NULL OR guild_id=?1)
           AND (?2 IS NULL OR actor_id=?2)
         ORDER BY id ASC LIMIT ?3",
    )?;
    statement
        .query_map(params![guild_id, actor_id, limit], |row| {
            row.get::<_, Option<String>>(0)
        })?
        .collect()
}

// ---------------------------------------------------------------------------
// dig_inventory
// ---------------------------------------------------------------------------

/// Load one player's inventory rows in id order.
pub fn inventory(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeInventoryItem>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id,item_type,queued FROM dig_inventory
         WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeInventoryItem {
                id: row.get(0)?,
                item_type: row.get(1)?,
                queued: row.get::<_, i64>(2)? != 0,
            })
        })?
        .collect()
}

/// True when the identified inventory row still belongs to the player.
pub fn inventory_item_exists(
    connection: &Connection,
    item_id: i64,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM dig_inventory
             WHERE id=?1 AND discord_id=?2 AND guild_id=?3",
            params![item_id, discord_id, guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

/// Upsert the staged inventory list: update known rows, insert new ones.
pub fn sync_inventory(
    connection: &Connection,
    inventory: &[DigRuntimeInventoryItem],
    discord_id: i64,
    guild_id: i64,
    now: i64,
) -> Result<(), rusqlite::Error> {
    for item in inventory {
        let changed = connection.execute(
            "UPDATE dig_inventory SET queued=?1
             WHERE id=?2 AND discord_id=?3 AND guild_id=?4",
            params![i64::from(item.queued), item.id, discord_id, guild_id],
        )?;
        if changed == 0 {
            connection.execute(
                "INSERT INTO dig_inventory
                 (discord_id,guild_id,item_type,queued,created_at)
                 VALUES (?1,?2,?3,?4,?5)",
                params![
                    discord_id,
                    guild_id,
                    item.item_type,
                    i64::from(item.queued),
                    now,
                ],
            )?;
        }
    }
    Ok(())
}

/// Delete one consumed or spilled inventory row.
pub fn delete_inventory_item(
    connection: &Connection,
    item_id: i64,
    discord_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "DELETE FROM dig_inventory WHERE id=?1 AND discord_id=?2 AND guild_id=?3",
        params![item_id, discord_id, guild_id],
    )
}

// ---------------------------------------------------------------------------
// dig_artifacts
// ---------------------------------------------------------------------------

/// Load one player's artifact rows in id order.
pub fn artifacts(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeArtifact>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id,artifact_id,is_relic,equipped FROM dig_artifacts
         WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeArtifact {
                id: row.get(0)?,
                artifact_id: row.get(1)?,
                is_relic: row.get::<_, i64>(2)? != 0,
                equipped: row.get::<_, i64>(3)? != 0,
            })
        })?
        .collect()
}

/// Upsert the staged artifact list: update known rows, insert new ones.
pub fn sync_artifacts(
    connection: &Connection,
    artifacts: &[DigRuntimeArtifact],
    discord_id: i64,
    guild_id: i64,
    now: i64,
) -> Result<(), rusqlite::Error> {
    for artifact in artifacts {
        let changed = connection.execute(
            "UPDATE dig_artifacts SET equipped=?1,artifact_id=?2,is_relic=?3
             WHERE id=?4 AND discord_id=?5 AND guild_id=?6",
            params![
                i64::from(artifact.equipped),
                artifact.artifact_id,
                i64::from(artifact.is_relic),
                artifact.id,
                discord_id,
                guild_id,
            ],
        )?;
        if changed == 0 {
            connection.execute(
                "INSERT INTO dig_artifacts
                 (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    discord_id,
                    guild_id,
                    artifact.artifact_id,
                    now,
                    i64::from(artifact.is_relic),
                    i64::from(artifact.equipped),
                ],
            )?;
        }
    }
    Ok(())
}

/// Move one owned relic copy to another player, unequipped.
pub fn transfer_relic(
    connection: &Connection,
    target_id: i64,
    guild_id: i64,
    now: i64,
    owner_id: i64,
    artifact_id: &str,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE dig_artifacts SET discord_id=?1, guild_id=?2, equipped=0, found_at=?3
         WHERE id = (
             SELECT id FROM dig_artifacts
             WHERE discord_id=?4 AND guild_id=?2 AND artifact_id=?5
               AND is_relic=1
             ORDER BY id LIMIT 1
         )",
        params![target_id, guild_id, now, owner_id, artifact_id],
    )
}

/// Equipped relic ids and artifact identifiers for the info renderer.
pub fn equipped_relic_rows(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<(i64, String)>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id,artifact_id FROM dig_artifacts
         WHERE discord_id=?1 AND guild_id=?2 AND is_relic=1 AND equipped=1
         ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect()
}

/// One player's full persisted artifact rows for the collection read model.
pub fn artifact_collection_rows(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigArtifactCollectionRow>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id,discord_id,guild_id,artifact_id,found_at,is_relic,equipped
         FROM dig_artifacts
         WHERE discord_id=?1 AND guild_id=?2
         ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigArtifactCollectionRow {
                id: row.get(0)?,
                discord_id: row.get(1)?,
                guild_id: row.get(2)?,
                artifact_id: row.get(3)?,
                found_at: row.get(4)?,
                is_relic: row.get::<_, i64>(5)? != 0,
                equipped: row.get::<_, i64>(6)? != 0,
            })
        })?
        .collect()
}

// ---------------------------------------------------------------------------
// dig_gear
// ---------------------------------------------------------------------------

/// Load one player's gear rows in id order.
pub fn gear(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeGear>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id,slot,tier,durability,equipped,acquired_at,source,item_id
         FROM dig_gear WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeGear {
                id: row.get(0)?,
                slot: row.get(1)?,
                tier: row.get(2)?,
                durability: row.get(3)?,
                equipped: row.get::<_, i64>(4)? != 0,
                acquired_at: row.get(5)?,
                source: row.get(6)?,
                item_id: row.get(7)?,
            })
        })?
        .collect()
}

/// Upsert the staged gear list: update known rows, insert new ones.
pub fn sync_gear(
    connection: &Connection,
    gear: &[DigRuntimeGear],
    discord_id: i64,
    guild_id: i64,
    now: i64,
) -> Result<(), rusqlite::Error> {
    for piece in gear {
        let changed = connection.execute(
            "UPDATE dig_gear SET slot=?1,tier=?2,durability=?3,equipped=?4,
                    acquired_at=?5,source=?6,item_id=?7
             WHERE id=?8 AND discord_id=?9 AND guild_id=?10",
            params![
                piece.slot,
                piece.tier,
                piece.durability.max(0),
                i64::from(piece.equipped),
                piece.acquired_at,
                piece.source,
                piece.item_id,
                piece.id,
                discord_id,
                guild_id,
            ],
        )?;
        if changed == 0 {
            connection.execute(
                "INSERT INTO dig_gear(
                     discord_id,guild_id,slot,tier,durability,equipped,
                     acquired_at,source,item_id
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    discord_id,
                    guild_id,
                    piece.slot,
                    piece.tier,
                    piece.durability.max(0),
                    i64::from(piece.equipped),
                    if piece.acquired_at == 0 {
                        now
                    } else {
                        piece.acquired_at
                    },
                    piece.source,
                    piece.item_id,
                ],
            )?;
        }
    }
    Ok(())
}

/// Create and equip the starter weapon when a tunnel has no weapon slot yet.
pub fn insert_starter_weapon(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
    pickaxe_tier: i64,
    now: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "INSERT INTO dig_gear(
             discord_id, guild_id, slot, tier, durability,
             equipped, acquired_at, source, item_id
         )
         SELECT ?1, ?2, 'weapon', ?3, 20, 1, ?4, 'starter', NULL
         WHERE NOT EXISTS (
             SELECT 1 FROM dig_gear
             WHERE discord_id=?1 AND guild_id=?2 AND slot='weapon'
         )",
        params![discord_id, guild_id, pickaxe_tier, now],
    )
}

// ---------------------------------------------------------------------------
// pets
// ---------------------------------------------------------------------------

/// CAS-settle one previewed pet dig work claim.
pub fn claim_pet_dig_work(
    connection: &Connection,
    claim: &PetDigWorkClaim,
    discord_id: i64,
    guild_id: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE pets
            SET dig_work_units=?1, dig_work_at=?2
          WHERE pet_id=?3 AND discord_id=?4 AND guild_id=?5
            AND died_at IS NULL AND is_active=1
            AND dig_work_units=?6 AND dig_work_at=?7",
        params![
            claim.new_units,
            claim.new_at,
            claim.pet_id,
            discord_id,
            guild_id,
            claim.expected_units,
            claim.expected_at,
        ],
    )
}

// ---------------------------------------------------------------------------
// slow_drip_claims
// ---------------------------------------------------------------------------

/// CAS-consume idle Slow Drip minutes against today's claim row.
#[allow(clippy::too_many_arguments)]
pub fn update_slow_drip_claim_cas(
    connection: &Connection,
    gross_jc: i64,
    claimed_at: i64,
    discord_id: i64,
    guild_id: i64,
    claim_date: &str,
    expected_claimed_today: i64,
    expected_last_claim_at: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "UPDATE slow_drip_claims
            SET claimed_today=claimed_today+?1,last_claim_at=?2
          WHERE discord_id=?3 AND guild_id=?4 AND claim_date=?5
            AND claimed_today=?6 AND last_claim_at=?7",
        params![
            gross_jc,
            claimed_at,
            discord_id,
            guild_id,
            claim_date,
            expected_claimed_today,
            expected_last_claim_at,
        ],
    )
}

/// Insert today's Slow Drip claim row; a lost race inserts nothing.
pub fn insert_slow_drip_claim(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
    claim_date: &str,
    claimed_today: i64,
    last_claim_at: i64,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "INSERT INTO slow_drip_claims
             (discord_id,guild_id,claim_date,claimed_today,last_claim_at)
         VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(discord_id,guild_id,claim_date) DO NOTHING",
        params![
            discord_id,
            guild_id,
            claim_date,
            claimed_today,
            last_claim_at
        ],
    )
}

#[cfg(test)]
#[path = "dig_runtime_store/tests.rs"]
mod tests;
