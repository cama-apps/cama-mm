use std::cell::{Cell, RefCell};

use thiserror::Error;

use super::*;

const ACTOR: i64 = 501;
const GUILD: i64 = 9;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{0}")]
struct FakePortError(String);

#[derive(Debug)]
struct FakePort {
    tunnel_exists: Cell<bool>,
    rows: RefCell<Vec<OwnedRelicRow>>,
    persisted_raw: RefCell<Vec<String>>,
    fail_atomic: Cell<bool>,
    next_row_id: Cell<i64>,
}

impl Default for FakePort {
    fn default() -> Self {
        Self {
            tunnel_exists: Cell::new(true),
            rows: RefCell::new(Vec::new()),
            persisted_raw: RefCell::new(Vec::new()),
            fail_atomic: Cell::new(false),
            next_row_id: Cell::new(1),
        }
    }
}

impl FakePort {
    fn add(&self, artifact_id: &str) -> i64 {
        self.add_with_state(artifact_id, true, false)
    }

    fn add_with_state(&self, artifact_id: &str, is_relic: bool, equipped: bool) -> i64 {
        let row_id = self.next_row_id.get();
        self.next_row_id.set(row_id + 1);
        self.rows.borrow_mut().push(OwnedRelicRow {
            row_id,
            artifact_id: artifact_id.to_owned(),
            is_relic,
            equipped,
        });
        row_id
    }

    fn artifact_ids(&self) -> Vec<String> {
        self.rows
            .borrow()
            .iter()
            .map(|row| row.artifact_id.clone())
            .collect()
    }
}

impl RawRelicFindPort for FakePort {
    type Error = FakePortError;

    fn tunnel_exists(&self, _discord_id: i64, _guild_id: i64) -> Result<bool, Self::Error> {
        Ok(self.tunnel_exists.get())
    }

    fn owned_artifact_ids(
        &self,
        _discord_id: i64,
        _guild_id: i64,
    ) -> Result<BTreeSet<String>, Self::Error> {
        Ok(self.artifact_ids().into_iter().collect())
    }

    fn persist_raw_relic(
        &self,
        _discord_id: i64,
        _guild_id: i64,
        artifact_id: &str,
    ) -> Result<(), Self::Error> {
        self.persisted_raw.borrow_mut().push(artifact_id.to_owned());
        self.add(artifact_id);
        Ok(())
    }
}

impl RelicRecyclePort for FakePort {
    type Error = FakePortError;

    fn owned_relic_rows(
        &self,
        _discord_id: i64,
        _guild_id: i64,
    ) -> Result<Vec<OwnedRelicRow>, Self::Error> {
        Ok(self.rows.borrow().clone())
    }

    fn atomic_recycle(
        &self,
        _discord_id: i64,
        _guild_id: i64,
        selected_row_ids: &[i64],
        output_artifact_id: &str,
        _found_at: i64,
    ) -> Result<i64, Self::Error> {
        if self.fail_atomic.get() {
            return Err(FakePortError(
                "Expected exactly three eligible relic rows.".to_owned(),
            ));
        }
        let selected = selected_row_ids.iter().copied().collect::<BTreeSet<_>>();
        let eligible_count = self
            .rows
            .borrow()
            .iter()
            .filter(|row| selected.contains(&row.row_id) && row.is_relic && !row.equipped)
            .count();
        if selected.len() != 3 || eligible_count != 3 {
            return Err(FakePortError(
                "Expected exactly three eligible relic rows.".to_owned(),
            ));
        }
        self.rows
            .borrow_mut()
            .retain(|row| !selected.contains(&row.row_id));
        Ok(self.add(output_artifact_id))
    }
}

fn rarity_name(rarity: Rarity) -> &'static str {
    match rarity {
        Rarity::Common => "Common",
        Rarity::Uncommon => "Uncommon",
        Rarity::Rare => "Rare",
        Rarity::Legendary => "Legendary",
    }
}

fn assert_rarity_roll(roll: f64, expected: Rarity) {
    assert_eq!(relic_rarity_for_roll(roll), Ok(expected));
}

fn assert_raw_find_rarity(rarity_roll: f64, expected: Rarity) {
    let service = DigRelicRecyclingService::new(FakePort::default());
    let mut entropy = ScriptedRelicEntropy::new([0.0, rarity_roll], [0]);
    let result = service
        .roll_raw_relic_find(
            RawRelicFindRequest {
                discord_id: ACTOR,
                guild_id: GUILD,
                depth: 160,
                extra_rate_mod: 1.0,
            },
            &mut entropy,
        )
        .expect("raw relic roll")
        .expect("relic found");
    assert_eq!(result.rarity, expected);
    let definition = artifact_catalog()
        .into_iter()
        .find(|artifact| artifact.id == result.artifact_id)
        .expect("selected catalog relic");
    assert_eq!(definition.rarity, expected);
    assert!(is_ordinary_relic(definition));
    assert_eq!(
        service.port().persisted_raw.borrow().as_slice(),
        [result.artifact_id]
    );
}

fn common_rows(port: &FakePort) -> Vec<i64> {
    ["crystal_compass", "obsidian_shield", "mycelium_link"]
        .into_iter()
        .map(|artifact_id| port.add(artifact_id))
        .collect()
}

#[test]
fn test_existing_ordinary_relics_are_reclassified_across_four_tiers() {
    let expected = BTreeMap::from([
        (
            Rarity::Common,
            BTreeSet::from([
                "crystal_compass",
                "obsidian_shield",
                "mycelium_link",
                "midas_splinter",
                "lucky_seam",
            ]),
        ),
        (
            Rarity::Uncommon,
            BTreeSet::from([
                "mole_claws",
                "magma_heart",
                "root_network",
                "mana_conduit",
                "mentors_lantern",
                "prospectors_streak",
            ]),
        ),
        (
            Rarity::Rare,
            BTreeSet::from([
                "echo_stone",
                "spore_cloak",
                "frozen_clock",
                "gamblers_charm",
                "stormcaller",
                "slow_drip",
            ]),
        ),
        (
            Rarity::Legendary,
            BTreeSet::from(["hollow_eye", "prism_heart", "bloodstone", "vendetta_coin"]),
        ),
    ]);
    assert_eq!(
        RELIC_RARITY_ORDER.map(rarity_name),
        ["Common", "Uncommon", "Rare", "Legendary"]
    );
    let catalog = artifact_catalog();
    for (rarity, ids) in expected {
        assert!(ids.iter().all(|artifact_id| {
            catalog
                .iter()
                .find(|artifact| artifact.id == *artifact_id)
                .is_some_and(|artifact| artifact.rarity == rarity)
        }));
    }
}

#[test]
fn test_new_relic_catalog_has_two_relics_per_rarity() {
    let expected = BTreeMap::from([
        ("chipped_compass", Rarity::Common),
        ("lantern_stub", Rarity::Common),
        ("paper_crane", Rarity::Uncommon),
        ("bone_abacus", Rarity::Uncommon),
        ("bottled_quake", Rarity::Rare),
        ("black_wax_seal", Rarity::Rare),
        ("burning_ledger", Rarity::Legendary),
        ("shifting_idol", Rarity::Legendary),
    ]);
    let catalog = artifact_catalog();
    for (artifact_id, rarity) in &expected {
        let relic = catalog
            .iter()
            .find(|artifact| artifact.id == *artifact_id)
            .expect("new relic");
        assert!(relic.is_relic);
        assert_eq!(relic.rarity, *rarity);
    }
    for rarity in RELIC_RARITY_ORDER {
        assert_eq!(
            expected
                .values()
                .filter(|candidate| **candidate == rarity)
                .count(),
            2
        );
    }
}

#[test]
fn test_relic_rarity_roll_uses_60_30_9_1_weights_0_0_common() {
    assert_rarity_roll(0.0, Rarity::Common);
}

#[test]
fn test_relic_rarity_roll_uses_60_30_9_1_weights_0_599999_common() {
    assert_rarity_roll(0.599_999, Rarity::Common);
}

#[test]
fn test_relic_rarity_roll_uses_60_30_9_1_weights_0_6_uncommon() {
    assert_rarity_roll(0.60, Rarity::Uncommon);
}

#[test]
fn test_relic_rarity_roll_uses_60_30_9_1_weights_0_899999_uncommon() {
    assert_rarity_roll(0.899_999, Rarity::Uncommon);
}

#[test]
fn test_relic_rarity_roll_uses_60_30_9_1_weights_0_9_rare() {
    assert_rarity_roll(0.90, Rarity::Rare);
}

#[test]
fn test_relic_rarity_roll_uses_60_30_9_1_weights_0_989999_rare() {
    assert_rarity_roll(0.989_999, Rarity::Rare);
}

#[test]
fn test_relic_rarity_roll_uses_60_30_9_1_weights_0_99_legendary() {
    assert_rarity_roll(0.99, Rarity::Legendary);
}

#[test]
fn test_relic_rarity_roll_uses_60_30_9_1_weights_0_999999_legendary() {
    assert_rarity_roll(0.999_999, Rarity::Legendary);
}

#[test]
fn test_raw_relic_find_uses_weighted_ordinary_rarity_pool_0_1_common() {
    assert_raw_find_rarity(0.10, Rarity::Common);
}

#[test]
fn test_raw_relic_find_uses_weighted_ordinary_rarity_pool_0_7_uncommon() {
    assert_raw_find_rarity(0.70, Rarity::Uncommon);
}

#[test]
fn test_raw_relic_find_uses_weighted_ordinary_rarity_pool_0_95_rare() {
    assert_raw_find_rarity(0.95, Rarity::Rare);
}

#[test]
fn test_raw_relic_find_uses_weighted_ordinary_rarity_pool_0_995_legendary() {
    assert_raw_find_rarity(0.995, Rarity::Legendary);
}

#[test]
fn test_ordinary_relic_excludes_progression_and_boss_exclusives() {
    let catalog = artifact_catalog();
    let by_id = |artifact_id: &str| {
        catalog
            .iter()
            .find(|artifact| artifact.id == artifact_id)
            .copied()
            .expect("catalog artifact")
    };
    assert!(is_ordinary_relic(by_id("crystal_compass")));
    assert!(!is_ordinary_relic(by_id("first_light")));
    assert!(!is_ordinary_relic(by_id("weeping_fang")));
    assert!(!is_ordinary_relic(by_id("aegis_fragment")));
}

#[test]
fn test_recycle_three_common_relics_into_random_uncommon() {
    let port = FakePort::default();
    let rows = common_rows(&port);
    let service = DigRelicRecyclingService::new(port);
    let result = service.recycle_relics(
        ACTOR,
        GUILD,
        &rows,
        123,
        &mut ScriptedRelicEntropy::choosing(0),
    );
    assert!(result.success);
    assert_eq!(result.source_rarity, Some(Rarity::Common));
    assert_eq!(result.output_rarity, Some(Rarity::Uncommon));
    assert_eq!(result.relic_id, Some("mole_claws"));
    assert_eq!(service.port().artifact_ids(), ["mole_claws"]);
}

#[test]
fn test_recycle_allows_duplicate_random_output() {
    let port = FakePort::default();
    port.add("mole_claws");
    let rows = common_rows(&port);
    let service = DigRelicRecyclingService::new(port);
    let result = service.recycle_relics(
        ACTOR,
        GUILD,
        &rows,
        123,
        &mut ScriptedRelicEntropy::choosing(0),
    );
    assert!(result.success);
    assert_eq!(
        service
            .port()
            .artifact_ids()
            .iter()
            .filter(|artifact_id| artifact_id.as_str() == "mole_claws")
            .count(),
        2
    );
}

#[test]
fn test_recycle_rejects_invalid_sets_artifact_ids0_exactly_three() {
    let port = FakePort::default();
    let rows = [port.add("crystal_compass"), port.add("obsidian_shield")];
    let service = DigRelicRecyclingService::new(port);
    let result = service.recycle_relics(
        ACTOR,
        GUILD,
        &rows,
        123,
        &mut ScriptedRelicEntropy::choosing(0),
    );
    assert!(!result.success);
    assert!(
        result
            .error
            .unwrap()
            .to_ascii_lowercase()
            .contains("exactly three")
    );
    assert_eq!(service.port().artifact_ids().len(), 2);
}

#[test]
fn test_recycle_rejects_invalid_sets_artifact_ids1_same_rarity() {
    let port = FakePort::default();
    let rows = [
        port.add("crystal_compass"),
        port.add("obsidian_shield"),
        port.add("mole_claws"),
    ];
    let service = DigRelicRecyclingService::new(port);
    let result = service.recycle_relics(
        ACTOR,
        GUILD,
        &rows,
        123,
        &mut ScriptedRelicEntropy::choosing(0),
    );
    assert!(!result.success);
    assert!(
        result
            .error
            .unwrap()
            .to_ascii_lowercase()
            .contains("same rarity")
    );
    assert_eq!(service.port().artifact_ids().len(), 3);
}

#[test]
fn test_recycle_rejects_invalid_sets_artifact_ids2_legendary() {
    let port = FakePort::default();
    let rows = [
        port.add("hollow_eye"),
        port.add("prism_heart"),
        port.add("bloodstone"),
    ];
    let service = DigRelicRecyclingService::new(port);
    let result = service.recycle_relics(
        ACTOR,
        GUILD,
        &rows,
        123,
        &mut ScriptedRelicEntropy::choosing(0),
    );
    assert!(!result.success);
    assert!(result.error.unwrap().contains("Legendary"));
    assert_eq!(service.port().artifact_ids().len(), 3);
}

#[test]
fn test_recycle_rejects_invalid_sets_artifact_ids3_progression() {
    let port = FakePort::default();
    let rows = [
        port.add("first_light"),
        port.add("berserkers_mark"),
        port.add("gamblers_edge"),
    ];
    let service = DigRelicRecyclingService::new(port);
    let result = service.recycle_relics(
        ACTOR,
        GUILD,
        &rows,
        123,
        &mut ScriptedRelicEntropy::choosing(0),
    );
    assert!(!result.success);
    assert!(
        result
            .error
            .unwrap()
            .to_ascii_lowercase()
            .contains("progression")
    );
    assert_eq!(service.port().artifact_ids().len(), 3);
}

#[test]
fn test_recycle_rejects_equipped_input_without_consuming_anything() {
    let port = FakePort::default();
    let rows = [
        port.add_with_state("crystal_compass", true, true),
        port.add("obsidian_shield"),
        port.add("mycelium_link"),
    ];
    let service = DigRelicRecyclingService::new(port);
    let result = service.recycle_relics(
        ACTOR,
        GUILD,
        &rows,
        123,
        &mut ScriptedRelicEntropy::choosing(0),
    );
    assert!(!result.success);
    assert!(
        result
            .error
            .unwrap()
            .to_ascii_lowercase()
            .contains("unequipped")
    );
    assert_eq!(service.port().artifact_ids().len(), 3);
}

#[test]
fn invalid_nan_and_upper_bound_rolls_are_rejected() {
    assert_eq!(
        relic_rarity_for_roll(f64::NAN),
        Err(RelicFindError::InvalidUnitRoll)
    );
    assert_eq!(
        relic_rarity_for_roll(1.0),
        Err(RelicFindError::InvalidUnitRoll)
    );
}

#[test]
fn raw_find_excludes_owned_relics_and_returns_none_when_rarity_pool_is_exhausted() {
    let port = FakePort::default();
    for artifact in artifact_catalog()
        .into_iter()
        .filter(|artifact| is_ordinary_relic(*artifact) && artifact.rarity == Rarity::Common)
    {
        port.add(artifact.id);
    }
    let service = DigRelicRecyclingService::new(port);
    let mut entropy = ScriptedRelicEntropy::new([0.0, 0.1], [0]);
    assert_eq!(
        service
            .roll_raw_relic_find(
                RawRelicFindRequest {
                    discord_id: ACTOR,
                    guild_id: GUILD,
                    depth: 10,
                    extra_rate_mod: 1.0,
                },
                &mut entropy,
            )
            .expect("exhausted roll"),
        None
    );
}

#[test]
fn stale_between_policy_read_and_atomic_commit_is_reported_without_fake_consumption() {
    let port = FakePort::default();
    let rows = common_rows(&port);
    port.fail_atomic.set(true);
    let service = DigRelicRecyclingService::new(port);
    let result = service.recycle_relics(
        ACTOR,
        GUILD,
        &rows,
        123,
        &mut ScriptedRelicEntropy::choosing(0),
    );
    assert!(!result.success);
    assert!(result.error.unwrap().contains("eligible relic rows"));
    assert_eq!(service.port().artifact_ids().len(), 3);
}
