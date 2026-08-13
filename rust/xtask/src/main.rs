//! Repository automation for cross-language parity accounting.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output, Stdio};

#[cfg(test)]
mod architecture_tests;

const PYTHON_MIGRATIONS_SCRIPT: &str = "from infrastructure.schema_manager import SchemaManager; print('\\n'.join(name for name, _ in SchemaManager(':memory:')._get_migrations()))";

const REQUIRED_CUTOVER_GATES: &[&str] = &[
    "backup_rollback_rehearsal",
    "container_health_smoke",
    "database_migration_and_interop",
    "discord_gateway_and_commands",
    "external_service_recordings",
    "image_and_attachment_equivalence",
    "observability_and_post_deploy",
    "persistent_views_and_reconnect",
    "production_snapshot_replay",
    "python_rollback_compatibility",
    "runtime_selector_and_same_deploy",
    "scheduled_workers_and_shutdown",
    "single_writer_and_process_lock",
    "sqlite_repository_coverage",
];

struct ScriptedBrawlRng {
    values: Vec<i32>,
    cursor: usize,
}

impl cama_domain::pet_brawl::BrawlRng for ScriptedBrawlRng {
    fn randint(&mut self, low: i32, high: i32) -> i32 {
        let value = self
            .values
            .get(self.cursor)
            .copied()
            .expect("pet-brawl differential vector exhausted its scripted RNG");
        self.cursor += 1;
        assert!(
            (low..=high).contains(&value),
            "pet-brawl differential RNG value {value} outside [{low}, {high}]"
        );
        value
    }
}

fn main() -> ExitCode {
    match run(env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("parity gate failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    match args.next().as_deref() {
        Some("parity") => {
            let require_complete = match args.next().as_deref() {
                Some("--require-complete") => true,
                Some(argument) => return Err(format!("unknown parity argument {argument:?}")),
                None => false,
            };
            if let Some(argument) = args.next() {
                return Err(format!("unexpected parity argument {argument:?}"));
            }
            parity(require_complete)
        }
        Some(command) => Err(format!("unknown xtask command {command:?}")),
        None => Err("usage: cargo run -p xtask -- parity [--require-complete]".to_owned()),
    }
}

fn parity(require_complete: bool) -> Result<(), String> {
    let root = repository_root()?;
    let runtime_required = cama_runtime::inventory::required_count();
    let runtime_wired = cama_runtime::inventory::wired_count();
    verify_schema_contract(&root)?;
    let vector_count = verify_domain_vectors(&root)?;
    verify_repository_interoperability(&root)?;
    let readiness = read_cutover_readiness(&root.join("rust/parity/cutover_readiness.tsv"))?;

    let python_output = checked_output(
        parity_python(&root).args(["-m", "pytest", "-n", "0", "--collect-only", "-q"]),
        "collect Python tests",
    )?;
    let python_stdout = output_text(python_output, "pytest collection")?;
    let python_tests: BTreeSet<String> = python_stdout
        .lines()
        .filter(|line| line.starts_with("tests/") && line.contains("::"))
        .map(str::to_owned)
        .collect();
    if python_tests.is_empty() {
        return Err("pytest collection returned no test node IDs".to_owned());
    }

    let baseline = read_baseline(&root.join("rust/parity/baseline.txt"))?;
    let actual_fingerprint = fnv1a64_lines(python_tests.iter().map(String::as_str));
    if baseline.collected_cases != python_tests.len()
        || baseline.node_id_fingerprint != actual_fingerprint
    {
        return Err(format!(
            "Python test inventory changed: expected {} cases/{:016x}, found {} cases/{:016x}; review the new behavior and intentionally update rust/parity/baseline.txt",
            baseline.collected_cases,
            baseline.node_id_fingerprint,
            python_tests.len(),
            actual_fingerprint,
        ));
    }

    let mappings = read_mappings(&root.join("rust/parity/tests.tsv"))?;
    let mapped_python: BTreeSet<&str> = mappings
        .iter()
        .map(|mapping| mapping.python_test.as_str())
        .collect();
    if mapped_python.len() != mappings.len() {
        return Err("rust/parity/tests.tsv contains duplicate Python node IDs".to_owned());
    }
    let mapped_rust: BTreeSet<&str> = mappings
        .iter()
        .map(|mapping| mapping.rust_test.as_str())
        .collect();
    if mapped_rust.len() != mappings.len() {
        return Err(
            "rust/parity/tests.tsv must map each Python case to a distinct Rust test ID".to_owned(),
        );
    }
    let unknown_python: Vec<_> = mapped_python
        .difference(&python_tests.iter().map(String::as_str).collect())
        .copied()
        .collect();
    if !unknown_python.is_empty() {
        return Err(format!(
            "parity mappings reference unknown Python tests: {}",
            unknown_python.join(", ")
        ));
    }

    let rust_output = checked_output(
        Command::new("cargo").current_dir(&root).args([
            "test",
            "--locked",
            "--manifest-path",
            "rust/Cargo.toml",
            "--workspace",
            "--all-targets",
            "--",
            "--list",
        ]),
        "list Rust tests",
    )?;
    let rust_stdout = output_text(rust_output, "cargo test -- --list")?;
    let rust_test_list: Vec<&str> = rust_stdout
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .collect();
    let mut rust_test_counts = BTreeMap::new();
    for test in &rust_test_list {
        *rust_test_counts.entry(*test).or_insert(0_usize) += 1;
    }
    let duplicate_rust_tests: Vec<_> = rust_test_counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(test, count)| format!("{test} ({count} copies)"))
        .collect();
    if !duplicate_rust_tests.is_empty() {
        return Err(format!(
            "Rust test IDs must be unique across the workspace: {}",
            duplicate_rust_tests.join(", ")
        ));
    }
    let rust_tests: BTreeSet<&str> = rust_test_list.into_iter().collect();
    let unknown_rust: Vec<_> = mappings
        .iter()
        .filter(|mapping| !rust_tests.contains(mapping.rust_test.as_str()))
        .map(|mapping| mapping.rust_test.as_str())
        .collect();
    if !unknown_rust.is_empty() {
        return Err(format!(
            "parity mappings reference unknown Rust tests: {}",
            unknown_rust.join(", ")
        ));
    }

    let mapped = mappings.len();
    let total = python_tests.len();
    let percentage = (mapped as f64 / total as f64) * 100.0;
    println!(
        "Rust parity inventory: {mapped}/{total} Python cases mapped to distinct existing Rust tests ({percentage:.2}%)"
    );
    println!(
        "Differential contracts: {vector_count} domain vectors plus one thirteen-repository serial Python→Rust→Python SQLite bridge"
    );
    let open_readiness: Vec<_> = readiness
        .iter()
        .filter(|gate| gate.status == ReadinessStatus::Open)
        .collect();
    println!(
        "Cutover readiness: {}/{} operational gates complete ({} open)",
        readiness.len() - open_readiness.len(),
        readiness.len(),
        open_readiness.len(),
    );
    println!("Production runtime inventory: {runtime_wired} wired, {runtime_required} required");
    if mapped != total {
        let summary = unmapped_module_counts(&python_tests, &mapped_python)
            .into_iter()
            .take(10)
            .map(|(module, count)| format!("{module} ({count})"))
            .collect::<Vec<_>>()
            .join(", ");
        println!("Largest remaining Python modules: {summary}");
    }
    if require_complete && mapped != total {
        return Err(format!(
            "cutover requires complete parity; {} cases remain unmapped",
            total - mapped
        ));
    }
    if require_complete && !open_readiness.is_empty() {
        return Err(format!(
            "cutover requires all operational readiness gates; {} remain open: {}",
            open_readiness.len(),
            open_readiness
                .iter()
                .map(|gate| gate.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if require_complete && runtime_required != 0 {
        return Err(format!(
            "cutover requires every production runtime capability to be wired; {runtime_required} inventory items remain"
        ));
    }
    Ok(())
}

fn unmapped_module_counts(
    python_tests: &BTreeSet<String>,
    mapped_python: &BTreeSet<&str>,
) -> Vec<(String, usize)> {
    let mut counts = BTreeMap::new();
    for test in python_tests {
        if mapped_python.contains(test.as_str()) {
            continue;
        }
        let module = test.split("::").next().unwrap_or(test).to_owned();
        *counts.entry(module).or_insert(0_usize) += 1;
    }
    let mut counts = counts.into_iter().collect::<Vec<_>>();
    counts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    counts
}

fn verify_repository_interoperability(root: &Path) -> Result<(), String> {
    use cama_db::core_repositories::PlayerRepository as CorePlayerRepository;
    use cama_db::duel_repository::{DuelChallengeRepository, DuelStatus, DuelTrial};
    use cama_db::economy_event_repository::{EconomyEventRepository, PolicyMode};
    use cama_db::loan_repository::LoanRepository;
    use cama_db::low_priority_repository::{
        LowPriorityRepository, PythonIntegerInput, SetLowPriorityInput,
    };
    use cama_db::package_deal_repository::PackageDealRepository;
    use cama_db::pairings_repository::PairingsRepository;
    use cama_db::pet_brawl_repository::PetBrawlRepository;
    use cama_db::pet_repository::{BuySuppliesRequest, PetRepository};
    use cama_db::soft_avoid_repository::SoftAvoidRepository;
    use cama_db::tip_repository::TipRepository;
    use cama_db::wheel_spin_repository::{JsonValue, NewWheelSpin, WheelSpinRepository};
    use cama_domain::guild_config::GuildConfigStore;

    let temporary_directory =
        tempfile::tempdir().map_err(|error| format!("cannot create interop temp dir: {error}"))?;
    let database_path = temporary_directory.path().join("python-rust-interop.db");
    let python_seed = run_python_db_bridge(root, "seed", &database_path)?;
    if python_seed
        != "424242\t777\tfalse\ttrue\t515151\t5\t5\t41\ttrue\tpython seed\t901\t424242\t616161\t626262\t3\t424242\t717171\t727272\t4\t70\t12\t1\t2\t0\t0\t1\t1\t1\t1\t901\t1010101\tInterop Blep\tunhatched\tstandard\t1800000000\t1800086400\t980\t0\t3\t1\tpending\t2\tnone\t1\tpending\tnone\tnone\t450\t1000\t1\t25\tCROWN\t0\tpython-wheel-1\t{\"a\":{\"b\":true},\"z\":2}\t25\tCROWN\t0\tpython-wheel-1\t{\"a\":{\"b\":true},\"z\":2}\t424242\tnormal\t0.02\t0.08\tnone\t1700000000\t1060606\t434343\t1\t0\t0\t100\t10\t150\t0\tloan\t\tnone\tnone\tnone"
    {
        return Err(format!(
            "Python seeded unexpected repository values: {python_seed:?}"
        ));
    }

    let audit = cama_db::audit_database(&database_path)
        .map_err(|error| format!("cannot audit Python-created interop DB: {error}"))?;
    if !audit.is_compatible() {
        return Err(format!(
            "Python-created interop DB failed Rust audit: {}",
            audit.issues().join("; ")
        ));
    }

    let repository =
        cama_db::guild_config_repository::GuildConfigRepository::new(&database_path, false);
    let config = repository
        .get_config(424_242)
        .map_err(|error| format!("Rust cannot read Python guild_config row: {error}"))?
        .ok_or_else(|| "Rust did not find Python guild_config row".to_owned())?;
    if config.league_id != Some(777)
        || config.auto_enrich_matches
        || config.ai_features_enabled != Some(true)
    {
        return Err(format!("Rust decoded Python row incorrectly: {config:?}"));
    }

    let low_priority_repository = LowPriorityRepository::new(&database_path);
    let low_priority = low_priority_repository
        .get_state(515_151, Some(424_242))
        .map_err(|error| format!("Rust cannot read Python low_priority row: {error}"))?
        .ok_or_else(|| "Rust did not find Python low_priority row".to_owned())?;
    if low_priority.wins_required != 5
        || low_priority.wins_remaining != 5
        || low_priority.start_pending_match_id != Some(41)
        || !low_priority.active
        || low_priority.reason.as_deref() != Some("python seed")
        || low_priority.set_by != 901
    {
        return Err(format!(
            "Rust decoded Python low_priority row incorrectly: {low_priority:?}"
        ));
    }

    let soft_avoid_repository = SoftAvoidRepository::new(&database_path);
    let soft_avoids = soft_avoid_repository
        .get_active_avoids_for_players(Some(424_242), &[616_161, 626_262])
        .map_err(|error| format!("Rust cannot read Python soft_avoid row: {error}"))?;
    let [soft_avoid] = soft_avoids.as_slice() else {
        return Err(format!(
            "Rust expected one Python soft_avoid row, found {soft_avoids:?}"
        ));
    };
    if soft_avoid.guild_id != 424_242
        || soft_avoid.avoider_discord_id != 616_161
        || soft_avoid.avoided_discord_id != 626_262
        || soft_avoid.games_remaining != 3
    {
        return Err(format!(
            "Rust decoded Python soft_avoid row incorrectly: {soft_avoid:?}"
        ));
    }
    let soft_avoid_id = soft_avoid.id;

    let package_deal_repository = PackageDealRepository::new(&database_path);
    let package_deals = package_deal_repository
        .get_active_deals_for_players(Some(424_242), &[717_171, 727_272])
        .map_err(|error| format!("Rust cannot read Python package-deal row: {error}"))?;
    let [package_deal] = package_deals.as_slice() else {
        return Err(format!(
            "Rust expected one Python package-deal row, found {package_deals:?}"
        ));
    };
    if package_deal.guild_id != 424_242
        || package_deal.buyer_discord_id != 717_171
        || package_deal.partner_discord_id != 727_272
        || package_deal.games_remaining != 4
        || package_deal.cost_paid != 70
    {
        return Err(format!(
            "Rust decoded Python package-deal row incorrectly: {package_deal:?}"
        ));
    }
    let package_deal_id = package_deal.id;

    let tip_repository = TipRepository::new(&database_path);
    let tip_stats = tip_repository
        .get_user_tip_stats(818_181, Some(424_242))
        .map_err(|error| format!("Rust cannot read Python tip rows: {error}"))?;
    if tip_stats.total_sent != 12
        || tip_stats.tips_sent_count != 1
        || tip_stats.fees_paid != 2
        || tip_stats.total_received != 0
        || tip_stats.tips_received_count != 0
    {
        return Err(format!(
            "Rust decoded Python tip aggregates incorrectly: {tip_stats:?}"
        ));
    }

    let pairings_repository = PairingsRepository::new(&database_path);
    let pairings = pairings_repository
        .get_pairings_for_players(&[919_191, 929_292, 939_393], Some(424_242))
        .map_err(|error| format!("Rust cannot read Python pairing rows: {error}"))?;
    let teammate_pairing = pairings
        .get(&(919_191, 929_292))
        .ok_or_else(|| "Rust did not find Python teammate pairing".to_owned())?;
    let opponent_pairing = pairings
        .get(&(919_191, 939_393))
        .ok_or_else(|| "Rust did not find Python opponent pairing".to_owned())?;
    if teammate_pairing.games_together != 1
        || teammate_pairing.wins_together != 1
        || teammate_pairing.last_match_id != Some(901)
        || opponent_pairing.games_against != 1
        || opponent_pairing.player1_wins_against != 1
        || opponent_pairing.last_match_id != Some(901)
    {
        return Err(format!(
            "Rust decoded Python pairings incorrectly: teammate={teammate_pairing:?}, opponent={opponent_pairing:?}"
        ));
    }

    let pet_repository = PetRepository::new(&database_path);
    let pet = pet_repository
        .get_active_pet(1_010_101, Some(424_242))
        .map_err(|error| format!("Rust cannot read Python pet row: {error}"))?
        .ok_or_else(|| "Rust did not find Python pet row".to_owned())?;
    if pet.discord_id != 1_010_101
        || pet.guild_id != 424_242
        || pet.name != "Interop Blep"
        || pet.species != "unhatched"
        || pet.egg_tier != "standard"
        || pet.adopted_at != 1_800_000_000
        || pet.hatched_at != 1_800_086_400
    {
        return Err(format!("Rust decoded Python pet row incorrectly: {pet:?}"));
    }
    let supplies = pet_repository
        .get_supplies(1_010_101, Some(424_242))
        .map_err(|error| format!("Rust cannot read Python pet supplies: {error}"))?;
    if !supplies.is_empty() {
        return Err(format!(
            "Python seed unexpectedly created pet supplies: {supplies:?}"
        ));
    }

    let pet_brawl_repository = PetBrawlRepository::new(&database_path);
    let brawl = pet_brawl_repository
        .get_brawl(1, Some(424_242))
        .map_err(|error| format!("Rust cannot read Python pet-brawl row: {error}"))?
        .ok_or_else(|| "Rust did not find Python pet-brawl row".to_owned())?;
    if brawl.status != "pending"
        || brawl.challenger_id != 1_020_202
        || brawl.recipient_id != 1_030_303
        || brawl.challenger_pet_id != 2
        || brawl.recipient_pet_id.is_some()
    {
        return Err(format!(
            "Rust decoded Python pet-brawl row incorrectly: {brawl:?}"
        ));
    }

    let duel_repository = DuelChallengeRepository::new(&database_path);
    let duel = duel_repository
        .get_challenge(1, Some(424_242))
        .map_err(|error| format!("Rust cannot read Python duel row: {error}"))?
        .ok_or_else(|| "Rust did not find Python duel row".to_owned())?;
    if duel.status != DuelStatus::Pending
        || duel.challenger_id != 1_040_404
        || duel.recipient_id != 1_050_505
        || duel.wager != 500
        || duel.issuance_fee != 50
        || duel.message_id.is_some()
    {
        return Err(format!(
            "Rust decoded Python duel row incorrectly: {duel:?}"
        ));
    }

    let wheel_spin_repository = WheelSpinRepository::new(&database_path);
    let wheel_spin = wheel_spin_repository
        .get_wheel_spin(1)
        .map_err(|error| format!("Rust cannot read Python wheel-spin row: {error}"))?
        .ok_or_else(|| "Rust did not find Python wheel-spin row".to_owned())?;
    if wheel_spin.guild_id != 424_242
        || wheel_spin.discord_id != 1_010_101
        || wheel_spin.result != 25
        || wheel_spin.outcome_code.as_deref() != Some("CROWN")
        || wheel_spin.is_bonus
        || wheel_spin.event_id.as_deref() != Some("python-wheel-1")
        || wheel_spin.outcome_metadata.as_deref() != Some("{\"a\":{\"b\":true},\"z\":2}")
    {
        return Err(format!(
            "Rust decoded Python wheel-spin row incorrectly: {wheel_spin:?}"
        ));
    }

    let economy_event_repository = EconomyEventRepository::new(&database_path);
    let economy_policy = economy_event_repository
        .get_policy_state(Some(424_242))
        .map_err(|error| format!("Rust cannot read Python economy policy: {error}"))?
        .ok_or_else(|| "Rust did not find Python economy policy".to_owned())?;
    if economy_policy.mode != PolicyMode::Normal
        || (economy_policy.target_annual_rate - 0.02).abs() > f64::EPSILON
        || (economy_policy.inflation_ceiling - 0.08).abs() > f64::EPSILON
        || economy_policy.recovery_started_at.is_some()
        || economy_policy.updated_at != 1_700_000_000
    {
        return Err(format!(
            "Rust decoded Python economy policy incorrectly: {economy_policy:?}"
        ));
    }

    let loan_repository = LoanRepository::new(&database_path);
    let loan_state = loan_repository
        .get_state(1_060_606, Some(434_343))
        .map_err(|error| format!("Rust cannot read Python loan state: {error}"))?
        .ok_or_else(|| "Rust did not find Python loan state".to_owned())?;
    let loan_balance = loan_repository
        .get_player_balance(1_060_606, Some(434_343))
        .map_err(|error| format!("Rust cannot read Python loan balance: {error}"))?;
    let loan_fund = loan_repository
        .get_nonprofit_fund(Some(434_343))
        .map_err(|error| format!("Rust cannot read Python loan reserve: {error}"))?;
    if loan_state.discord_id != 1_060_606
        || loan_state.guild_id != 434_343
        || loan_state.total_loans_taken != 1
        || loan_state.total_fees_paid != 0
        || loan_state.negative_loans_taken != 0
        || loan_state.outstanding_principal != 100
        || loan_state.outstanding_fee != 10
        || loan_balance != Some(150)
        || loan_fund != 0
    {
        return Err(format!(
            "Rust decoded Python loan state incorrectly: state={loan_state:?}, balance={loan_balance:?}, fund={loan_fund}"
        ));
    }

    let core_player_repository = CorePlayerRepository::new(&database_path);
    let core_player = core_player_repository
        .get_by_id(1_010_101, Some(424_242))
        .map_err(|error| format!("Rust core repository cannot read Python player: {error}"))?
        .ok_or_else(|| "Rust core repository did not find Python player".to_owned())?;
    if core_player.name != "Pet interop"
        || core_player.discord_id != Some(1_010_101)
        || core_player.guild_id != Some(424_242)
        || core_player.jopacoin_balance != 980
        || core_player.preferred_roles.is_some()
        || core_player.glicko_rating.is_some()
    {
        return Err(format!(
            "Rust core repository decoded Python player incorrectly: {core_player:?}"
        ));
    }

    repository
        .set_league_id(424_242, 888)
        .and_then(|()| repository.set_auto_enrich(424_242, true))
        .and_then(|()| repository.set_ai_enabled(424_242, false))
        .map_err(|error| format!("Rust cannot update Python guild_config row: {error}"))?;

    if !low_priority_repository
        .clear_low_priority(
            515_151,
            Some(424_242),
            903,
            Some("Rust interop replacement"),
        )
        .map_err(|error| format!("Rust cannot clear Python low_priority row: {error}"))?
    {
        return Err("Rust did not clear the Python low_priority row".to_owned());
    }
    let mut replacement = SetLowPriorityInput::new(
        515_151,
        Some(424_242),
        904,
        Some("rust replacement".to_owned()),
    );
    replacement.wins_required = PythonIntegerInput::Integer(7);
    replacement.start_pending_match_id = Some(PythonIntegerInput::Integer(84));
    low_priority_repository
        .set_low_priority(&replacement)
        .map_err(|error| format!("Rust cannot replace Python low_priority row: {error}"))?;
    let decremented = soft_avoid_repository
        .decrement_avoids(Some(424_242), &[soft_avoid_id])
        .map_err(|error| format!("Rust cannot decrement Python soft_avoid row: {error}"))?;
    if decremented != 1 {
        return Err(format!(
            "Rust expected to decrement one soft_avoid row, changed {decremented}"
        ));
    }
    let package_deals_decremented = package_deal_repository
        .decrement_deals(Some(424_242), &[package_deal_id])
        .map_err(|error| format!("Rust cannot decrement Python package-deal row: {error}"))?;
    if package_deals_decremented != 1 {
        return Err(format!(
            "Rust expected to decrement one package-deal row, changed {package_deals_decremented}"
        ));
    }
    tip_repository
        .log_tip(818_181, 828_282, 18, 3, Some(424_242))
        .map_err(|error| format!("Rust cannot append to Python tip history: {error}"))?;
    pairings_repository
        .update_pairings_for_match(902, Some(424_242), &[919_191, 929_292], &[939_393], 2)
        .map_err(|error| format!("Rust cannot update Python pairings: {error}"))?;
    let quantity = pet_repository
        .buy_supplies(&BuySuppliesRequest {
            discord_id: 1_010_101,
            guild_id: Some(424_242),
            item_id: "tango",
            qty: 3,
            total_cost: 15,
            now: 1_800_000_001,
            stack_cap: 99,
        })
        .map_err(|error| format!("Rust cannot buy supplies for Python pet: {error}"))?;
    if quantity != 3 {
        return Err(format!(
            "Rust expected pet supply quantity 3, found {quantity}"
        ));
    }
    pet_brawl_repository
        .accept_atomic(1, Some(424_242), 1_030_303, 3, 1_800_086_402)
        .map_err(|error| format!("Rust cannot accept Python pet brawl: {error}"))?;
    duel_repository
        .bind_message(1, Some(424_242), 5_555)
        .and_then(|_| {
            duel_repository.accept_atomic(
                1,
                Some(424_242),
                1_050_505,
                DuelTrial::TrialByCombat,
                1_900_000_100,
                1_050_505,
            )
        })
        .map_err(|error| format!("Rust cannot bind/accept Python duel: {error}"))?;
    wheel_spin_repository
        .log_wheel_spin(&NewWheelSpin {
            discord_id: 1_010_101,
            guild_id: Some(424_242),
            result: 0,
            spin_time: 1_700_000_001,
            is_bankrupt: false,
            is_golden: false,
            outcome_code: Some("LIGHTNING_BOLT".to_owned()),
            is_bonus: true,
            event_id: Some("rust-wheel-2".to_owned()),
            outcome_metadata: Some(JsonValue::Object(BTreeMap::from([
                ("lightning_count".to_owned(), JsonValue::Integer(2)),
                ("lightning_total".to_owned(), JsonValue::Integer(40)),
            ]))),
        })
        .map_err(|error| format!("Rust cannot append to Python wheel history: {error}"))?;
    economy_event_repository
        .ensure_policy_state(
            Some(424_242),
            PolicyMode::Recovery,
            0.01,
            0.06,
            1_700_000_100,
        )
        .map_err(|error| format!("Rust cannot update Python economy policy: {error}"))?;
    let repayment = loan_repository
        .execute_repayment_atomic(1_060_606, Some(434_343))
        .map_err(|error| format!("Rust cannot repay Python-created loan: {error}"))?;
    if repayment.principal != 100
        || repayment.fee != 10
        || repayment.total_owed != 110
        || repayment.balance_before != 150
        || repayment.new_balance != 40
        || repayment.nonprofit_total != 10
    {
        return Err(format!(
            "Rust repaid Python-created loan incorrectly: {repayment:?}"
        ));
    }
    core_player_repository
        .update_roles(1_010_101, Some(424_242), &["1".to_owned(), "5".to_owned()])
        .and_then(|()| {
            core_player_repository.update_glicko_rating(
                1_010_101,
                Some(424_242),
                1_777.0,
                88.0,
                0.07,
            )
        })
        .map_err(|error| format!("Rust core repository cannot update Python player: {error}"))?;

    let python_read = run_python_db_bridge(root, "read", &database_path)?;
    if python_read
        != "424242\t888\ttrue\tfalse\t515151\t7\t7\t84\ttrue\trust replacement\t904\t424242\t616161\t626262\t2\t424242\t717171\t727272\t3\t70\t30\t2\t5\t0\t0\t2\t1\t2\t1\t902\t1010101\tInterop Blep\tunhatched\tstandard\t1800000000\t1800086400\t965\t3\t4\t1\tactive\t2\t3\t1\taccepted\t5555\ttrial_by_combat\t450\t500\t2\t25\tCROWN\t0\tpython-wheel-1\t{\"a\":{\"b\":true},\"z\":2}\t0\tLIGHTNING_BOLT\t1\trust-wheel-2\t{\"lightning_count\":2,\"lightning_total\":40}\t424242\trecovery\t0.01\t0.06\t1700000100\t1700000100\t1060606\t434343\t1\t10\t0\t0\t0\t40\t10\tloan,loan_repayment,loan_repayment\t1,5\t1777.0\t88.0\t0.07"
    {
        return Err(format!(
            "Python decoded Rust repository writes incorrectly: {python_read:?}"
        ));
    }
    Ok(())
}

fn run_python_db_bridge(root: &Path, operation: &str, path: &Path) -> Result<String, String> {
    let path = path
        .to_str()
        .ok_or_else(|| "interop DB path is not UTF-8".to_owned())?;
    let output = checked_output(
        parity_python(root).args(["-m", "rust.parity.python_db_bridge", operation, path]),
        "run Python repository interoperability bridge",
    )?;
    Ok(
        output_text(output, "Python repository interoperability bridge")?
            .trim()
            .to_owned(),
    )
}

fn verify_domain_vectors(root: &Path) -> Result<usize, String> {
    let path = root.join("rust/parity/domain_vectors.tsv");
    let contents = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let vectors: Vec<_> = contents
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#')
        })
        .collect();
    if vectors.is_empty() {
        return Err("domain vector inventory is empty".to_owned());
    }
    let vector_count = vectors.len();

    let mut child = parity_python(root)
        .args(["-m", "rust.parity.python_vectors"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start Python domain vector runner: {error}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "Python domain vector runner has no stdin".to_owned())?
        .write_all(contents.as_bytes())
        .map_err(|error| format!("cannot feed Python domain vectors: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("cannot wait for Python domain vectors: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Python domain vectors failed with {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let python_stdout = output_text(output, "Python domain vectors")?;
    let mut python_results = BTreeMap::new();
    for line in python_stdout.lines() {
        let (vector_id, result) = line
            .split_once('\t')
            .ok_or_else(|| format!("invalid Python vector output {line:?}"))?;
        if python_results.insert(vector_id, result).is_some() {
            return Err(format!("duplicate Python vector result {vector_id:?}"));
        }
    }

    let mut seen_ids = BTreeSet::new();
    for vector in vectors {
        let fields: Vec<_> = vector.split('\t').collect();
        let [vector_id, operation, arguments @ ..] = fields.as_slice() else {
            return Err(format!("invalid domain vector {vector:?}"));
        };
        if !seen_ids.insert(*vector_id) {
            return Err(format!("duplicate domain vector ID {vector_id:?}"));
        }
        let rust_result = evaluate_rust_vector(operation, arguments)?;
        let python_result = python_results
            .remove(vector_id)
            .ok_or_else(|| format!("Python omitted domain vector {vector_id:?}"))?;
        compare_vector_result(vector_id, operation, &rust_result, python_result)?;
    }
    if !python_results.is_empty() {
        return Err(format!(
            "Python emitted unknown domain vector IDs: {}",
            python_results
                .keys()
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(vector_count)
}

fn evaluate_rust_vector(operation: &str, arguments: &[&str]) -> Result<String, String> {
    use std::sync::Arc;

    use cama_app::ai_services::{
        ColumnSchema, InMemoryAIQueryRepository, QueryResult, SQLQueryService, TableSchema, Value,
    };
    use cama_app::ask::format_query_result;
    use cama_app::dig_view_supplements::phase_event_boss_hp_delta;
    use cama_app::disburse_actions::{METHODS, Proposal, build_disburse_embed};
    use cama_app::economy_actions::{apply_gamba_event_multiplier, bounded_economy_multiplier};
    use cama_app::wrapped_story::FLAVOR_POOLS;
    use cama_db::package_deal_repository::calculate_package_deal_cost;
    use cama_domain::catalogs::{BETTING_PERSONAS, PERSONAS};
    use cama_domain::config_contract::select_llm_config;
    use cama_domain::dig_economy::{
        WagerMultiplier, WinChance, effective_wager_multiplier, log_capped_multiplier,
        scale_positive_dig_jc, settled_wager_multiplier,
    };
    use cama_domain::dig_splash::strengthen_dig_event_penalty;
    use cama_domain::dig_stats::{MinerStats, miner_stat_effects, wager_skin_bonus};
    use cama_domain::economy_scaling::{
        scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta,
    };
    use cama_domain::embed_safety::{EmbedModel, add_lines_field, truncate_field, validate_embed};
    use cama_domain::guild::{is_dm_context, normalize_guild_id};
    use cama_domain::openskill::{CamaOpenSkillSystem, Rating};
    use cama_domain::pet::PetStage;
    use cama_domain::pet_brawl::{
        BrawlWinner, DuelistSnapshot, PetBrawlMove, brawl_traits, build_duelist, initial_state,
        move_name, resolve_round as resolve_pet_brawl_round,
    };
    use cama_domain::pet_evolution::{PetInstinct, hint_key_for_scores, resolve_evolution};
    use cama_domain::rating::CamaRatingSystem;
    use cama_domain::rating_insights::{get_rd_tier_name, rd_to_certainty};
    use cama_domain::wheel::{
        WHEEL_EXPLOSION_REWARD, WedgeValue, eruption_reward, golden_wheel_wedges, wheel_wedges,
    };

    let argument = |index: usize| {
        arguments
            .get(index)
            .copied()
            .ok_or_else(|| format!("{operation} is missing argument {}", index + 1))
    };
    let escape_vector_text = |value: String| {
        value
            .replace('\\', "\\\\")
            .replace('\t', "\\t")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
    };
    let parse_i64 = |index| {
        argument(index)?.parse::<i64>().map_err(|error| {
            format!(
                "{operation} argument {:?} is not an integer: {error}",
                argument(index).unwrap_or_default()
            )
        })
    };
    let parse_i32 = |index| {
        argument(index)?.parse::<i32>().map_err(|error| {
            format!(
                "{operation} argument {:?} is not an i32: {error}",
                argument(index).unwrap_or_default()
            )
        })
    };
    let parse_f64 = |index| {
        argument(index)?.parse::<f64>().map_err(|error| {
            format!(
                "{operation} argument {:?} is not a float: {error}",
                argument(index).unwrap_or_default()
            )
        })
    };
    let optional_guild = || match argument(0)? {
        "none" => Ok(None),
        _ => parse_i64(0).map(Some),
    };
    let parse_optional_f64 = |index| match argument(index)? {
        "none" => Ok(None),
        _ => parse_f64(index).map(Some),
    };
    let parse_bool = |index| match argument(index)? {
        "true" => Ok(true),
        "false" => Ok(false),
        value => Err(format!("{operation} argument {value:?} is not a boolean")),
    };
    let parse_pet_brawl_move = |index| match argument(index)? {
        "spit" => Ok(PetBrawlMove::Spit),
        "stampede" => Ok(PetBrawlMove::Stampede),
        "hunker" => Ok(PetBrawlMove::Hunker),
        "feint" => Ok(PetBrawlMove::Feint),
        "sidestep" => Ok(PetBrawlMove::Sidestep),
        value => Err(format!("invalid pet brawl move {value:?}")),
    };
    let parse_pet_stage = |index| match argument(index)? {
        "egg" => Ok(PetStage::Egg),
        "baby" => Ok(PetStage::Baby),
        "adult" => Ok(PetStage::Adult),
        value => Err(format!("invalid pet stage {value:?}")),
    };
    let parse_pet_scores = |index: usize| {
        let values = argument(index)?
            .split(',')
            .map(|value| {
                value
                    .parse::<i64>()
                    .map_err(|error| format!("invalid pet evolution score {value:?}: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if values.len() != PetInstinct::ALL.len() {
            return Err(format!(
                "pet evolution vector must contain {} scores, found {}",
                PetInstinct::ALL.len(),
                values.len()
            ));
        }
        Ok(PetInstinct::ALL.into_iter().zip(values).collect())
    };

    match operation {
        "normalize_guild_id" => Ok(normalize_guild_id(optional_guild()?).to_string()),
        "is_dm_context" => Ok(is_dm_context(optional_guild()?).to_string()),
        "select_llm_config" => {
            let optional_text = |index| -> Result<Option<&str>, String> {
                match argument(index)? {
                    "none" => Ok(None),
                    value => Ok(Some(value)),
                }
            };
            let (model, api_key) =
                select_llm_config(optional_text(0)?, optional_text(1)?, optional_text(2)?);
            Ok(format!("{model}|{}", api_key.unwrap_or("none")))
        }
        "gamba_bound" => Ok(bounded_economy_multiplier(parse_f64(0)?).to_string()),
        "gamba_scale" => {
            Ok(
                apply_gamba_event_multiplier(parse_i64(0)?, parse_f64(1)?, parse_f64(2)?)
                    .to_string(),
            )
        }
        "dig_strengthen_penalty" => Ok(strengthen_dig_event_penalty(parse_i64(0)?).to_string()),
        "dig_splash_burn_penalty" => Ok(scale_deflationary_minigame_jc_delta(
            strengthen_dig_event_penalty(parse_i64(0)?) as f64,
            1.0,
        )
        .to_string()),
        "dig_phase_event_boss_hp_delta" => Ok(phase_event_boss_hp_delta(argument(0)?).to_string()),
        "wheel_catalog" => {
            let wedges = match argument(0)? {
                "regular" => wheel_wedges(1.0),
                "golden" => golden_wheel_wedges(1.0),
                value => return Err(format!("unknown wheel catalog {value:?}")),
            };
            let mut rows = wedges
                .into_iter()
                .filter_map(|wedge| match wedge.value {
                    WedgeValue::Numeric(value) if value < 0 => None,
                    WedgeValue::Numeric(value) => {
                        Some(format!("{}|{value}|{}", wedge.label, wedge.color))
                    }
                    WedgeValue::Special(value) => {
                        Some(format!("{}|{value}|{}", wedge.label, wedge.color))
                    }
                })
                .collect::<Vec<_>>();
            rows.sort();
            Ok(format!(
                "{:016x}",
                fnv1a64_lines(rows.iter().map(String::as_str))
            ))
        }
        "wheel_explosion_reward" => Ok(WHEEL_EXPLOSION_REWARD.to_string()),
        "wheel_eruption" => Ok(eruption_reward(
            match argument(0)? {
                "none" => None,
                _ => Some(parse_i64(0)?),
            },
            1.0,
        )
        .to_string()),
        "embed_truncate_repeat" => {
            let character = match argument(0)? {
                "llama" => '🦙',
                raw => {
                    let mut characters = raw.chars();
                    let character = characters
                        .next()
                        .ok_or_else(|| "embed repeat character cannot be empty".to_owned())?;
                    if characters.next().is_some() {
                        return Err(format!(
                            "embed repeat character must contain one character, found {raw:?}"
                        ));
                    }
                    character
                }
            };
            let count = usize::try_from(parse_i64(1)?)
                .map_err(|error| format!("invalid embed repeat count: {error}"))?;
            let max_len = usize::try_from(parse_i64(2)?)
                .map_err(|error| format!("invalid embed truncate limit: {error}"))?;
            let source = std::iter::repeat_n(character, count).collect::<String>();
            let result = truncate_field(&source, max_len);
            Ok(format!(
                "{}|{}|{:016x}",
                result.chars().count(),
                result.ends_with("..."),
                fnv1a64_lines([result.as_str()].into_iter())
            ))
        }
        "embed_pack_lines" => {
            let line_count = usize::try_from(parse_i64(0)?)
                .map_err(|error| format!("invalid embed line count: {error}"))?;
            let payload_len = usize::try_from(parse_i64(1)?)
                .map_err(|error| format!("invalid embed line payload: {error}"))?;
            let max_len = usize::try_from(parse_i64(2)?)
                .map_err(|error| format!("invalid embed field limit: {error}"))?;
            let lines = (0..line_count)
                .map(|index| format!("item-{index:03}-{}", "x".repeat(payload_len)))
                .collect::<Vec<_>>();
            let mut embed = EmbedModel::default();
            add_lines_field(&mut embed, "Stuff", &lines, false, max_len);
            let rows = embed
                .fields
                .iter()
                .map(|field| {
                    format!(
                        "{}\0{}\0{}",
                        field.name,
                        field.value,
                        if field.inline { "True" } else { "False" }
                    )
                })
                .collect::<Vec<_>>();
            Ok(format!(
                "{}|{:016x}",
                embed.fields.len(),
                fnv1a64_lines(rows.iter().map(String::as_str))
            ))
        }
        "embed_validate" => {
            let parse_size = |index| {
                usize::try_from(parse_i64(index)?)
                    .map_err(|error| format!("invalid embed component length: {error}"))
            };
            let optional_repeated = |index, character: char| -> Result<Option<String>, String> {
                if parse_i64(index)? < 0 {
                    Ok(None)
                } else {
                    Ok(Some(
                        std::iter::repeat_n(character, parse_size(index)?).collect(),
                    ))
                }
            };
            let mut embed = EmbedModel {
                title: optional_repeated(0, 't')?,
                description: optional_repeated(1, 'd')?,
                footer: optional_repeated(4, 'f')?,
                author: optional_repeated(5, 'a')?,
                ..EmbedModel::default()
            };
            let field_name = "n".repeat(parse_size(2)?);
            let field_value = "v".repeat(parse_size(3)?);
            for _ in 0..parse_size(6)? {
                embed.add_field(&field_name, &field_value, false);
            }
            Ok(validate_embed(&embed).join("|"))
        }
        "persona_catalog_fingerprint" => {
            let mut fingerprint = 0xcbf2_9ce4_8422_2325_u64;
            let mut first = true;
            for persona in PERSONAS.iter().chain(BETTING_PERSONAS.iter()) {
                for field in [
                    persona.key,
                    persona.key,
                    persona.name,
                    persona.system_prompt,
                ]
                .into_iter()
                .chain(persona.examples.iter().copied())
                {
                    if first {
                        first = false;
                    } else {
                        fingerprint ^= 0;
                        fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
                    }
                    for byte in field.as_bytes() {
                        fingerprint ^= u64::from(*byte);
                        fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
                    }
                }
            }
            Ok(format!("{fingerprint:016x}"))
        }
        "wrapped_flavor_catalog_fingerprint" => {
            let rows = FLAVOR_POOLS.iter().flat_map(|(key, pool)| {
                pool.iter()
                    .enumerate()
                    .map(move |(index, entry)| format!("{key}\0{index}\0{entry}"))
            });
            Ok(format!(
                "{:016x}",
                fnv1a64_lines(rows.collect::<Vec<_>>().iter().map(String::as_str))
            ))
        }
        "disburse_embed_fingerprint" => {
            let fund_amount = parse_i64(0)?;
            let even_votes = u32::try_from(parse_i64(1)?)
                .map_err(|error| format!("invalid even vote count: {error}"))?;
            let proportional_votes = u32::try_from(parse_i64(2)?)
                .map_err(|error| format!("invalid proportional vote count: {error}"))?;
            let quorum_required = u32::try_from(parse_i64(3)?)
                .map_err(|error| format!("invalid disbursement quorum: {error}"))?;
            let mut votes = METHODS
                .iter()
                .map(|(method, _)| ((*method).to_owned(), 0))
                .collect::<BTreeMap<_, _>>();
            votes.insert("even".to_owned(), even_votes);
            votes.insert("proportional".to_owned(), proportional_votes);
            let embed = build_disburse_embed(&Proposal {
                guild_id: 0,
                proposal_id: 1,
                message_id: None,
                channel_id: None,
                fund_amount,
                quorum_required,
                status: "active".to_owned(),
                votes,
            });
            let mut rows = vec![embed.title, embed.description];
            rows.extend(embed.fields.into_iter().map(|field| {
                format!(
                    "{}\0{}\0{}",
                    field.name,
                    field.value,
                    if field.inline { "True" } else { "False" }
                )
            }));
            rows.push(format!("footer\0{}", embed.footer.unwrap_or_default()));
            Ok(format!(
                "{:016x}",
                fnv1a64_lines(rows.iter().map(String::as_str))
            ))
        }
        "sql_validate" => {
            let repository = Arc::new(InMemoryAIQueryRepository::new([
                TableSchema {
                    name: "players".to_owned(),
                    columns: vec![
                        ColumnSchema::new("guild_id", "INTEGER", true, false),
                        ColumnSchema::new("discord_id", "INTEGER", true, false),
                        ColumnSchema::new("discord_username", "TEXT", true, false),
                        ColumnSchema::new("balance", "INTEGER", true, false),
                    ],
                    foreign_keys: Vec::new(),
                },
                TableSchema {
                    name: "player_steam_ids".to_owned(),
                    columns: vec![ColumnSchema::new("steam_id", "INTEGER", true, false)],
                    foreign_keys: Vec::new(),
                },
            ]));
            match SQLQueryService::new(repository).validate_sql(argument(0)?) {
                Ok(()) => Ok("ok".to_owned()),
                Err(error) => Ok(format!("error:{}", error.to_string().to_ascii_lowercase())),
            }
        }
        "ask_format_no_results" => Ok(escape_vector_text(format_query_result(
            &QueryResult {
                success: true,
                ..QueryResult::default()
            },
            1_024,
        ))),
        "ask_format_failure" => Ok(escape_vector_text(format_query_result(
            &QueryResult {
                success: false,
                error: Some("boom".to_owned()),
                ..QueryResult::default()
            },
            1_024,
        ))),
        "ask_format_single" => Ok(escape_vector_text(format_query_result(
            &QueryResult {
                success: true,
                results: vec![BTreeMap::from([
                    (
                        "discord_username".to_owned(),
                        Value::Text("Solo".to_owned()),
                    ),
                    ("wins".to_owned(), Value::Integer(5)),
                ])],
                row_count: 1,
                ..QueryResult::default()
            },
            1_024,
        ))),
        "ask_format_multi" => Ok(escape_vector_text(format_query_result(
            &QueryResult {
                success: true,
                results: vec![
                    BTreeMap::from([
                        ("discord_username".to_owned(), Value::Text("A".to_owned())),
                        ("wins".to_owned(), Value::Integer(10)),
                    ]),
                    BTreeMap::from([
                        ("discord_username".to_owned(), Value::Text("B".to_owned())),
                        ("wins".to_owned(), Value::Integer(7)),
                    ]),
                ],
                row_count: 2,
                ..QueryResult::default()
            },
            1_024,
        ))),
        "ask_format_cap" => Ok(escape_vector_text(format_query_result(
            &QueryResult {
                success: true,
                results: (0..12)
                    .map(|index| {
                        BTreeMap::from([
                            (
                                "discord_username".to_owned(),
                                Value::Text(format!("P{index}")),
                            ),
                            ("wins".to_owned(), Value::Integer(index)),
                        ])
                    })
                    .collect(),
                row_count: 12,
                ..QueryResult::default()
            },
            1_024,
        ))),
        "pet_evolution" => {
            let decision = resolve_evolution(&parse_pet_scores(1)?, parse_i64(0)?);
            let instinct = |value: Option<PetInstinct>| value.map_or("none", PetInstinct::as_str);
            Ok(format!(
                "{},{},{}",
                decision.calling.as_str(),
                instinct(decision.primary),
                instinct(decision.secondary)
            ))
        }
        "pet_evolution_hint" => {
            Ok(hint_key_for_scores(&parse_pet_scores(0)?).unwrap_or_else(|| "none".to_owned()))
        }
        "pet_brawl_move_name" => Ok(move_name(argument(0)?, parse_pet_brawl_move(1)?).to_owned()),
        "pet_brawl_traits" => {
            let traits = brawl_traits(argument(0)?);
            Ok(format!(
                "{},{},{},{},{},{}",
                traits.dodge_bonus_pp,
                traits.crit_pct,
                traits.damage_dealt_bonus,
                traits.damage_taken_mod,
                traits.heal_bonus,
                traits.loss_insurance
            ))
        }
        "pet_brawl_duelist" => {
            let duelist = build_duelist(DuelistSnapshot {
                pet_id: 1,
                owner_id: 2,
                name: "Vector",
                species_id: argument(0)?,
                stage: parse_pet_stage(1)?,
                hunger: parse_i32(2)?,
                happy: parse_bool(3)?,
                aegis_used: parse_i64(4)?,
                training_str: parse_i32(5)?,
                training_int: parse_i32(6)?,
                training_dex: parse_i32(7)?,
            });
            Ok(format!(
                "{},{},{},{},{},{},{},{}",
                duelist.tier.as_str(),
                duelist.max_hp,
                duelist.hp,
                duelist.power,
                duelist.shield_available,
                duelist.training_str,
                duelist.training_int,
                duelist.training_dex
            ))
        }
        "pet_brawl_round" => {
            let make_duelist = |pet_id, owner_id, name, species_id| {
                build_duelist(DuelistSnapshot {
                    pet_id,
                    owner_id,
                    name,
                    species_id,
                    stage: PetStage::Adult,
                    hunger: 100,
                    happy: false,
                    aegis_used: 0,
                    training_str: 0,
                    training_int: 0,
                    training_dex: 0,
                })
            };
            let mut a = make_duelist(1, 10, "A", argument(0)?);
            let mut b = make_duelist(2, 20, "B", argument(1)?);
            a.hp = parse_i32(4)?;
            b.hp = parse_i32(5)?;
            let mut state = initial_state(a, b);
            state.round_no = u8::try_from(parse_i32(6)?)
                .map_err(|error| format!("invalid pet-brawl round number: {error}"))?;
            let values = argument(7)?
                .split(',')
                .map(|value| {
                    value
                        .parse::<i32>()
                        .map_err(|error| format!("invalid scripted brawl RNG {value:?}: {error}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut rng = ScriptedBrawlRng { values, cursor: 0 };
            let (state, log) = resolve_pet_brawl_round(
                &state,
                parse_pet_brawl_move(2)?,
                parse_pet_brawl_move(3)?,
                &mut rng,
            )
            .map_err(|error| error.to_string())?;
            let winner = match state.winner {
                Some(BrawlWinner::A) => "a",
                Some(BrawlWinner::B) => "b",
                Some(BrawlWinner::Draw) => "draw",
                None => "none",
            };
            Ok(format!(
                "{},{},{},{},{},{},{},{:016x}",
                state.a.hp,
                state.b.hp,
                state.round_no,
                winner,
                state.a.shield_available,
                state.b.shield_available,
                log.len(),
                fnv1a64_lines(log.iter().map(String::as_str))
            ))
        }
        "package_deal_cost" => Ok(calculate_package_deal_cost(
            parse_i64(0)?,
            match argument(1)? {
                "true" => true,
                "false" => false,
                value => return Err(format!("invalid package-deal extend flag {value:?}")),
            },
            parse_optional_f64(2)?,
            parse_optional_f64(3)?,
        )
        .to_string()),
        "dig_cave_in_band" => Ok(cama_domain::dig_cave_in::cave_in_band(parse_i64(0)?)
            .as_str()
            .to_owned()),
        "dig_cave_in_catalog" => Ok(cave_in_catalog()),
        "scale_minigame" => Ok(scale_minigame_jc_delta(parse_f64(0)?, parse_f64(1)?).to_string()),
        "scale_deflationary" => {
            Ok(scale_deflationary_minigame_jc_delta(parse_f64(0)?, parse_f64(1)?).to_string())
        }
        "dig_scale_positive" => Ok(scale_positive_dig_jc(parse_i64(0)?).to_string()),
        "dig_wager_taper" => {
            let multiplier =
                WagerMultiplier::new(parse_f64(0)?).map_err(|error| error.to_string())?;
            let chance = WinChance::new(parse_f64(1)?).map_err(|error| error.to_string())?;
            Ok(effective_wager_multiplier(multiplier, chance)
                .value()
                .to_string())
        }
        "dig_log_cap" => {
            let multiplier =
                WagerMultiplier::new(parse_f64(0)?).map_err(|error| error.to_string())?;
            Ok(log_capped_multiplier(multiplier).value().to_string())
        }
        "dig_settled_multiplier" => {
            let tapered = WagerMultiplier::new(parse_f64(0)?).map_err(|error| error.to_string())?;
            let bonus = WagerMultiplier::new(parse_f64(1)?).map_err(|error| error.to_string())?;
            Ok(settled_wager_multiplier(tapered, Some(bonus))
                .value()
                .to_string())
        }
        "dig_strength_effects" => {
            let stats = MinerStats::new(parse_i64(0)?, 0, 0).map_err(|error| error.to_string())?;
            let effects = miner_stat_effects(stats);
            Ok(format!(
                "{},{}",
                effects.advance_min_bonus, effects.advance_max_bonus
            ))
        }
        "dig_smarts_effect" => {
            let stats = MinerStats::new(0, parse_i64(0)?, 0).map_err(|error| error.to_string())?;
            Ok(miner_stat_effects(stats).cave_in_reduction.to_string())
        }
        "dig_stamina_effect" => {
            let stats = MinerStats::new(0, 0, parse_i64(0)?).map_err(|error| error.to_string())?;
            Ok(miner_stat_effects(stats).cooldown_multiplier.to_string())
        }
        "dig_wager_skin_bonus" => Ok(wager_skin_bonus(parse_i64(0)?).to_string()),
        "mmr_to_rating" => Ok(CamaRatingSystem::default()
            .mmr_to_rating(parse_i32(0)?)
            .to_string()),
        "new_player_seed_mmr" => Ok(CamaRatingSystem::default()
            .new_player_seed_mmr(parse_i32(0)?)
            .to_string()),
        "streak" => {
            let history = if argument(0)? == "-" {
                Vec::new()
            } else {
                argument(0)?
                    .split(',')
                    .map(parse_outcome)
                    .collect::<Result<Vec<_>, _>>()?
            };
            let won = parse_outcome(argument(1)?)?;
            let rate = if argument(2)? == "default" {
                None
            } else {
                Some(parse_f64(2)?)
            };
            let threshold = if argument(3)? == "default" {
                None
            } else {
                Some(parse_i64(3)?)
            };
            let adjustment = CamaRatingSystem::default()
                .calculate_streak_multiplier(&history, won, rate, threshold);
            Ok(format!(
                "{},{:.17}",
                adjustment.streak_length, adjustment.multiplier
            ))
        }
        "rd_to_certainty" => Ok(rd_to_certainty(parse_f64(0)?).to_string()),
        "rd_tier" => Ok(get_rd_tier_name(parse_f64(0)?).to_owned()),
        "openskill_display" => Ok(CamaOpenSkillSystem::new()
            .mu_to_display(parse_f64(0)?)
            .to_string()),
        "openskill_fingerprint" => Ok(CamaOpenSkillSystem::new().algorithm_fingerprint()),
        "openskill_calibrate" => CamaOpenSkillSystem::new()
            .calibrate_win_probability(parse_f64(0)?, Some(parse_f64(1)?))
            .map(|probability| probability.to_string())
            .map_err(|error| error.to_string()),
        "openskill_predict" => {
            let parse_team = |raw: &str| {
                raw.split(';')
                    .map(|player| {
                        let (mu, sigma) = player
                            .split_once(',')
                            .ok_or_else(|| format!("invalid OpenSkill rating {player:?}"))?;
                        Ok(Rating::new(
                            mu.parse()
                                .map_err(|error| format!("invalid OpenSkill mu {mu:?}: {error}"))?,
                            sigma.parse().map_err(|error| {
                                format!("invalid OpenSkill sigma {sigma:?}: {error}")
                            })?,
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()
            };
            let team1 = parse_team(argument(0)?)?;
            let team2 = parse_team(argument(1)?)?;
            Ok(CamaOpenSkillSystem::new()
                .os_predict_win_probability(&team1, &team2)
                .to_string())
        }
        _ => Err(format!("unknown Rust vector operation {operation:?}")),
    }
}

fn cave_in_catalog() -> String {
    use cama_domain::dig_cave_in::{
        CAVE_IN_BLOCK_LOSS_RANGES, CAVE_IN_CATASTROPHIC_GEAR_TICKS,
        CAVE_IN_CATASTROPHIC_MEDICAL_BILL, CAVE_IN_CATASTROPHIC_MILESTONE_STEP,
        CAVE_IN_CATASTROPHIC_PCT_BY_BAND, CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE,
        CAVE_IN_CONSEQUENCE_WEIGHTS, CAVE_IN_INJURY_DIGS_BY_BAND, CAVE_IN_MEDICAL_BILL_RANGES,
        CAVE_IN_STUN_DIGS_BY_BAND, CaveInBand,
    };

    let mut rows = Vec::new();
    for band in [
        CaveInBand::Shallow,
        CaveInBand::Mid,
        CaveInBand::Deep,
        CaveInBand::Endgame,
    ] {
        let (block_min, block_max) = CAVE_IN_BLOCK_LOSS_RANGES[band];
        let (bill_min, bill_max) = CAVE_IN_MEDICAL_BILL_RANGES[band];
        let weights = CAVE_IN_CONSEQUENCE_WEIGHTS[band]
            .iter()
            .map(|entry| format!("{}={}", entry.consequence.as_str(), entry.weight))
            .collect::<Vec<_>>()
            .join(",");
        let catastrophic_basis_points =
            (CAVE_IN_CATASTROPHIC_PCT_BY_BAND[band] * 10_000.0).round() as u32;
        rows.push(format!(
            "{}:{block_min}-{block_max}:{bill_min}-{bill_max}:{}:{}:{catastrophic_basis_points}:{weights}",
            band.as_str(),
            CAVE_IN_STUN_DIGS_BY_BAND[band],
            CAVE_IN_INJURY_DIGS_BY_BAND[band],
        ));
    }
    rows.push(format!(
        "cat:{}-{}:{}-{}:{}:{}",
        CAVE_IN_CATASTROPHIC_MEDICAL_BILL.0,
        CAVE_IN_CATASTROPHIC_MEDICAL_BILL.1,
        CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE.0,
        CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE.1,
        CAVE_IN_CATASTROPHIC_GEAR_TICKS,
        CAVE_IN_CATASTROPHIC_MILESTONE_STEP,
    ));
    rows.join("|")
}

fn parse_outcome(value: &str) -> Result<bool, String> {
    match value {
        "W" => Ok(true),
        "L" => Ok(false),
        _ => Err(format!("invalid outcome {value:?}")),
    }
}

fn compare_vector_result(
    vector_id: &str,
    operation: &str,
    rust: &str,
    python: &str,
) -> Result<(), String> {
    let mismatch = || {
        Err(format!(
            "domain vector {vector_id:?} differs: Python={python:?}, Rust={rust:?}"
        ))
    };
    match operation {
        "gamba_bound"
        | "mmr_to_rating"
        | "rd_to_certainty"
        | "openskill_calibrate"
        | "openskill_predict"
        | "dig_wager_taper"
        | "dig_log_cap"
        | "dig_settled_multiplier"
        | "dig_smarts_effect"
        | "dig_stamina_effect"
        | "dig_wager_skin_bonus" => {
            let rust_value: f64 = rust
                .parse()
                .map_err(|error| format!("invalid Rust float {rust:?}: {error}"))?;
            let python_value: f64 = python
                .parse()
                .map_err(|error| format!("invalid Python float {python:?}: {error}"))?;
            let tolerance = if operation == "openskill_predict" {
                2e-7
            } else {
                1e-10
            };
            if (rust_value - python_value).abs() <= tolerance {
                Ok(())
            } else {
                mismatch()
            }
        }
        "streak" => {
            let (rust_length, rust_multiplier) = rust
                .split_once(',')
                .ok_or_else(|| format!("invalid Rust streak result {rust:?}"))?;
            let (python_length, python_multiplier) = python
                .split_once(',')
                .ok_or_else(|| format!("invalid Python streak result {python:?}"))?;
            let rust_multiplier: f64 = rust_multiplier
                .parse()
                .map_err(|error| format!("invalid Rust streak multiplier: {error}"))?;
            let python_multiplier: f64 = python_multiplier
                .parse()
                .map_err(|error| format!("invalid Python streak multiplier: {error}"))?;
            if rust_length == python_length && (rust_multiplier - python_multiplier).abs() <= 1e-12
            {
                Ok(())
            } else {
                mismatch()
            }
        }
        _ if rust == python => Ok(()),
        _ => mismatch(),
    }
}

fn verify_schema_contract(root: &Path) -> Result<(), String> {
    let output = checked_output(
        parity_python(root).args(["-c", PYTHON_MIGRATIONS_SCRIPT]),
        "read Python migration contract",
    )?;
    let stdout = output_text(output, "Python migration contract")?;
    let python: Vec<_> = stdout.lines().filter(|line| !line.is_empty()).collect();
    let rust = cama_db::expected_migrations();
    if python != rust {
        return Err(format!(
            "Rust migration manifest is stale: Python declares {} migrations and Rust declares {}; update rust/schema/expected_migrations.txt",
            python.len(),
            rust.len()
        ));
    }
    Ok(())
}

fn repository_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot locate repository root from xtask manifest".to_owned())
}

/// Construct the Python process used by every cross-language parity probe.
///
/// CI deliberately keeps `uv run --locked python` as the default so the lock
/// file remains authoritative. Sandboxed/offline development can point this
/// at the already-created locked environment (normally `.venv/bin/python`)
/// without changing what any probe executes or verifies.
fn parity_python(root: &Path) -> Command {
    if let Some(executable) = env::var_os("CAMA_PARITY_PYTHON") {
        let executable = PathBuf::from(executable);
        let executable = if executable.is_absolute() {
            executable
        } else {
            root.join(executable)
        };
        let mut command = Command::new(executable);
        command.current_dir(root);
        command
    } else {
        let mut command = Command::new("uv");
        command
            .current_dir(root)
            .args(["run", "--locked", "python"]);
        command
    }
}

fn checked_output(command: &mut Command, description: &str) -> Result<Output, String> {
    let output = command
        .output()
        .map_err(|error| format!("cannot {description}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot {description}: process exited {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output)
}

fn output_text(output: Output, description: &str) -> Result<String, String> {
    String::from_utf8(output.stdout)
        .map_err(|error| format!("{description} emitted non-UTF-8 output: {error}"))
}

struct Baseline {
    collected_cases: usize,
    node_id_fingerprint: u64,
}

fn read_baseline(path: &Path) -> Result<Baseline, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut collected_cases = None;
    let mut node_id_fingerprint = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("invalid baseline line {line:?}"))?;
        match key.trim() {
            "collected_cases" => {
                collected_cases = Some(
                    value
                        .trim()
                        .parse()
                        .map_err(|error| format!("invalid collected_cases: {error}"))?,
                );
            }
            "node_id_fnv1a64" => {
                node_id_fingerprint = Some(
                    u64::from_str_radix(value.trim(), 16)
                        .map_err(|error| format!("invalid node_id_fnv1a64: {error}"))?,
                );
            }
            key => return Err(format!("unknown baseline key {key:?}")),
        }
    }
    Ok(Baseline {
        collected_cases: collected_cases
            .ok_or_else(|| "baseline is missing collected_cases".to_owned())?,
        node_id_fingerprint: node_id_fingerprint
            .ok_or_else(|| "baseline is missing node_id_fnv1a64".to_owned())?,
    })
}

struct Mapping {
    python_test: String,
    rust_test: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadinessStatus {
    Open,
    Complete,
}

#[derive(Debug, Eq, PartialEq)]
struct ReadinessGate {
    id: String,
    status: ReadinessStatus,
}

fn read_cutover_readiness(path: &Path) -> Result<Vec<ReadinessGate>, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse_cutover_readiness(&contents, &path.display().to_string())
}

fn parse_cutover_readiness(contents: &str, source: &str) -> Result<Vec<ReadinessGate>, String> {
    let mut gates = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        let [id, status, evidence] = fields.as_slice() else {
            return Err(format!(
                "{source}:{} must contain exactly three tab-separated fields: gate, status, evidence",
                index + 1
            ));
        };
        if id.is_empty()
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(format!(
                "{source}:{} has invalid readiness gate ID {id:?}",
                index + 1
            ));
        }
        if !seen.insert(*id) {
            return Err(format!(
                "{source}:{} duplicates readiness gate {id:?}",
                index + 1
            ));
        }
        let status = match *status {
            "open" => ReadinessStatus::Open,
            "complete" => ReadinessStatus::Complete,
            value => {
                return Err(format!(
                    "{source}:{} has invalid readiness status {value:?}; expected open or complete",
                    index + 1
                ));
            }
        };
        if evidence.trim().is_empty() {
            return Err(format!(
                "{source}:{} must explain the evidence or remaining blocker",
                index + 1
            ));
        }
        gates.push(ReadinessGate {
            id: (*id).to_owned(),
            status,
        });
    }

    let required = REQUIRED_CUTOVER_GATES
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let actual = gates
        .iter()
        .map(|gate| gate.id.as_str())
        .collect::<BTreeSet<_>>();
    let missing = required.difference(&actual).copied().collect::<Vec<_>>();
    let unknown = actual.difference(&required).copied().collect::<Vec<_>>();
    if !missing.is_empty() || !unknown.is_empty() {
        return Err(format!(
            "{source} readiness inventory drifted; missing [{}], unknown [{}]",
            missing.join(", "),
            unknown.join(", ")
        ));
    }
    gates.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(gates)
}

fn read_mappings(path: &Path) -> Result<Vec<Mapping>, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    contents
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#')
        })
        .map(|(index, line)| {
            let (python_test, rust_test) = line.split_once('\t').ok_or_else(|| {
                format!(
                    "{}:{} must contain a tab-separated Python and Rust test ID",
                    path.display(),
                    index + 1
                )
            })?;
            Ok(Mapping {
                python_test: python_test.to_owned(),
                rust_test: rust_test.to_owned(),
            })
        })
        .collect()
}

fn fnv1a64_lines<'a>(lines: impl Iterator<Item = &'a str>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for line in lines {
        for byte in line.bytes().chain(std::iter::once(b'\n')) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_fingerprint_is_stable() {
        assert_eq!(fnv1a64_lines(["a", "b"].into_iter()), 0x78ed_6781_f136_a14e);
    }

    #[test]
    fn unmapped_report_is_sorted_by_count_then_module() {
        let python_tests = [
            "tests/test_b.py::test_two".to_owned(),
            "tests/test_a.py::test_one".to_owned(),
            "tests/test_b.py::test_one".to_owned(),
            "tests/test_c.py::test_one".to_owned(),
        ]
        .into_iter()
        .collect();
        let mapped_python = ["tests/test_c.py::test_one"].into_iter().collect();
        assert_eq!(
            unmapped_module_counts(&python_tests, &mapped_python),
            vec![
                ("tests/test_b.py".to_owned(), 2),
                ("tests/test_a.py".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn cutover_readiness_requires_every_named_operational_gate() {
        let manifest = REQUIRED_CUTOVER_GATES
            .iter()
            .map(|gate| format!("{gate}\topen\tpending proof"))
            .collect::<Vec<_>>()
            .join("\n");
        let gates = parse_cutover_readiness(&manifest, "test manifest")
            .expect("complete readiness inventory");

        assert_eq!(gates.len(), REQUIRED_CUTOVER_GATES.len());
        assert!(
            gates
                .iter()
                .all(|gate| gate.status == ReadinessStatus::Open)
        );

        let incomplete = manifest.lines().skip(1).collect::<Vec<_>>().join("\n");
        let error = parse_cutover_readiness(&incomplete, "test manifest")
            .expect_err("missing readiness gate must fail");
        assert!(error.contains(REQUIRED_CUTOVER_GATES[0]));
    }

    #[test]
    fn cutover_readiness_rejects_invalid_status_and_empty_evidence() {
        let manifest = REQUIRED_CUTOVER_GATES
            .iter()
            .map(|gate| format!("{gate}\topen\tpending proof"))
            .collect::<Vec<_>>()
            .join("\n");
        let invalid_status = manifest.replacen("\topen\t", "\twaived\t", 1);
        assert!(
            parse_cutover_readiness(&invalid_status, "test manifest")
                .expect_err("unknown status must fail")
                .contains("expected open or complete")
        );

        let empty_evidence = manifest.replacen("\topen\tpending proof", "\topen\t", 1);
        assert!(
            parse_cutover_readiness(&empty_evidence, "test manifest")
                .expect_err("empty evidence must fail")
                .contains("must explain")
        );
    }
}
