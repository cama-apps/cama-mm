use std::collections::VecDeque;

use super::*;

#[derive(Debug)]
struct ScriptedEntropy {
    units: VecDeque<f64>,
    indexes: VecDeque<usize>,
}

impl ScriptedEntropy {
    fn new(units: impl IntoIterator<Item = f64>, indexes: impl IntoIterator<Item = usize>) -> Self {
        Self {
            units: units.into_iter().collect(),
            indexes: indexes.into_iter().collect(),
        }
    }
}

impl TunnelNameEntropy for ScriptedEntropy {
    fn unit_f64(&mut self) -> f64 {
        self.units.pop_front().expect("scripted pool roll")
    }

    fn index(&mut self, upper_exclusive: usize) -> usize {
        self.indexes.pop_front().expect("scripted pool index") % upper_exclusive
    }
}

fn scripted(roll: f64, indexes: impl IntoIterator<Item = usize>) -> String {
    DigTunnelNamingService::new(ScriptedEntropy::new([roll], indexes)).generate_tunnel_name()
}

#[test]
fn test_low_roll_yields_adjective_noun_name() {
    assert_eq!(scripted(0.0, [0, 0]), "The Whispering Descent");
}

#[test]
fn test_boundary_below_040_is_adjective_noun() {
    assert_eq!(scripted(0.3999, [14, 14]), "The Slippery Mine");
}

#[test]
fn test_mid_roll_yields_title_name() {
    assert_eq!(scripted(0.50, [0]), "Shaft of Sorrows");
}

#[test]
fn test_boundary_at_040_is_title() {
    assert_eq!(scripted(0.40, [1]), "Shaft of Echoes");
}

#[test]
fn test_boundary_below_075_is_title() {
    assert_eq!(scripted(0.7499, [TITLE_COUNT - 1]), "Excavation of Regret");
}

#[test]
fn test_high_roll_yields_silly_name() {
    assert_eq!(scripted(0.99, [0]), "Tunnel McTunnelface");
}

#[test]
fn test_boundary_at_075_is_silly() {
    assert_eq!(scripted(0.75, [1]), "The Big Hole");
}

#[test]
fn test_always_returns_a_known_pool_name() {
    let mut service = DigTunnelNamingService::new(FastrandTunnelNameEntropy::seeded(2_024));
    for _ in 0..500 {
        assert!(is_known_tunnel_name(&service.generate_tunnel_name()));
    }
}

#[test]
fn test_names_are_non_blank_strings() {
    let mut service = DigTunnelNamingService::new(FastrandTunnelNameEntropy::seeded(11));
    for _ in 0..200 {
        assert!(!service.generate_tunnel_name().trim().is_empty());
    }
}

#[test]
fn test_all_three_pools_are_reachable() {
    let mut service = DigTunnelNamingService::new(FastrandTunnelNameEntropy::seeded(777));
    let mut saw_adjective_noun = false;
    let mut saw_title = false;
    let mut saw_silly = false;
    for _ in 0..2_000 {
        let name = service.generate_tunnel_name();
        if TUNNEL_NAME_SILLY.contains(&name.as_str()) {
            saw_silly = true;
        } else if name.starts_with("The ") {
            saw_adjective_noun = true;
        } else {
            saw_title = true;
        }
    }
    assert!(saw_adjective_noun && saw_title && saw_silly);
}

#[test]
fn test_seeded_generation_is_deterministic() {
    let sequence = |seed| {
        let mut service = DigTunnelNamingService::new(FastrandTunnelNameEntropy::seeded(seed));
        (0..20)
            .map(|_| service.generate_tunnel_name())
            .collect::<Vec<_>>()
    };
    assert_eq!(sequence(555), sequence(555));
}

#[test]
fn test_weighting_roughly_matches_40_35_25() {
    let mut service = DigTunnelNamingService::new(FastrandTunnelNameEntropy::seeded(20_260_516));
    let mut adjective_noun = 0_u64;
    let mut title = 0_u64;
    let mut silly = 0_u64;
    let total = 6_000_u64;
    for _ in 0..total {
        let name = service.generate_tunnel_name();
        if TUNNEL_NAME_SILLY.contains(&name.as_str()) {
            silly += 1;
        } else if name.starts_with("The ") {
            adjective_noun += 1;
        } else {
            title += 1;
        }
    }
    let share = |count| count as f64 / total as f64;
    assert!((0.33..0.47).contains(&share(adjective_noun)));
    assert!((0.28..0.42).contains(&share(title)));
    assert!((0.18..0.32).contains(&share(silly)));
}
