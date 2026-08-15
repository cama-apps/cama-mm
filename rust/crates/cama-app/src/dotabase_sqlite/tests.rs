use std::path::Path;

use rusqlite::Connection;
use tempfile::NamedTempFile;

use super::*;
use crate::trivia_data::TriviaDataRegistry;

fn fixture() -> NamedTempFile {
    let file = NamedTempFile::new().unwrap();
    let connection = Connection::open(file.path()).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE heroes (
                id INTEGER PRIMARY KEY,name TEXT,localized_name TEXT,real_name TEXT,hype TEXT,
                bio TEXT,attr_primary TEXT,is_melee INTEGER,base_movement INTEGER,
                base_armor REAL,attack_rate REAL,attr_strength_base REAL,
                attr_agility_base REAL,attr_intelligence_base REAL,attr_strength_gain REAL,
                attr_agility_gain REAL,attr_intelligence_gain REAL,vision_night INTEGER,
                turn_rate REAL,attack_damage_min INTEGER,attack_damage_max INTEGER
             );
             CREATE TABLE abilities (
                id INTEGER PRIMARY KEY,name TEXT,localized_name TEXT,hero_id INTEGER,
                damage_type TEXT,damage TEXT,cooldown TEXT,lore TEXT,scepter_upgrades INTEGER,
                scepter_description TEXT,shard_upgrades INTEGER,shard_description TEXT,
                innate INTEGER,icon TEXT,mana_cost TEXT
             );
             CREATE TABLE talents (hero_id INTEGER,ability_id INTEGER,slot INTEGER);
             CREATE TABLE items (
                id INTEGER PRIMARY KEY,localized_name TEXT,cost INTEGER,lore TEXT,
                neutral_tier TEXT,icon TEXT,is_neutral_enhancement INTEGER,
                ability_special TEXT,cooldown TEXT,json_data TEXT
             );
             CREATE TABLE responses (fullname TEXT PRIMARY KEY,hero_id INTEGER,text_simple TEXT);
             CREATE TABLE facets (
                id INTEGER PRIMARY KEY,localized_name TEXT,hero_id INTEGER,description TEXT,
                slot INTEGER
             );
             INSERT INTO heroes VALUES
                (1,'npc_dota_hero_antimage','Anti-Mage','Magina','Hype','Bio','agility',1,
                 310,2.0,1.4,23,24,12,1.6,2.8,1.8,800,0.5,29,33);
             INSERT INTO abilities VALUES
                (10,'antimage_blink','Blink',1,'pure','50','5','Lore',1,'Scepter',1,
                 'Shard',0,'/panorama/images/spellicons/antimage_blink_png.png','60');
             INSERT INTO abilities VALUES
                (11,'special_bonus_unique','Talent',1,NULL,NULL,NULL,NULL,0,NULL,0,NULL,0,NULL,NULL);
             INSERT INTO talents VALUES (1,11,0);
             INSERT INTO items VALUES
                (20,'Dagon',2850,'Zap',NULL,'/panorama/images/items/dagon_png.png',0,
                 '[{\"key\":\"damage\",\"header\":\"Damage\",\"value\":\"400\"}]',
                 '35','{\"ItemPurchasable\":\"1\",\"ItemBaseLevel\":\"1\"}');
             INSERT INTO responses VALUES
                ('anti_long',1,'I will end this magic before it begins');
             INSERT INTO facets VALUES (30,'Magebane''s Mirror',1,'Reflects a spell',0);",
        )
        .unwrap();
    file
}

#[test]
fn immutable_sqlite_source_populates_the_shared_trivia_registry() {
    let file = fixture();
    let source = DotabaseSqliteSource::new(file.path());
    let registry = TriviaDataRegistry::new(source);

    let heroes = registry.load_heroes().unwrap();
    assert_eq!(heroes.len(), 1);
    assert_eq!(heroes[0].localized_name, "Anti-Mage");
    assert_eq!(heroes[0].damage_min_at_level_one, Some(53));

    let abilities = registry.load_abilities().unwrap();
    assert_eq!(abilities.len(), 1);
    assert_eq!(abilities[0].localized_name, "Blink");
    assert_eq!(
        abilities[0].icon_url.as_deref(),
        Some(
            "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/abilities/antimage_blink.png"
        )
    );

    let items = registry.load_items().unwrap();
    assert_eq!(items[0].localized_name, "Dagon");
    assert_eq!(items[0].special_values[0].value, "400");
    assert_eq!(items[0].base_level, Some(1));
    assert_eq!(registry.load_voicelines().unwrap().len(), 1);
    assert_eq!(
        registry.load_facets().unwrap()[0].localized_name,
        "Magebane's Mirror"
    );
}

#[test]
fn source_never_creates_a_missing_asset_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing-dotabase.db");
    let error = DotabaseSqliteSource::new(&path).heroes().unwrap_err();
    assert!(error.to_string().contains("unable to open database file"));
    assert!(!Path::new(&path).exists());
}

#[test]
fn test_import_does_not_initialize_dotabase_orm_trivia_data() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing-trivia-dotabase.db");
    let _source = DotabaseSqliteSource::new(&path);
    assert!(
        !path.exists(),
        "constructing the trivia adapter must not open SQLite"
    );
}

#[test]
fn test_import_does_not_initialize_dotabase_orm_dota_info() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing-dota-info-dotabase.db");
    let _service = crate::dota_info::DotaInfoService::new(DotabaseSqliteSource::new(&path));
    assert!(
        !path.exists(),
        "constructing the Dota-info service must not open SQLite"
    );
}

#[test]
fn test_first_data_lookup_initializes_dotabase_orm_trivia_data_load_heroes() {
    let file = fixture();
    let registry = TriviaDataRegistry::new(DotabaseSqliteSource::new(file.path()));
    let heroes = registry.load_heroes().expect("first trivia lookup");
    assert_eq!(heroes.len(), 1);
    assert_eq!(heroes[0].localized_name, "Anti-Mage");
}

#[test]
fn test_first_data_lookup_initializes_dotabase_orm_dota_info_get_all_heroes() {
    let file = fixture();
    let source = DotabaseSqliteSource::new(file.path());
    let heroes = source.all_heroes().expect("first Dota-info lookup");
    assert_eq!(heroes, [("Anti-Mage".to_owned(), 1)]);
}

#[test]
#[ignore = "requires the pinned Dotabase package asset"]
fn pinned_dotabase_asset_loads_through_the_production_adapter() {
    let path = std::env::var("CAMA_DOTABASE_TEST_PATH")
        .expect("set CAMA_DOTABASE_TEST_PATH to the pinned dotabase.db asset");
    let registry = TriviaDataRegistry::new(DotabaseSqliteSource::new(path));

    let heroes = registry.load_heroes().unwrap();
    assert!(heroes.len() >= 100);
    assert!(heroes.iter().any(|hero| hero.localized_name == "Anti-Mage"));

    let abilities = registry.load_abilities().unwrap();
    assert!(abilities.len() >= 500);
    assert!(
        abilities
            .iter()
            .any(|ability| ability.localized_name == "Blink")
    );

    assert!(registry.load_items().unwrap().len() >= 300);
    assert!(registry.load_voicelines().unwrap().len() >= 1_000);
    assert!(registry.load_facets().unwrap().len() >= 35);

    let source = DotabaseSqliteSource::new(
        std::env::var("CAMA_DOTABASE_TEST_PATH").expect("asset path remains available"),
    );
    assert!(source.all_heroes().unwrap().len() > 100);
    assert!(source.all_abilities().unwrap().len() > 200);
    for query in ["Anti-Mage", "anti-mage", "am"] {
        let hero = source.hero_by_name(query).unwrap().expect("Anti-Mage");
        assert_eq!(hero.id, 1);
        assert_eq!(hero.localized_name, "Anti-Mage");
    }
    let pudge = source.hero_by_name("Pudge").unwrap().expect("Pudge");
    assert!(pudge.abilities.iter().any(|ability| ability == "Meat Hook"));
    assert_eq!(pudge.talents.len(), 8);
    let witch_doctor = source
        .hero_by_name("Witch Doctor")
        .unwrap()
        .expect("Witch Doctor");
    assert!(witch_doctor.facets.len() >= 2);
    assert!(
        witch_doctor
            .facets
            .iter()
            .all(|facet| !facet.localized_name.is_empty() && !facet.description.is_empty())
    );
    let meat_hook = source
        .ability_by_name("meat hook")
        .unwrap()
        .expect("Meat Hook");
    assert_eq!(meat_hook.localized_name, "Meat Hook");
    assert!(meat_hook.description.is_some());
}
