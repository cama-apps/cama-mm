//! Migrated-SQLite parity harness for `tests/test_dig_leaderboard_service.py`.

use cama_app::dig_leaderboard_runtime::DigLeaderboardRuntime;
use cama_db::schema_manager::initialize_or_migrate;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

const GUILD: i64 = 42;

struct TestDb {
    file: NamedTempFile,
}

impl TestDb {
    fn new() -> Self {
        let file = NamedTempFile::new().expect("temporary migrated database");
        initialize_or_migrate(file.path()).expect("canonical migration");
        Self { file }
    }

    fn runtime(&self) -> DigLeaderboardRuntime {
        DigLeaderboardRuntime::new(self.file.path())
    }

    fn connection(&self) -> Connection {
        Connection::open(self.file.path()).expect("open migrated database")
    }

    fn tunnel(&self, discord_id: i64, guild_id: i64, name: &str, values: TunnelValues) {
        self.connection()
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,tunnel_name,depth,total_digs,total_jc_earned,
                  prestige_level,best_run_score)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    discord_id,
                    guild_id,
                    name,
                    values.depth,
                    values.total_digs,
                    values.total_jc_earned,
                    values.prestige_level,
                    values.best_run_score,
                ],
            )
            .expect("seed tunnel");
    }

    fn artifact(&self, discord_id: i64, guild_id: i64, artifact_id: &str) {
        self.connection()
            .execute(
                "INSERT INTO dig_artifacts
                 (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
                 VALUES (?1,?2,?3,?4,1,0)",
                params![discord_id, guild_id, artifact_id, 1_700_000_000_i64],
            )
            .expect("seed artifact");
    }

    fn equipped_relic(&self, discord_id: i64, guild_id: i64, artifact_id: &str) {
        self.connection()
            .execute(
                "INSERT INTO dig_artifacts
                 (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
                 VALUES (?1,?2,?3,?4,1,1)",
                params![discord_id, guild_id, artifact_id, 1_700_000_000_i64],
            )
            .expect("seed equipped relic");
    }
}

#[derive(Clone, Copy)]
struct TunnelValues {
    depth: i64,
    total_digs: i64,
    total_jc_earned: i64,
    prestige_level: i64,
    best_run_score: i64,
}

impl TunnelValues {
    const fn depth(depth: i64) -> Self {
        Self {
            depth,
            total_digs: 0,
            total_jc_earned: 0,
            prestige_level: 0,
            best_run_score: 0,
        }
    }
}

#[test]
fn empty_guild_has_no_rows_or_art() {
    let db = TestDb::new();
    let result = db.runtime().leaderboard(GUILD).expect("leaderboard");
    assert!(result.tunnels.is_empty());
    assert_eq!(result.ascii_art, "");
}

#[test]
fn tunnels_order_by_prestige_then_depth() {
    let db = TestDb::new();
    db.tunnel(1, GUILD, "Shallow", TunnelValues::depth(10));
    db.tunnel(2, GUILD, "Deep", TunnelValues::depth(500));
    db.tunnel(
        3,
        GUILD,
        "Prestiged",
        TunnelValues {
            prestige_level: 2,
            ..TunnelValues::depth(5)
        },
    );
    let ids = db
        .runtime()
        .leaderboard(GUILD)
        .expect("leaderboard")
        .tunnels
        .into_iter()
        .map(|tunnel| tunnel.discord_id)
        .collect::<Vec<_>>();
    assert_eq!(ids, [3, 2, 1]);
}

#[test]
fn ascii_bar_scales_to_deepest_tunnel() {
    let db = TestDb::new();
    db.tunnel(1, GUILD, "Deepest", TunnelValues::depth(100));
    db.tunnel(2, GUILD, "Half", TunnelValues::depth(50));
    let lines = db
        .runtime()
        .leaderboard(GUILD)
        .expect("leaderboard")
        .ascii_art
        .split('\n')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].matches('█').count(), 40);
    assert_eq!(lines[1].matches('█').count(), 20);
    assert!(lines[0].ends_with("100m"));
    assert!(lines[1].ends_with("50m"));
}

#[test]
fn ascii_bar_has_minimum_length_one() {
    let db = TestDb::new();
    db.tunnel(1, GUILD, "Brand New", TunnelValues::depth(0));
    let line = db
        .runtime()
        .leaderboard(GUILD)
        .expect("leaderboard")
        .ascii_art;
    assert_eq!(line.matches('█').count(), 1);
    assert!(line.ends_with("0m"));
}

#[test]
fn long_tunnel_name_is_truncated_to_fifteen_chars() {
    let db = TestDb::new();
    db.tunnel(1, GUILD, &"A".repeat(40), TunnelValues::depth(10));
    let line = db
        .runtime()
        .leaderboard(GUILD)
        .expect("leaderboard")
        .ascii_art;
    assert!(!line.contains(&"A".repeat(40)));
    assert!(line.contains(&"A".repeat(15)));
}

#[test]
fn hall_of_fame_excludes_zero_scores() {
    let db = TestDb::new();
    db.tunnel(
        1,
        GUILD,
        "Never Prestiged",
        TunnelValues {
            best_run_score: 0,
            ..TunnelValues::depth(0)
        },
    );
    let result = db.runtime().hall_of_fame(GUILD).expect("hall of fame");
    assert!(result.success);
    assert!(result.entries.is_empty());
}

#[test]
fn hall_of_fame_entries_are_score_descending() {
    let db = TestDb::new();
    for (id, name, score) in [(1, "Low", 100), (2, "High", 900), (3, "Mid", 400)] {
        db.tunnel(
            id,
            GUILD,
            name,
            TunnelValues {
                best_run_score: score,
                ..TunnelValues::depth(0)
            },
        );
    }
    let entries = db
        .runtime()
        .hall_of_fame(GUILD)
        .expect("hall of fame")
        .entries;
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.best_run_score)
            .collect::<Vec<_>>(),
        [900, 400, 100]
    );
    assert_eq!(entries[0].tunnel_name.as_deref(), Some("High"));
}

#[test]
fn hall_of_fame_entry_carries_display_fields() {
    let db = TestDb::new();
    db.tunnel(
        42,
        GUILD,
        "Champion",
        TunnelValues {
            prestige_level: 3,
            best_run_score: 500,
            ..TunnelValues::depth(0)
        },
    );
    let entry = &db
        .runtime()
        .hall_of_fame(GUILD)
        .expect("hall of fame")
        .entries[0];
    assert_eq!(entry.discord_id, 42);
    assert_eq!(entry.tunnel_name.as_deref(), Some("Champion"));
    assert_eq!(entry.prestige_level, 3);
    assert_eq!(entry.best_run_score, 500);
}

#[test]
fn empty_collection_has_zero_total() {
    let db = TestDb::new();
    let result = db.runtime().collection(9_999, GUILD).expect("collection");
    assert!(result.artifacts.is_empty());
    assert_eq!(result.total, 0);
}

#[test]
fn collection_groups_by_catalog_rarity() {
    let db = TestDb::new();
    db.artifact(1, GUILD, "mole_claws");
    db.artifact(1, GUILD, "crystal_compass");
    let result = db.runtime().collection(1, GUILD).expect("collection");
    assert_eq!(result.total, 2);
    assert_eq!(
        result.artifacts.keys().cloned().collect::<Vec<_>>(),
        ["Common", "Uncommon"]
    );
    assert_eq!(result.artifacts["Common"][0].artifact_id, "crystal_compass");
    assert_eq!(result.artifacts["Uncommon"][0].artifact_id, "mole_claws");
}

#[test]
fn collection_is_player_scoped() {
    let db = TestDb::new();
    db.artifact(1, GUILD, "mole_claws");
    db.artifact(2, GUILD, "crystal_compass");
    let result = db.runtime().collection(1, GUILD).expect("collection");
    assert_eq!(result.total, 1);
    assert_eq!(result.artifacts["Uncommon"][0].discord_id, 1);
}

#[test]
fn empty_guild_stats_are_zeroed() {
    let db = TestDb::new();
    let result = db.runtime().guild_stats(GUILD).expect("guild stats");
    assert!(result.success);
    assert_eq!(result.total_digs, 0);
    assert_eq!(result.total_depth, 0);
    assert_eq!(result.total_jc_earned, 0);
    assert_eq!(result.tunnel_count, 0);
    assert!(result.most_active.is_none());
    assert!(result.deepest.is_none());
}

#[test]
fn guild_stats_sum_across_tunnels() {
    let db = TestDb::new();
    db.tunnel(
        1,
        GUILD,
        "A",
        TunnelValues {
            total_digs: 10,
            depth: 100,
            total_jc_earned: 50,
            ..TunnelValues::depth(100)
        },
    );
    db.tunnel(
        2,
        GUILD,
        "B",
        TunnelValues {
            total_digs: 5,
            depth: 200,
            total_jc_earned: 75,
            ..TunnelValues::depth(200)
        },
    );
    let result = db.runtime().guild_stats(GUILD).expect("guild stats");
    assert_eq!(result.tunnel_count, 2);
    assert_eq!(result.total_digs, 15);
    assert_eq!(result.total_depth, 300);
    assert_eq!(result.total_jc_earned, 125);
}

#[test]
fn guild_stats_choose_most_active_and_deepest() {
    let db = TestDb::new();
    db.tunnel(
        1,
        GUILD,
        "Busy",
        TunnelValues {
            total_digs: 99,
            depth: 10,
            ..TunnelValues::depth(10)
        },
    );
    db.tunnel(
        2,
        GUILD,
        "Profound",
        TunnelValues {
            total_digs: 2,
            depth: 999,
            ..TunnelValues::depth(999)
        },
    );
    let result = db.runtime().guild_stats(GUILD).expect("guild stats");
    let most_active = result.most_active.expect("most active");
    assert_eq!(most_active.discord_id, 1);
    assert_eq!(most_active.name.as_deref(), Some("Busy"));
    assert_eq!(most_active.total_digs, 99);
    let deepest = result.deepest.expect("deepest");
    assert_eq!(deepest.discord_id, 2);
    assert_eq!(deepest.name.as_deref(), Some("Profound"));
    assert_eq!(deepest.depth, 999);
}

#[test]
fn guild_stats_are_isolated_by_guild() {
    let db = TestDb::new();
    db.tunnel(
        1,
        GUILD,
        "Ours",
        TunnelValues {
            total_digs: 10,
            depth: 100,
            ..TunnelValues::depth(100)
        },
    );
    db.tunnel(
        2,
        99_999,
        "Theirs",
        TunnelValues {
            total_digs: 999,
            depth: 9_999,
            ..TunnelValues::depth(9_999)
        },
    );
    let result = db.runtime().guild_stats(GUILD).expect("guild stats");
    assert_eq!(result.tunnel_count, 1);
    assert_eq!(result.total_digs, 10);
    assert_eq!(result.deepest.expect("deepest").discord_id, 1);
}

#[test]
fn leaderboard_display_higher_prestige_outranks_higher_depth() {
    let db = TestDb::new();
    db.tunnel(1, GUILD, "T1", TunnelValues::depth(250));
    db.tunnel(
        2,
        GUILD,
        "T2",
        TunnelValues {
            depth: 100,
            prestige_level: 3,
            ..TunnelValues::depth(100)
        },
    );
    db.tunnel(
        3,
        GUILD,
        "T3",
        TunnelValues {
            depth: 300,
            prestige_level: 1,
            ..TunnelValues::depth(300)
        },
    );
    let ids = db
        .runtime()
        .leaderboard(GUILD)
        .expect("leaderboard")
        .tunnels
        .into_iter()
        .map(|row| row.discord_id)
        .collect::<Vec<_>>();
    assert_eq!(ids, [2, 3, 1]);
}

#[test]
fn leaderboard_display_depth_breaks_ties_at_same_prestige() {
    let db = TestDb::new();
    for (id, depth) in [(1, 50), (2, 200), (3, 125)] {
        db.tunnel(
            id,
            GUILD,
            &format!("T{id}"),
            TunnelValues {
                depth,
                prestige_level: 2,
                ..TunnelValues::depth(depth)
            },
        );
    }
    let ids = db
        .runtime()
        .leaderboard(GUILD)
        .expect("leaderboard")
        .tunnels
        .into_iter()
        .map(|row| row.discord_id)
        .collect::<Vec<_>>();
    assert_eq!(ids, [2, 3, 1]);
}

#[test]
fn leaderboard_display_player_rank_matches_top_tunnels() {
    let db = TestDb::new();
    db.tunnel(1, GUILD, "T1", TunnelValues::depth(250));
    db.tunnel(
        2,
        GUILD,
        "T2",
        TunnelValues {
            prestige_level: 3,
            ..TunnelValues::depth(100)
        },
    );
    db.tunnel(
        3,
        GUILD,
        "T3",
        TunnelValues {
            prestige_level: 1,
            ..TunnelValues::depth(300)
        },
    );
    let runtime = db.runtime();
    assert_eq!(runtime.player_rank(2, GUILD).expect("rank"), Some(1));
    assert_eq!(runtime.player_rank(3, GUILD).expect("rank"), Some(2));
    assert_eq!(runtime.player_rank(1, GUILD).expect("rank"), Some(3));
}

#[test]
fn leaderboard_display_full_ties_break_by_discord_id() {
    let db = TestDb::new();
    for id in [5, 7, 9] {
        db.tunnel(
            id,
            GUILD,
            &format!("T{id}"),
            TunnelValues {
                prestige_level: 2,
                ..TunnelValues::depth(120)
            },
        );
    }
    let runtime = db.runtime();
    let ids = runtime
        .leaderboard(GUILD)
        .expect("leaderboard")
        .tunnels
        .into_iter()
        .map(|row| row.discord_id)
        .collect::<Vec<_>>();
    assert_eq!(ids, [5, 7, 9]);
    assert_eq!(runtime.player_rank(5, GUILD).expect("rank"), Some(1));
    assert_eq!(runtime.player_rank(7, GUILD).expect("rank"), Some(2));
    assert_eq!(runtime.player_rank(9, GUILD).expect("rank"), Some(3));
}

#[test]
fn dig_info_equipped_relics_carry_artifact_id() {
    let db = TestDb::new();
    db.tunnel(99_001, GUILD, "Vault", TunnelValues::depth(0));
    db.equipped_relic(99_001, GUILD, "mole_claws");
    db.equipped_relic(99_001, GUILD, "crystal_compass");
    let mut ids = db
        .runtime()
        .equipped_relics(99_001, GUILD)
        .expect("equipped relics")
        .into_iter()
        .map(|row| row.artifact_id)
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, ["crystal_compass", "mole_claws"]);
}
