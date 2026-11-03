use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::test_support::copy_migrated_database;

const GUILD: i64 = 42;
const OTHER_GUILD: i64 = 77;
const ME: i64 = 100;
const FRIEND: i64 = 200;
const RIVAL: i64 = 300;
/// 2026-01-01T00:00:00Z .. 2027-01-01T00:00:00Z
const YEAR: YearWindow = YearWindow {
    start: 1_767_225_600,
    end: 1_798_761_600,
};
const MARCH: i64 = 1_772_323_200;
const LAST_YEAR: i64 = YEAR.start - 86_400;

struct Fixture {
    _file: NamedTempFile,
    connection: Connection,
    repository: WrappedActivityRepository,
}

fn fixture() -> Fixture {
    let file = NamedTempFile::new().unwrap();
    copy_migrated_database(file.path()).unwrap();
    let connection = Connection::open(file.path()).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    let repository = WrappedActivityRepository::new(file.path());
    Fixture {
        _file: file,
        connection,
        repository,
    }
}

impl Fixture {
    #[allow(clippy::too_many_arguments)]
    fn dig(
        &self,
        guild: i64,
        actor: i64,
        target: Option<i64>,
        kind: &str,
        depth: (i64, i64),
        jc: i64,
        detail: &str,
        at: i64,
    ) {
        self.connection
            .execute(
                "INSERT INTO dig_actions(guild_id,actor_id,target_id,action_type,depth_before,
                     depth_after,jc_delta,detail,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![guild, actor, target, kind, depth.0, depth.1, jc, detail, at],
            )
            .unwrap();
    }

    fn mafia_game(&self, game: i64, guild: i64, winner: &str, mvp: Option<i64>, at: i64) {
        self.mafia_game_with(game, guild, winner, mvp, at, "RESOLVED", "ACTIVE");
    }

    #[allow(clippy::too_many_arguments)]
    fn mafia_game_with(
        &self,
        game: i64,
        guild: i64,
        winner: &str,
        mvp: Option<i64>,
        at: i64,
        phase: &str,
        status: &str,
    ) {
        self.connection
            .execute(
                "INSERT INTO mafia_games(game_id,guild_id,game_date,phase,started_at,winner,
                     payout_per_winner,mvp_id,roster_size,status)
                 VALUES(?1,?2,'2026-03-01',?3,?4,?5,25,?6,6,?7)",
                params![game, guild, phase, at, winner, mvp, status],
            )
            .unwrap();
    }

    fn mafia_player(&self, game: i64, who: i64, role: &str, eliminated: Option<&str>) {
        self.connection
            .execute(
                "INSERT INTO mafia_players(game_id,discord_id,guild_id,role,hero_name,is_alive,
                     eliminated_phase)
                 VALUES(?1,?2,?3,?4,'Pudge',?5,?6)",
                params![
                    game,
                    who,
                    GUILD,
                    role,
                    i64::from(eliminated.is_none()),
                    eliminated
                ],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn mafia_action(&self, game: i64, actor: i64, target: i64, kind: &str, phase: &str, day: i64) {
        self.connection
            .execute(
                "INSERT INTO mafia_actions(game_id,guild_id,actor_id,target_id,action_type,phase,
                     day_number,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![game, GUILD, actor, target, kind, phase, day, MARCH],
            )
            .unwrap();
    }
}

#[test]
fn empty_year_is_all_zero_for_every_activity() {
    let fixture = fixture();
    let repository = &fixture.repository;
    assert_eq!(
        repository.dig_year(GUILD, ME, YEAR).unwrap(),
        DigYear::default()
    );
    assert_eq!(
        repository.pet_year(GUILD, ME, YEAR).unwrap(),
        PetYear::default()
    );
    assert_eq!(
        repository.mafia_year(GUILD, ME, YEAR).unwrap(),
        MafiaYear::default()
    );
    let (wheel, tips, rank) = repository.gamba_side_year(GUILD, ME, YEAR).unwrap();
    assert_eq!(wheel, WheelYear::default());
    assert_eq!(tips, TipYear::default());
    assert_eq!(rank, None);
}

#[test]
fn dig_year_counts_only_this_guild_this_year() {
    let f = fixture();
    let cave = r#"{"cave_in":true,"jc":0}"#;
    let event = r#"{"event":"abandoned_forge","jc":4}"#;
    f.dig(GUILD, ME, None, "dig", (0, 5), 3, r#"{"jc":3}"#, MARCH);
    f.dig(GUILD, ME, None, "dig", (5, 2), 0, cave, MARCH + 60);
    f.dig(
        GUILD,
        ME,
        None,
        "paid_dig",
        (2, 9),
        4,
        event,
        MARCH + 86_400,
    );
    f.dig(GUILD, ME, None, "dig", (9, 30), 9, "not json", LAST_YEAR);
    f.dig(OTHER_GUILD, ME, None, "dig", (0, 99), 9, "{}", MARCH);
    f.dig(GUILD, FRIEND, None, "dig", (0, 1), 1, "{}", MARCH);
    let won = r#"{"boss_id":"grothak","won":true}"#;
    let lost = r#"{"boss_id":"grothak","won":false}"#;
    for detail in [won, lost, lost] {
        f.dig(GUILD, ME, None, "boss_fight", (9, 9), 0, detail, MARCH);
    }
    f.dig(
        GUILD,
        ME,
        None,
        "pinnacle_fight",
        (9, 9),
        0,
        r#"{"boss_id":"lina","won":true}"#,
        MARCH,
    );
    f.dig(
        GUILD,
        ME,
        None,
        "boss_fight",
        (9, 9),
        0,
        r#"{"won":true}"#,
        MARCH,
    );
    for _ in 0..2 {
        f.dig(GUILD, ME, Some(RIVAL), "sabotage", (0, 0), -5, "{}", MARCH);
    }
    f.dig(GUILD, ME, Some(FRIEND), "sabotage", (0, 0), -5, "{}", MARCH);
    f.dig(GUILD, RIVAL, Some(ME), "sabotage", (0, 0), -5, "{}", MARCH);
    f.dig(GUILD, FRIEND, Some(ME), "help", (0, 4), 1, "{}", MARCH);
    f.connection
        .execute_batch(&format!(
            "INSERT INTO dig_artifacts(discord_id,guild_id,artifact_id,found_at,is_relic)
                 VALUES({ME},{GUILD},'a',{MARCH},1),({ME},{GUILD},'b',{MARCH},0),
                       ({ME},{GUILD},'c',{LAST_YEAR},0);
             INSERT INTO dig_artifact_registry(artifact_id,guild_id,first_finder_id,first_found_at,total_found)
                 VALUES('a',{GUILD},{ME},{MARCH},1),('z',{GUILD},{ME},{LAST_YEAR},1);
             INSERT INTO dig_achievements(discord_id,guild_id,achievement_id,unlocked_at)
                 VALUES({ME},{GUILD},'first',{MARCH});
             INSERT INTO tunnels(discord_id,guild_id,prestige_level,tunnel_name)
                 VALUES({ME},{GUILD},2,'The Burrow');"
        ))
        .unwrap();

    let year = f.repository.dig_year(GUILD, ME, YEAR).unwrap();
    assert_eq!(year.digs, 3);
    assert_eq!(year.paid_digs, 1);
    assert_eq!(year.blocks_dug, 12, "only downward progress counts");
    assert_eq!(year.deepest, 9);
    assert_eq!(year.jc_from_digs, 7);
    assert_eq!(year.cave_ins, 1);
    assert_eq!(year.events, 1);
    assert_eq!(year.active_days, 2);
    assert_eq!(year.busiest_day, Some(("2026-03-01".to_owned(), 2)));
    assert_eq!(
        year.bosses,
        vec![
            BossRecord {
                boss_id: "grothak".into(),
                wins: 1,
                losses: 2
            },
            BossRecord {
                boss_id: "lina".into(),
                wins: 1,
                losses: 0
            },
        ]
    );
    assert_eq!((year.sabotages_sent, year.sabotages_received), (3, 1));
    assert_eq!(
        year.favorite_target,
        Some(PersonTally {
            discord_id: RIVAL,
            count: 2
        })
    );
    assert_eq!(year.top_saboteur.map(|p| p.discord_id), Some(RIVAL));
    assert_eq!((year.helps_given, year.helps_received), (0, 1));
    assert_eq!((year.artifacts, year.relics, year.first_finds), (2, 1, 1));
    assert_eq!(year.achievements, 1);
    assert_eq!(year.prestige_level, 2);
    assert_eq!(year.tunnel_name.as_deref(), Some("The Burrow"));
    assert_eq!(year.rank, Some(ServerRank { rank: 1, of: 2 }));
    assert_eq!(
        f.repository.dig_year(GUILD, FRIEND, YEAR).unwrap().rank,
        Some(ServerRank { rank: 2, of: 2 })
    );
}

#[test]
fn pet_year_covers_pets_alive_during_the_year() {
    let f = fixture();
    f.connection
        .execute_batch(&format!(
            "INSERT INTO pets(pet_id,discord_id,guild_id,name,species,adopted_at,hatched_at,
                  adopt_fee,last_fed_at,hunger_at_last_fed,times_fed,is_active,died_at,death_cause)
             VALUES (1,{ME},{GUILD},'Old Timer','any',{LAST_YEAR},{LAST_YEAR},0,0,0,30,0,{MARCH},'starvation'),
                    (2,{ME},{GUILD},'Gone Before','any',{LAST_YEAR},{LAST_YEAR},0,0,0,99,0,{LAST_YEAR},'starvation'),
                    (3,{ME},{GUILD},'Sir Fluff','any',{MARCH},{MARCH},0,0,0,12,1,NULL,NULL),
                    (4,{ME},{OTHER_GUILD},'Elsewhere','any',{MARCH},{MARCH},0,0,0,50,1,NULL,NULL),
                    (5,{FRIEND},{GUILD},'Rex','any',{MARCH},{MARCH},0,0,0,80,1,NULL,NULL);
             INSERT INTO pet_brawls(guild_id,channel_id,challenger_id,recipient_id,challenger_pet_id,
                  status,created_at,expires_at,resolved_at,winner_id)
             VALUES ({GUILD},1,{ME},{FRIEND},3,'done',{MARCH},{MARCH},{MARCH},{ME}),
                    ({GUILD},1,{FRIEND},{ME},5,'done',{MARCH},{MARCH},{MARCH},{FRIEND}),
                    ({GUILD},1,{ME},{RIVAL},3,'done',{MARCH},{MARCH},{MARCH},NULL),
                    ({GUILD},1,{ME},{FRIEND},3,'declined',{MARCH},{MARCH},{MARCH},{ME}),
                    ({GUILD},1,{ME},{FRIEND},3,'done',{LAST_YEAR},{LAST_YEAR},{LAST_YEAR},{ME});
             INSERT INTO pet_keepsakes(discord_id,guild_id,game_day,pet_id,pet_name,keepsake_id,received_at)
             VALUES ({ME},{GUILD},1,3,'Sir Fluff','bell',{MARCH});
             INSERT INTO economy_ledger_entries(guild_id,account_type,account_id,delta,balance_before,
                  balance_after,source,created_at)
             VALUES ({GUILD},'player',{ME},-40,100,60,'pet',{MARCH}),
                    ({GUILD},'player',{ME},15,60,75,'pet',{MARCH}),
                    ({GUILD},'player',{ME},-9,75,66,'bet',{MARCH}),
                    ({GUILD},'player',{ME},-7,66,59,'pet',{LAST_YEAR});"
        ))
        .unwrap();
    let year = f.repository.pet_year(GUILD, ME, YEAR).unwrap();
    assert_eq!(
        year.pets
            .iter()
            .map(|pet| pet.name.as_str())
            .collect::<Vec<_>>(),
        ["Old Timer", "Sir Fluff"]
    );
    assert_eq!(year.meals(), 42);
    assert_eq!(year.losses().len(), 1);
    assert!(year.pets[1].is_active);
    assert_eq!(year.jc_spent, 40);
    assert_eq!(
        (year.brawl_wins, year.brawl_losses, year.brawl_draws),
        (1, 1, 1)
    );
    assert_eq!(
        year.brawl_rival,
        Some(PersonTally {
            discord_id: FRIEND,
            count: 2
        })
    );
    assert_eq!(year.keepsakes, 1);
    assert_eq!(year.rank, Some(ServerRank { rank: 2, of: 2 }));
}

#[test]
fn mafia_year_reads_settled_games_only() {
    let f = fixture();
    // Game 1: I was Mafia and won, killed FRIEND on night one, was MVP.
    f.mafia_game(1, GUILD, "MAFIA", Some(ME), MARCH);
    f.mafia_player(1, ME, "MAFIA", None);
    f.mafia_player(1, FRIEND, "TOWNIE", Some("NIGHT"));
    f.mafia_player(1, RIVAL, "DETECTIVE", None);
    f.mafia_action(1, ME, FRIEND, "KILL", "NIGHT", 1);
    f.mafia_action(1, RIVAL, ME, "VOTE", "DAY", 1);
    // Game 2: I was town, voted correctly for the Mafia, got voted out anyway.
    f.mafia_game(2, GUILD, "TOWN", None, MARCH + 86_400);
    f.mafia_player(2, ME, "DOCTOR", Some("DAY"));
    f.mafia_player(2, RIVAL, "MAFIA", Some("DAY"));
    f.mafia_action(2, ME, RIVAL, "VOTE", "DAY", 1);
    f.mafia_action(2, RIVAL, ME, "VOTE", "DAY", 1);
    // Unsettled, cancelled, last-year, and other-guild games never count.
    f.mafia_game_with(3, GUILD, "NONE", None, MARCH, "NIGHT", "ACTIVE");
    f.mafia_player(3, ME, "MAFIA", None);
    f.mafia_action(3, ME, FRIEND, "KILL", "NIGHT", 1);
    f.mafia_game_with(4, GUILD, "NONE", None, MARCH, "RESOLVED", "CANCELLED");
    f.mafia_player(4, ME, "JESTER", None);
    f.mafia_game(5, GUILD, "MAFIA", None, LAST_YEAR);
    f.mafia_player(5, ME, "MAFIA", None);
    f.mafia_game(6, OTHER_GUILD, "MAFIA", None, MARCH);
    f.connection
        .execute(
            "INSERT INTO mafia_players(game_id,discord_id,guild_id,role) VALUES(6,?1,?2,'MAFIA')",
            params![ME, OTHER_GUILD],
        )
        .unwrap();
    f.connection
        .execute(
            "INSERT INTO mafia_bounties(game_id,guild_id,day_number,target_id,contributor_id,amount,created_at)
             VALUES(2,?1,1,?2,?3,15,?4)",
            params![GUILD, RIVAL, ME, MARCH],
        )
        .unwrap();

    let year = f.repository.mafia_year(GUILD, ME, YEAR).unwrap();
    assert_eq!((year.games, year.wins), (2, 2));
    assert_eq!((year.mafia_games, year.mafia_wins), (1, 1));
    assert_eq!((year.town_games, year.town_wins), (1, 1));
    assert_eq!((year.doctor_games, year.mvps), (1, 1));
    assert_eq!(
        (year.survived, year.voted_out, year.killed_at_night),
        (1, 1, 0)
    );
    assert_eq!(year.kills, 1);
    assert_eq!((year.votes_cast, year.votes_on_mafia), (1, 1));
    assert_eq!(
        year.favorite_victim,
        Some(PersonTally {
            discord_id: FRIEND,
            count: 1
        })
    );
    assert_eq!(
        year.top_accuser,
        Some(PersonTally {
            discord_id: RIVAL,
            count: 2
        })
    );
    assert_eq!(year.jc_won, 50);
    assert_eq!(year.bounties_placed, 15);
    assert_eq!(year.favorite_alias, Some(("Pudge".to_owned(), 2)));
    assert_eq!(year.rank, Some(ServerRank { rank: 1, of: 3 }));

    let friend = f.repository.mafia_year(GUILD, FRIEND, YEAR).unwrap();
    assert_eq!((friend.killed_at_night, friend.first_night_deaths), (1, 1));
}

#[test]
fn gamba_side_year_reads_wheel_tips_and_bettor_rank() {
    let f = fixture();
    f.connection
        .execute_batch(&format!(
            "INSERT INTO wheel_spins(guild_id,discord_id,result,spin_time,is_bankrupt,is_golden)
             VALUES ({GUILD},{ME},50,{MARCH},0,0),({GUILD},{ME},-80,{MARCH},1,0),
                    ({GUILD},{ME},200,{MARCH},0,1),({GUILD},{ME},999,{LAST_YEAR},0,0),
                    ({OTHER_GUILD},{ME},999,{MARCH},0,0);
             INSERT INTO tip_transactions(sender_id,recipient_id,amount,fee,guild_id,timestamp)
             VALUES ({ME},{FRIEND},30,1,{GUILD},{MARCH}),({ME},{FRIEND},20,1,{GUILD},{MARCH}),
                    ({ME},{RIVAL},40,1,{GUILD},{MARCH}),({RIVAL},{ME},5,1,{GUILD},{MARCH}),
                    ({ME},{FRIEND},500,1,{GUILD},{LAST_YEAR});
             INSERT INTO bets(guild_id,match_id,discord_id,team_bet_on,amount,bet_time,leverage)
             VALUES ({GUILD},1,{ME},'radiant',10,{MARCH},5),({GUILD},1,{FRIEND},'dire',40,{MARCH},1),
                    ({GUILD},1,{RIVAL},'dire',500,{LAST_YEAR},1);"
        ))
        .unwrap();
    let (wheel, tips, rank) = f.repository.gamba_side_year(GUILD, ME, YEAR).unwrap();
    assert_eq!(
        wheel,
        WheelYear {
            spins: 3,
            net: 170,
            best: 200,
            worst: -80,
            bankrupts: 1,
            golden: 1,
        }
    );
    assert_eq!(
        (tips.sent, tips.sent_jc, tips.received, tips.received_jc),
        (3, 90, 1, 5)
    );
    assert_eq!(
        tips.top_recipient,
        Some(PersonTally {
            discord_id: FRIEND,
            count: 50
        })
    );
    assert_eq!(rank, Some(ServerRank { rank: 1, of: 2 }));
}
