//! Cosmetic gifts and the memories pets leave behind.

use crate::pet_evolution::PetCalling;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keepsake {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub calling: Option<PetCalling>,
}

pub static KEEPSAKES: [Keepsake; 22] = [
    Keepsake {
        id: "moon_button",
        name: "Moon Button",
        description: "A tiny button that looks suspiciously like the moon.",
        calling: None,
    },
    Keepsake {
        id: "pocket_cloud",
        name: "Pocket Cloud",
        description: "A tuft of fluff kept for a rainy day.",
        calling: None,
    },
    Keepsake {
        id: "wonky_pebble",
        name: "Wonky Pebble",
        description: "A pebble with a face only your pet could love.",
        calling: None,
    },
    Keepsake {
        id: "ribbon_scrap",
        name: "Ribbon Scrap",
        description: "A proudly rescued piece of ribbon.",
        calling: None,
    },
    Keepsake {
        id: "paper_star",
        name: "Paper Star",
        description: "Folded with more enthusiasm than precision.",
        calling: None,
    },
    Keepsake {
        id: "tiny_teacup",
        name: "Tiny Teacup",
        description: "Just big enough for an imaginary tea party.",
        calling: None,
    },
    Keepsake {
        id: "glowstone_chip",
        name: "Glowstone Chip",
        description: "A soft glow from a favorite little tunnel.",
        calling: Some(PetCalling::Delver),
    },
    Keepsake {
        id: "acorn_trophy",
        name: "Acorn Trophy",
        description: "An acorn balanced on a very important pedestal.",
        calling: Some(PetCalling::Champion),
    },
    Keepsake {
        id: "two_heads_coin",
        name: "Two Heads Coin",
        description: "A pretend coin for impressively predictable tricks.",
        calling: Some(PetCalling::Trickster),
    },
    Keepsake {
        id: "friendship_thread",
        name: "Friendship Thread",
        description: "A little knot tied with you in mind.",
        calling: Some(PetCalling::Companion),
    },
    Keepsake {
        id: "leaf_bookmark",
        name: "Leaf Bookmark",
        description: "A pressed leaf marking a particularly wise page.",
        calling: Some(PetCalling::Sage),
    },
    Keepsake {
        id: "thimble_helmet",
        name: "Thimble Helmet",
        description: "A battered thimble from an imaginary arena.",
        calling: Some(PetCalling::Pitfighter),
    },
    Keepsake {
        id: "fools_gold_flake",
        name: "Fool's Gold Flake",
        description: "It sparkles, which is what really matters.",
        calling: Some(PetCalling::Prospector),
    },
    Keepsake {
        id: "twig_signpost",
        name: "Twig Signpost",
        description: "A tiny sign pointing toward home.",
        calling: Some(PetCalling::Trailkeeper),
    },
    Keepsake {
        id: "ancient_bottlecap",
        name: "Ancient Bottlecap",
        description: "A priceless relic of somebody's lunch.",
        calling: Some(PetCalling::Archaeologist),
    },
    Keepsake {
        id: "stunt_feather",
        name: "Stunt Feather",
        description: "A souvenir from a rather ambitious leap.",
        calling: Some(PetCalling::Daredevil),
    },
    Keepsake {
        id: "button_shield",
        name: "Button Shield",
        description: "A sturdy button ready to guard the album.",
        calling: Some(PetCalling::Guardian),
    },
    Keepsake {
        id: "pebble_chess_piece",
        name: "Pebble Chess Piece",
        description: "A small stone with very big plans.",
        calling: Some(PetCalling::Tactician),
    },
    Keepsake {
        id: "daisy_crown",
        name: "Daisy Crown",
        description: "A slightly crooked crown made with care.",
        calling: Some(PetCalling::Charmer),
    },
    Keepsake {
        id: "clouded_marble",
        name: "Clouded Marble",
        description: "The future inside looks mostly swirly.",
        calling: Some(PetCalling::Oracle),
    },
    Keepsake {
        id: "tiny_chalkboard",
        name: "Tiny Chalkboard",
        description: "Today's lesson is a lovingly drawn smile.",
        calling: Some(PetCalling::Mentor),
    },
    Keepsake {
        id: "postcard_leaf",
        name: "Postcard Leaf",
        description: "A leaf brought back from somewhere interesting.",
        calling: Some(PetCalling::Wayfarer),
    },
];

#[must_use]
pub fn get_keepsake(id: &str) -> Option<&'static Keepsake> {
    KEEPSAKES.iter().find(|item| item.id == id)
}

#[must_use]
pub fn choose_keepsake(calling: Option<PetCalling>, roll: u64) -> &'static Keepsake {
    let eligible = || {
        KEEPSAKES
            .iter()
            .filter(|item| item.calling.is_none() || item.calling == calling)
    };
    let index = (roll % eligible().count() as u64) as usize;
    eligible()
        .nth(index)
        .expect("the common keepsakes are always eligible")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiftReceipt {
    pub pet_id: i64,
    pub pet_name: String,
    pub keepsake_id: String,
    pub received_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumEntry {
    pub keepsake_id: String,
    pub first_pet_name: String,
    pub first_received_at: i64,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiaryEntry {
    pub pet_id: i64,
    pub pet_name: String,
    pub occurred_at: i64,
    pub kind: DiaryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiaryKind {
    Hatched,
    Calling(String),
    FirstBrawlWin,
    Gift(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn catalog_has_unique_ids_and_one_keepsake_per_calling() {
        assert_eq!(KEEPSAKES.len(), 22);
        let ids: BTreeSet<_> = KEEPSAKES.iter().map(|item| item.id).collect();
        assert_eq!(ids.len(), KEEPSAKES.len());
        assert_eq!(
            KEEPSAKES
                .iter()
                .filter(|item| item.calling.is_none())
                .count(),
            6
        );
        for calling in PetCalling::ALL {
            assert_eq!(
                KEEPSAKES
                    .iter()
                    .filter(|item| item.calling == Some(calling))
                    .count(),
                1
            );
        }
        for item in &KEEPSAKES {
            assert!(!item.name.is_empty());
            assert!(!item.description.is_empty());
            assert_eq!(get_keepsake(item.id), Some(item));
        }
        assert_eq!(get_keepsake("unknown"), None);
    }

    #[test]
    fn pets_without_a_calling_receive_all_six_common_keepsakes_only() {
        let mut ids = BTreeSet::new();
        for roll in 0..420 {
            let item = choose_keepsake(None, roll);
            assert_eq!(item.calling, None);
            ids.insert(item.id);
        }
        assert_eq!(ids.len(), 6);
        assert_eq!(choose_keepsake(None, u64::MAX).calling, None);
    }

    #[test]
    fn each_calling_receives_common_and_its_own_keepsake_only() {
        for calling in PetCalling::ALL {
            let mut ids = BTreeSet::new();
            for roll in (0..420).chain([u64::MAX]) {
                let item = choose_keepsake(Some(calling), roll);
                assert!(item.calling.is_none() || item.calling == Some(calling));
                ids.insert(item.id);
            }
            assert_eq!(ids.len(), 7);
        }
    }
}
