//! Destructive shared-repository smoke test for a disposable dev DB snapshot.

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::path::PathBuf;

use cama_db::{
    audit_database,
    dig_inventory_repository::{BuyItemOutcome, BuyItemRequest, DigInventoryRepository},
    dig_routes_repository::{DigRouteRepository, RouteStateWriteOutcome},
    duel_repository::{DuelChallengeRepository, DuelStatus, DuelTrial},
    economy_event_repository::{EconomyEventRepository, PolicyMode},
    guild_config_repository::GuildConfigRepository,
    loan_repository::LoanRepository,
    low_priority_repository::{LowPriorityRepository, PythonIntegerInput, SetLowPriorityInput},
    package_deal_repository::PackageDealRepository,
    pairings_repository::PairingsRepository,
    pet_brawl_repository::PetBrawlRepository,
    pet_repository::{AdoptPetRequest, BuySuppliesRequest, PetRepository},
    soft_avoid_repository::SoftAvoidRepository,
    tip_repository::TipRepository,
    wheel_spin_repository::{JsonValue, NewWheelSpin, WheelSpinRepository},
};
use cama_domain::guild_config::GuildConfigStore;
use rusqlite::{Connection, params};

const CONFIRMATION: &str = "--disposable-copy";
const SMOKE_GUILD_ID: i64 = -8_888_888_888_888_888_888;
const LOW_PRIORITY_PLAYER_ID: i64 = -8_888_888_888_888_888_887;
const SOFT_AVOID_AVOIDER_ID: i64 = -8_888_888_888_888_888_886;
const SOFT_AVOID_AVOIDED_ID: i64 = -8_888_888_888_888_888_885;
const PACKAGE_DEAL_BUYER_ID: i64 = -8_888_888_888_888_888_884;
const PACKAGE_DEAL_PARTNER_ID: i64 = -8_888_888_888_888_888_883;
const TIP_SENDER_ID: i64 = -8_888_888_888_888_888_882;
const TIP_RECIPIENT_ID: i64 = -8_888_888_888_888_888_881;
const PET_OWNER_ID: i64 = -8_888_888_888_888_888_880;
const PAIRING_PLAYER_1_ID: i64 = -8_888_888_888_888_888_879;
const PAIRING_PLAYER_2_ID: i64 = -8_888_888_888_888_888_878;
const PAIRING_PLAYER_3_ID: i64 = -8_888_888_888_888_888_877;
const PAIRING_MATCH_ID: i64 = -8_888_888_888_888_888_876;
const BRAWL_CHALLENGER_ID: i64 = -8_888_888_888_888_888_875;
const BRAWL_RECIPIENT_ID: i64 = -8_888_888_888_888_888_874;
const DUEL_CHALLENGER_ID: i64 = -8_888_888_888_888_888_873;
const DUEL_RECIPIENT_ID: i64 = -8_888_888_888_888_888_872;
const LOAN_PLAYER_ID: i64 = -8_888_888_888_888_888_871;
const DIG_INVENTORY_PLAYER_ID: i64 = -8_888_888_888_888_888_870;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let path = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: guild_config_snapshot_smoke <db-path> --disposable-copy")?;
    let confirmation = arguments
        .next()
        .and_then(|argument| argument.into_string().ok());
    if confirmation.as_deref() != Some(CONFIRMATION) || arguments.next().is_some() {
        return Err("refusing write smoke without exactly --disposable-copy".into());
    }

    let audit = audit_database(&path)?;
    if !audit.is_compatible() {
        return Err(format!("snapshot is incompatible: {}", audit.issues().join("; ")).into());
    }

    let repository = GuildConfigRepository::new(&path, false);
    if repository.get_config(SMOKE_GUILD_ID)?.is_some() {
        return Err("sentinel guild already exists; use a fresh disposable copy".into());
    }

    repository.set_league_id(SMOKE_GUILD_ID, 777)?;
    repository.set_auto_enrich(SMOKE_GUILD_ID, false)?;
    repository.set_ai_enabled(SMOKE_GUILD_ID, true)?;
    let config = repository
        .get_config(SMOKE_GUILD_ID)?
        .ok_or("Rust write was not readable")?;
    if config.league_id != Some(777)
        || config.auto_enrich_matches
        || config.ai_features_enabled != Some(true)
    {
        return Err(format!("round-trip mismatch: {config:?}").into());
    }

    let economy_event_repository = EconomyEventRepository::new(&path);
    if economy_event_repository
        .get_policy_state(Some(SMOKE_GUILD_ID))?
        .is_some()
    {
        return Err("economy-policy sentinel already exists; use a fresh disposable copy".into());
    }
    let economy_policy = economy_event_repository.ensure_policy_state(
        Some(SMOKE_GUILD_ID),
        PolicyMode::Recovery,
        0.02,
        0.08,
        1_700_000_000,
    )?;
    if economy_policy.guild_id != SMOKE_GUILD_ID
        || economy_policy.mode != PolicyMode::Recovery
        || (economy_policy.target_annual_rate - 0.02).abs() > f64::EPSILON
        || (economy_policy.inflation_ceiling - 0.08).abs() > f64::EPSILON
        || economy_policy.recovery_started_at != Some(1_700_000_000)
    {
        return Err(format!("economy-policy round-trip mismatch: {economy_policy:?}").into());
    }

    let low_priority_repository = LowPriorityRepository::new(&path);
    if low_priority_repository
        .get_state(LOW_PRIORITY_PLAYER_ID, Some(SMOKE_GUILD_ID))?
        .is_some()
    {
        return Err("low-priority sentinel already exists; use a fresh disposable copy".into());
    }
    let mut low_priority = SetLowPriorityInput::new(
        LOW_PRIORITY_PLAYER_ID,
        Some(SMOKE_GUILD_ID),
        LOW_PRIORITY_PLAYER_ID,
        Some("Rust disposable snapshot smoke".to_owned()),
    );
    low_priority.wins_required = PythonIntegerInput::Integer(4);
    low_priority.start_pending_match_id = Some(PythonIntegerInput::Integer(42));
    let low_priority_state = low_priority_repository.set_low_priority(&low_priority)?;
    if low_priority_state.wins_required != 4
        || low_priority_state.wins_remaining != 4
        || low_priority_state.start_pending_match_id != Some(42)
        || !low_priority_state.active
    {
        return Err(format!("low-priority round-trip mismatch: {low_priority_state:?}").into());
    }

    let soft_avoid_repository = SoftAvoidRepository::new(&path);
    if !soft_avoid_repository
        .get_active_avoids_for_players(
            Some(SMOKE_GUILD_ID),
            &[SOFT_AVOID_AVOIDER_ID, SOFT_AVOID_AVOIDED_ID],
        )?
        .is_empty()
    {
        return Err("soft-avoid sentinel already exists; use a fresh disposable copy".into());
    }
    let soft_avoid = soft_avoid_repository.create_or_reactivate_avoid(
        Some(SMOKE_GUILD_ID),
        SOFT_AVOID_AVOIDER_ID,
        SOFT_AVOID_AVOIDED_ID,
        3,
    )?;
    if soft_avoid.games_remaining != 3
        || soft_avoid.guild_id != SMOKE_GUILD_ID
        || soft_avoid.avoider_discord_id != SOFT_AVOID_AVOIDER_ID
        || soft_avoid.avoided_discord_id != SOFT_AVOID_AVOIDED_ID
    {
        return Err(format!("soft-avoid round-trip mismatch: {soft_avoid:?}").into());
    }

    let package_deal_repository = PackageDealRepository::new(&path);
    if !package_deal_repository
        .get_active_deals_for_players(
            Some(SMOKE_GUILD_ID),
            &[PACKAGE_DEAL_BUYER_ID, PACKAGE_DEAL_PARTNER_ID],
        )?
        .is_empty()
    {
        return Err("package-deal sentinel already exists; use a fresh disposable copy".into());
    }
    let package_deal = package_deal_repository.create_or_extend_deal(
        Some(SMOKE_GUILD_ID),
        PACKAGE_DEAL_BUYER_ID,
        PACKAGE_DEAL_PARTNER_ID,
        4,
        70,
    )?;
    if package_deal.guild_id != SMOKE_GUILD_ID
        || package_deal.buyer_discord_id != PACKAGE_DEAL_BUYER_ID
        || package_deal.partner_discord_id != PACKAGE_DEAL_PARTNER_ID
        || package_deal.games_remaining != 4
        || package_deal.cost_paid != 70
    {
        return Err(format!("package-deal round-trip mismatch: {package_deal:?}").into());
    }

    let tip_repository = TipRepository::new(&path);
    let existing_tip_stats =
        tip_repository.get_user_tip_stats(TIP_SENDER_ID, Some(SMOKE_GUILD_ID))?;
    if existing_tip_stats.tips_sent_count != 0 {
        return Err("tip sentinel already exists; use a fresh disposable copy".into());
    }
    tip_repository.log_tip(TIP_SENDER_ID, TIP_RECIPIENT_ID, 12, 2, Some(SMOKE_GUILD_ID))?;
    let tip_stats = tip_repository.get_user_tip_stats(TIP_SENDER_ID, Some(SMOKE_GUILD_ID))?;
    if tip_stats.total_sent != 12
        || tip_stats.tips_sent_count != 1
        || tip_stats.fees_paid != 2
        || tip_stats.total_received != 0
        || tip_stats.tips_received_count != 0
    {
        return Err(format!("tip round-trip mismatch: {tip_stats:?}").into());
    }

    let pairings_repository = PairingsRepository::new(&path);
    if !pairings_repository
        .get_pairings_for_players(
            &[
                PAIRING_PLAYER_1_ID,
                PAIRING_PLAYER_2_ID,
                PAIRING_PLAYER_3_ID,
            ],
            Some(SMOKE_GUILD_ID),
        )?
        .is_empty()
    {
        return Err("pairings sentinel already exists; use a fresh disposable copy".into());
    }
    pairings_repository.update_pairings_for_match(
        PAIRING_MATCH_ID,
        Some(SMOKE_GUILD_ID),
        &[PAIRING_PLAYER_1_ID, PAIRING_PLAYER_2_ID],
        &[PAIRING_PLAYER_3_ID],
        1,
    )?;
    let pairings = pairings_repository.get_pairings_for_players(
        &[
            PAIRING_PLAYER_1_ID,
            PAIRING_PLAYER_2_ID,
            PAIRING_PLAYER_3_ID,
        ],
        Some(SMOKE_GUILD_ID),
    )?;
    let teammate_pairing = pairings
        .get(&(PAIRING_PLAYER_1_ID, PAIRING_PLAYER_2_ID))
        .ok_or("teammate pairing was not readable")?;
    let opponent_pairing = pairings
        .get(&(PAIRING_PLAYER_1_ID, PAIRING_PLAYER_3_ID))
        .ok_or("opponent pairing was not readable")?;
    if teammate_pairing.games_together != 1
        || teammate_pairing.wins_together != 1
        || teammate_pairing.last_match_id != Some(PAIRING_MATCH_ID)
        || opponent_pairing.games_against != 1
        || opponent_pairing.player1_wins_against != 1
        || opponent_pairing.last_match_id != Some(PAIRING_MATCH_ID)
    {
        return Err(format!(
            "pairings round-trip mismatch: teammate={teammate_pairing:?}, opponent={opponent_pairing:?}"
        )
        .into());
    }

    let seed_connection = Connection::open(&path)?;
    seed_connection.pragma_update(None, "foreign_keys", false)?;
    for (discord_id, username, balance, rating) in [
        (PET_OWNER_ID, "Rust pet snapshot smoke", 1_000, None),
        (BRAWL_CHALLENGER_ID, "Rust brawl alpha", 100, None),
        (BRAWL_RECIPIENT_ID, "Rust brawl beta", 100, None),
        (
            DUEL_CHALLENGER_ID,
            "Rust duel challenger",
            1_000,
            Some(1_400.0),
        ),
        (
            DUEL_RECIPIENT_ID,
            "Rust duel recipient",
            1_000,
            Some(1_500.0),
        ),
        (LOAN_PLAYER_ID, "Rust loan borrower", 50, None),
        (DIG_INVENTORY_PLAYER_ID, "Rust inventory miner", 100, None),
    ] {
        if seed_connection.query_row(
            "SELECT COUNT(*) FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, SMOKE_GUILD_ID],
            |row| row.get::<_, i64>(0),
        )? != 0
        {
            return Err("player sentinel already exists; use a fresh disposable copy".into());
        }
        seed_connection.execute(
            "INSERT INTO players (
                 discord_id, guild_id, discord_username, jopacoin_balance,
                 glicko_rating, glicko_rd, glicko_volatility
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                discord_id,
                SMOKE_GUILD_ID,
                username,
                balance,
                rating,
                rating.map(|_| 80.0),
                rating.map(|_| 0.06)
            ],
        )?;
    }
    seed_connection.execute(
        "INSERT INTO tunnels (discord_id, guild_id, depth) VALUES (?1, ?2, 100)",
        params![DIG_INVENTORY_PLAYER_ID, SMOKE_GUILD_ID],
    )?;
    drop(seed_connection);

    let loan_repository = LoanRepository::new(&path);
    let loan = loan_repository.execute_loan_atomic_at(
        LOAN_PLAYER_ID,
        Some(SMOKE_GUILD_ID),
        100,
        10,
        86_400,
        200,
        1_800_000_000,
    )?;
    if loan.new_balance != 150 || loan.total_owed != 110 || loan.total_loans_taken != 1 {
        return Err(format!("loan issuance round-trip mismatch: {loan:?}").into());
    }
    let repayment =
        loan_repository.execute_repayment_atomic(LOAN_PLAYER_ID, Some(SMOKE_GUILD_ID))?;
    if repayment.principal != 100
        || repayment.fee != 10
        || repayment.new_balance != 40
        || repayment.nonprofit_total != 10
    {
        return Err(format!("loan repayment round-trip mismatch: {repayment:?}").into());
    }

    let dig_inventory_repository = DigInventoryRepository::new(&path);
    let inventory_purchase = dig_inventory_repository.buy_item_atomic(BuyItemRequest {
        discord_id: DIG_INVENTORY_PLAYER_ID,
        guild_id: Some(SMOKE_GUILD_ID),
        item_type: "hard_hat",
        price: 20,
        inventory_limit: 10,
        auto_queue_if_available: true,
        created_at: 1_800_000_000,
    })?;
    let BuyItemOutcome::Purchased {
        queued: true,
        cost: 20,
        balance_after: 80,
        ..
    } = inventory_purchase
    else {
        return Err(format!("dig-inventory purchase mismatch: {inventory_purchase:?}").into());
    };
    let inventory =
        dig_inventory_repository.inventory(DIG_INVENTORY_PLAYER_ID, Some(SMOKE_GUILD_ID))?;
    if inventory.len() != 1 || inventory[0].item_type != "hard_hat" || !inventory[0].queued {
        return Err(format!("dig-inventory round-trip mismatch: {inventory:?}").into());
    }

    let dig_route_repository = DigRouteRepository::new(&path);
    let route_state = r#"{"status":"active","route_id":"shored_passage"}"#;
    if dig_route_repository.set_route_state(
        DIG_INVENTORY_PLAYER_ID,
        Some(SMOKE_GUILD_ID),
        Some(route_state),
    )? != RouteStateWriteOutcome::Updated
    {
        return Err("dig-route sentinel tunnel was not updated".into());
    }
    let route_tunnel = dig_route_repository
        .tunnel(DIG_INVENTORY_PLAYER_ID, Some(SMOKE_GUILD_ID))?
        .ok_or("dig-route sentinel tunnel disappeared")?;
    if route_tunnel.route_state.as_deref() != Some(route_state) {
        return Err(format!("dig-route round-trip mismatch: {route_tunnel:?}").into());
    }

    let pet_repository = PetRepository::new(&path);
    let pet = pet_repository.adopt_pet(&AdoptPetRequest {
        discord_id: PET_OWNER_ID,
        guild_id: Some(SMOKE_GUILD_ID),
        name: "Snapshot Blep",
        egg_tier: "standard",
        fee: 20,
        now: 1_800_000_000,
        hatch_seconds: 86_400,
    })?;
    if pet.discord_id != PET_OWNER_ID
        || pet.guild_id != SMOKE_GUILD_ID
        || pet.name != "Snapshot Blep"
        || pet.species != "unhatched"
        || pet.egg_tier != "standard"
        || pet.hatched_at != 1_800_086_400
    {
        return Err(format!("pet adoption round-trip mismatch: {pet:?}").into());
    }
    let quantity = pet_repository.buy_supplies(&BuySuppliesRequest {
        discord_id: PET_OWNER_ID,
        guild_id: Some(SMOKE_GUILD_ID),
        item_id: "tango",
        qty: 3,
        total_cost: 15,
        now: 1_800_000_001,
        stack_cap: 99,
    })?;
    let supplies = pet_repository.get_supplies(PET_OWNER_ID, Some(SMOKE_GUILD_ID))?;
    if quantity != 3 || supplies.get("tango") != Some(&3) {
        return Err(format!("pet supplies round-trip mismatch: {supplies:?}").into());
    }

    let wheel_spin_repository = WheelSpinRepository::new(&path);
    let wheel_spin_id = wheel_spin_repository.log_wheel_spin(&NewWheelSpin {
        discord_id: PET_OWNER_ID,
        guild_id: Some(SMOKE_GUILD_ID),
        result: 0,
        spin_time: 1_800_000_002,
        is_bankrupt: false,
        is_golden: false,
        outcome_code: Some("LIGHTNING_BOLT".to_owned()),
        is_bonus: true,
        event_id: Some("rust-disposable-wheel-smoke".to_owned()),
        outcome_metadata: Some(JsonValue::Object(BTreeMap::from([
            ("lightning_count".to_owned(), JsonValue::Integer(2)),
            ("lightning_total".to_owned(), JsonValue::Integer(40)),
        ]))),
    })?;
    let wheel_spin = wheel_spin_repository
        .get_wheel_spin(wheel_spin_id)?
        .ok_or("wheel spin was not readable")?;
    if wheel_spin.guild_id != SMOKE_GUILD_ID
        || wheel_spin.discord_id != PET_OWNER_ID
        || wheel_spin.outcome_code.as_deref() != Some("LIGHTNING_BOLT")
        || !wheel_spin.is_bonus
        || wheel_spin.event_id.as_deref() != Some("rust-disposable-wheel-smoke")
        || wheel_spin.outcome_metadata.as_deref()
            != Some("{\"lightning_count\":2,\"lightning_total\":40}")
    {
        return Err(format!("wheel-spin round-trip mismatch: {wheel_spin:?}").into());
    }

    let mut brawl_pets = Vec::new();
    for (discord_id, name) in [
        (BRAWL_CHALLENGER_ID, "Snapshot Alpha"),
        (BRAWL_RECIPIENT_ID, "Snapshot Beta"),
    ] {
        let egg = pet_repository.adopt_pet(&AdoptPetRequest {
            discord_id,
            guild_id: Some(SMOKE_GUILD_ID),
            name,
            egg_tier: "standard",
            fee: 20,
            now: 1_800_000_000,
            hatch_seconds: 86_400,
        })?;
        brawl_pets.push(pet_repository.resolve_hatch_species(
            egg.pet_id,
            Some(SMOKE_GUILD_ID),
            "common_cama",
            egg.hatched_at,
        )?);
    }
    let pet_brawl_repository = PetBrawlRepository::new(&path);
    let brawl = pet_brawl_repository.create_brawl_atomic(
        Some(SMOKE_GUILD_ID),
        222,
        BRAWL_CHALLENGER_ID,
        BRAWL_RECIPIENT_ID,
        brawl_pets[0].pet_id,
        1_800_086_401,
        1_800_086_581,
        0,
        0,
    )?;
    let brawl = pet_brawl_repository.accept_atomic(
        brawl.brawl_id,
        Some(SMOKE_GUILD_ID),
        BRAWL_RECIPIENT_ID,
        brawl_pets[1].pet_id,
        1_800_086_402,
    )?;
    if brawl.status != "active"
        || brawl.challenger_pet_id != brawl_pets[0].pet_id
        || brawl.recipient_pet_id != Some(brawl_pets[1].pet_id)
    {
        return Err(format!("pet-brawl round-trip mismatch: {brawl:?}").into());
    }

    let duel_repository = DuelChallengeRepository::new(&path);
    let duel = duel_repository.create_challenge_atomic(
        Some(SMOKE_GUILD_ID),
        333,
        DUEL_CHALLENGER_ID,
        DUEL_RECIPIENT_ID,
        500,
        1_900_000_000,
        30 * 86_400,
        7 * 86_400,
        7 * 86_400,
        DUEL_CHALLENGER_ID,
    )?;
    let duel = duel_repository.bind_message(duel.challenge_id, Some(SMOKE_GUILD_ID), 5_555)?;
    let duel = duel_repository.accept_atomic(
        duel.challenge_id,
        Some(SMOKE_GUILD_ID),
        DUEL_RECIPIENT_ID,
        DuelTrial::TrialByCombat,
        1_900_000_100,
        DUEL_RECIPIENT_ID,
    )?;
    if duel.status != DuelStatus::Accepted
        || duel.message_id != Some(5_555)
        || duel.trial_type != Some(DuelTrial::TrialByCombat)
    {
        return Err(format!("duel round-trip mismatch: {duel:?}").into());
    }

    println!(
        "shared_repository_roundtrip=ok guild_id={} guild_config=ok economy_event=ok low_priority=ok soft_avoid=ok package_deal=ok tip=ok pairings=ok loan=ok dig_inventory=ok dig_routes=ok pet=ok wheel_spin=ok pet_brawl=ok duel=ok",
        SMOKE_GUILD_ID
    );
    Ok(())
}
