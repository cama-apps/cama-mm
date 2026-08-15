PRAGMA user_version = 70811;

CREATE TABLE heroes (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    localized_name TEXT,
    aliases TEXT,
    roles TEXT,
    real_name TEXT,
    hype TEXT,
    bio TEXT,
    attr_primary TEXT,
    is_melee INTEGER,
    base_movement INTEGER,
    base_armor INTEGER,
    attack_rate REAL,
    attr_strength_base INTEGER,
    attr_agility_base INTEGER,
    attr_intelligence_base INTEGER,
    attr_strength_gain REAL,
    attr_agility_gain REAL,
    attr_intelligence_gain REAL,
    vision_night INTEGER,
    turn_rate REAL,
    attack_damage_min INTEGER,
    attack_damage_max INTEGER,
    attack_range INTEGER,
    magic_resistance INTEGER,
    color TEXT
);

CREATE TABLE abilities (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    localized_name TEXT,
    hero_id INTEGER REFERENCES heroes(id),
    slot INTEGER,
    behavior TEXT,
    damage_type TEXT,
    damage TEXT,
    description TEXT,
    cooldown TEXT,
    lore TEXT,
    ability_special TEXT,
    scepter_upgrades INTEGER,
    scepter_description TEXT,
    shard_upgrades INTEGER,
    shard_description TEXT,
    innate INTEGER,
    icon TEXT,
    mana_cost TEXT
);

CREATE TABLE talents (
    hero_id INTEGER NOT NULL REFERENCES heroes(id),
    ability_id INTEGER NOT NULL REFERENCES abilities(id),
    slot INTEGER NOT NULL,
    PRIMARY KEY (hero_id, ability_id)
);

CREATE TABLE items (
    id INTEGER PRIMARY KEY,
    localized_name TEXT,
    cost INTEGER,
    lore TEXT,
    neutral_tier TEXT,
    icon TEXT,
    is_neutral_enhancement INTEGER,
    ability_special TEXT,
    cooldown TEXT,
    json_data TEXT
);

CREATE TABLE responses (
    fullname TEXT PRIMARY KEY,
    hero_id INTEGER REFERENCES heroes(id),
    text_simple TEXT
);

CREATE TABLE facets (
    id INTEGER PRIMARY KEY,
    localized_name TEXT,
    hero_id INTEGER NOT NULL REFERENCES heroes(id),
    description TEXT,
    slot INTEGER
);

INSERT INTO heroes (
    id,name,localized_name,aliases,roles,real_name,hype,bio,attr_primary,is_melee,
    base_movement,base_armor,attack_rate,attr_strength_base,attr_agility_base,
    attr_intelligence_base,attr_strength_gain,attr_agility_gain,attr_intelligence_gain,
    vision_night,turn_rate,attack_damage_min,attack_damage_max,attack_range,
    magic_resistance,color
) VALUES
    (1,'npc_dota_hero_antimage','Anti-Mage','am|magina','Carry|Escape|Nuker',
     'Magina','Hates magic.','A warrior who hunts spellcasters.','agility',1,
     310,2.0,1.4,23,24,12,1.6,2.8,1.8,800,0.5,29,33,150,25,'#784094'),
    (14,'npc_dota_hero_pudge','Pudge','butcher','Disabler|Initiator|Durable|Nuker',
     'Pudge','A butcher with a hook.','A rotund horror from the Halls of the Dead.',
     'strength',1,280,0.0,1.7,25,14,16,3.0,1.4,1.8,800,0.7,27,33,150,25,'#8E24AA');

INSERT INTO abilities (
    id,name,localized_name,hero_id,slot,behavior,damage_type,damage,cooldown,lore,
    ability_special,scepter_upgrades,scepter_description,shard_upgrades,shard_description,
    innate,icon,mana_cost
) VALUES
    (10,'antimage_blink','Blink',1,0,'Point Target','Magical','0','12','Teleport short distances.',
     '[{"key":"blink_range","header":"Blink Range","value":"1200"}]',1,
     'Blink has increased range.',1,'Blink silences enemies on arrival.',0,
     '/panorama/images/spellicons/antimage_blink_png.png','50'),
    (14,'pudge_meat_hook','Meat Hook',14,0,'Point Target','Pure','150','18',
     'Launches a bloody hook.',NULL,0,NULL,0,NULL,0,
     '/panorama/images/spellicons/pudge_meathook_png.png','110'),
    (101,'special_bonus_strength_10','+10 Strength',14,NULL,'Passive',NULL,NULL,NULL,NULL,
     NULL,0,NULL,0,NULL,0,NULL,NULL),
    (102,'special_bonus_health_175','+175 Health',14,NULL,'Passive',NULL,NULL,NULL,NULL,
     NULL,0,NULL,0,NULL,0,NULL,NULL),
    (103,'special_bonus_attack_damage_25','+25 Damage',14,NULL,'Passive',NULL,NULL,NULL,NULL,
     NULL,0,NULL,0,NULL,0,NULL,NULL),
    (104,'special_bonus_flesh_heap_2','+2 Flesh Heap',14,NULL,'Passive',NULL,NULL,NULL,NULL,
     NULL,0,NULL,0,NULL,0,NULL,NULL),
    (105,'special_bonus_dismember_damage_4','+4 Dismember Damage',14,NULL,'Passive',NULL,NULL,NULL,NULL,
     NULL,0,NULL,0,NULL,0,NULL,NULL),
    (106,'special_bonus_rot_slow_16','+16 Rot Slow',14,NULL,'Passive',NULL,NULL,NULL,NULL,
     NULL,0,NULL,0,NULL,0,NULL,NULL),
    (107,'special_bonus_meat_hook_damage_140','+140 Meat Hook Damage',14,NULL,'Passive',NULL,NULL,NULL,NULL,
     NULL,0,NULL,0,NULL,0,NULL,NULL),
    (108,'special_bonus_dismember_duration_4','+4 Dismember Duration',14,NULL,'Passive',NULL,NULL,NULL,NULL,
     NULL,0,NULL,0,NULL,0,NULL,NULL),
    (109,'special_bonus_unique_antimage','Hidden Talent',1,NULL,'Passive',NULL,NULL,NULL,NULL,
     NULL,0,NULL,0,NULL,0,NULL,NULL);

INSERT INTO talents (hero_id,ability_id,slot) VALUES
    (1,109,0),
    (14,101,0),(14,102,1),(14,103,2),(14,104,3),
    (14,105,4),(14,106,5),(14,107,6),(14,108,7);

INSERT INTO items (
    id,localized_name,cost,lore,neutral_tier,icon,is_neutral_enhancement,
    ability_special,cooldown,json_data
) VALUES
    (1,'Blink Dagger',2250,'A dagger that bends space.',NULL,
     '/panorama/images/items/blink_png.png',0,NULL,NULL,
     '{"ItemPurchasable":"1","ItemBaseLevel":"1"}'),
    (20,'Dagon',2850,'A burst of magical damage.',NULL,
     '/panorama/images/items/dagon_png.png',0,
     '[{"key":"damage","header":"Damage","value":"400"}]','35',
     '{"ItemPurchasable":"1","ItemBaseLevel":"1"}');

INSERT INTO responses (fullname,hero_id,text_simple) VALUES
    ('anti_fixture',1,'I will end this magic before it begins'),
    ('pudge_fixture',14,'A fresh meal awaits!');

INSERT INTO facets (id,localized_name,hero_id,description,slot) VALUES
    (30,'Magebane''s Mirror',1,'Reflects a spell.',0),
    (40,'Flayer''s Hook',14,'Extends the butcher''s reach.',0),
    (41,'Ravenous Maw',14,'Feeds the rot.',1);
