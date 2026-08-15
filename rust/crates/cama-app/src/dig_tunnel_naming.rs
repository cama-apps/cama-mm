//! Canonical three-pool tunnel-name policy used by every Dig tunnel creator.
//!
//! Pool selection is exactly 40% adjective+noun, 35% title, and 25% silly.
//! Entropy is injected so the application layer can test every strict branch
//! boundary without coupling Discord or SQLite to a random-number generator.

pub const TUNNEL_NAME_ADJECTIVES: [&str; 15] = [
    "Whispering",
    "Echoing",
    "Forgotten",
    "Shimmering",
    "Crooked",
    "Haunted",
    "Dusty",
    "Verdant",
    "Frostbitten",
    "Soggy",
    "Screaming",
    "Gilded",
    "Moldy",
    "Thundering",
    "Slippery",
];

pub const TUNNEL_NAME_NOUNS: [&str; 15] = [
    "Descent",
    "Passage",
    "Burrow",
    "Excavation",
    "Shaft",
    "Tunnel",
    "Grotto",
    "Hollow",
    "Delve",
    "Pit",
    "Crevice",
    "Gallery",
    "Abyss",
    "Warren",
    "Mine",
];

pub const TUNNEL_NAME_TITLE_X: [&str; 10] = [
    "Shaft",
    "Tunnel",
    "Mine",
    "Passage",
    "Depths",
    "Burrow",
    "Pit",
    "Grotto",
    "Hollow",
    "Excavation",
];

pub const TUNNEL_NAME_TITLE_Y: [&str; 13] = [
    "Sorrows", "Echoes", "Fortune", "Doom", "Whispers", "Secrets", "Madness", "Riches", "Despair",
    "Wonder", "Cheese", "Bones", "Regret",
];

pub const TUNNEL_NAME_SILLY: [&str; 10] = [
    "Tunnel McTunnelface",
    "The Big Hole",
    "Definitely Not a Grave",
    "Hole-y Moley",
    "Rock Bottom",
    "Dig Dug's Revenge",
    "The Mole Hole",
    "Shovel Knight's Disgrace",
    "Spelunky Rejects",
    "The Underground Railroad to Nowhere",
];

const TITLE_COUNT: usize = TUNNEL_NAME_TITLE_X.len() * TUNNEL_NAME_TITLE_Y.len();

pub trait TunnelNameEntropy {
    fn unit_f64(&mut self) -> f64;
    fn index(&mut self, upper_exclusive: usize) -> usize;
}

impl<T: TunnelNameEntropy + ?Sized> TunnelNameEntropy for &mut T {
    fn unit_f64(&mut self) -> f64 {
        T::unit_f64(self)
    }

    fn index(&mut self, upper_exclusive: usize) -> usize {
        T::index(self, upper_exclusive)
    }
}

#[derive(Debug)]
pub struct FastrandTunnelNameEntropy {
    rng: fastrand::Rng,
}

impl Default for FastrandTunnelNameEntropy {
    fn default() -> Self {
        Self {
            rng: fastrand::Rng::new(),
        }
    }
}

impl FastrandTunnelNameEntropy {
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        Self {
            rng: fastrand::Rng::with_seed(seed),
        }
    }
}

impl TunnelNameEntropy for FastrandTunnelNameEntropy {
    fn unit_f64(&mut self) -> f64 {
        self.rng.f64()
    }

    fn index(&mut self, upper_exclusive: usize) -> usize {
        self.rng.usize(0..upper_exclusive)
    }
}

#[derive(Debug)]
pub struct DigTunnelNamingService<E = FastrandTunnelNameEntropy> {
    entropy: E,
}

impl Default for DigTunnelNamingService<FastrandTunnelNameEntropy> {
    fn default() -> Self {
        Self::new(FastrandTunnelNameEntropy::default())
    }
}

impl<E: TunnelNameEntropy> DigTunnelNamingService<E> {
    #[must_use]
    pub const fn new(entropy: E) -> Self {
        Self { entropy }
    }

    #[must_use]
    pub fn generate_tunnel_name(&mut self) -> String {
        let roll = self.entropy.unit_f64();
        if roll < 0.40 {
            let adjective =
                TUNNEL_NAME_ADJECTIVES[self.entropy.index(TUNNEL_NAME_ADJECTIVES.len())];
            let noun = TUNNEL_NAME_NOUNS[self.entropy.index(TUNNEL_NAME_NOUNS.len())];
            format!("The {adjective} {noun}")
        } else if roll < 0.75 {
            title_at(self.entropy.index(TITLE_COUNT))
        } else {
            TUNNEL_NAME_SILLY[self.entropy.index(TUNNEL_NAME_SILLY.len())].to_owned()
        }
    }

    #[must_use]
    pub fn into_entropy(self) -> E {
        self.entropy
    }
}

#[must_use]
pub fn is_known_tunnel_name(name: &str) -> bool {
    TUNNEL_NAME_SILLY.contains(&name)
        || (0..TITLE_COUNT).any(|index| title_at(index) == name)
        || TUNNEL_NAME_ADJECTIVES.iter().any(|adjective| {
            TUNNEL_NAME_NOUNS
                .iter()
                .any(|noun| name == format!("The {adjective} {noun}"))
        })
}

fn title_at(index: usize) -> String {
    let bounded = index % TITLE_COUNT;
    let x = TUNNEL_NAME_TITLE_X[bounded / TUNNEL_NAME_TITLE_Y.len()];
    let y = TUNNEL_NAME_TITLE_Y[bounded % TUNNEL_NAME_TITLE_Y.len()];
    format!("{x} of {y}")
}

#[cfg(test)]
#[path = "dig_tunnel_naming/tests.rs"]
mod tests;
