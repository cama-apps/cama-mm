//! One player's year outside the match queue, as read for Cama Wrapped.
//!
//! Plain data shared by the read-only repository that fills it and the story
//! policy that turns it into cards. Every count covers the Wrapped year only
//! unless a field says it is the player's current state.

/// A person and how many times something happened with them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PersonTally {
    pub discord_id: i64,
    pub count: i64,
}

/// Where a player placed among everyone in the guild who did the activity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerRank {
    /// One-based; ties share the better rank.
    pub rank: i64,
    pub of: i64,
}

impl ServerRank {
    /// Rank a player's total against everyone's totals, or `None` when the
    /// player did not take part.
    #[must_use]
    pub fn among(totals: &[(i64, i64)], discord_id: i64) -> Option<Self> {
        let mine = totals
            .iter()
            .find(|(id, total)| *id == discord_id && *total > 0)?
            .1;
        let active = totals.iter().filter(|(_, total)| *total > 0).count() as i64;
        let ahead = totals.iter().filter(|(_, total)| *total > mine).count() as i64;
        Some(Self {
            rank: ahead + 1,
            of: active,
        })
    }

    /// Share of the other participants this player matched or beat, in
    /// percent. A lone participant has nobody to beat and returns `None`.
    #[must_use]
    pub fn ahead_of_percent(self) -> Option<f64> {
        (self.of > 1).then(|| (self.of - self.rank) as f64 / (self.of - 1) as f64 * 100.0)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BossRecord {
    pub boss_id: String,
    pub wins: i64,
    pub losses: i64,
}

impl BossRecord {
    #[must_use]
    pub const fn fights(&self) -> i64 {
        self.wins + self.losses
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigYear {
    pub digs: i64,
    pub paid_digs: i64,
    pub blocks_dug: i64,
    pub deepest: i64,
    pub jc_from_digs: i64,
    pub cave_ins: i64,
    pub events: i64,
    pub active_days: i64,
    /// The calendar day (UTC) with the most digs and how many happened.
    pub busiest_day: Option<(String, i64)>,
    pub artifacts: i64,
    pub relics: i64,
    /// Artifacts this player was the first in the guild to find.
    pub first_finds: i64,
    pub achievements: i64,
    /// Most-fought first.
    pub bosses: Vec<BossRecord>,
    pub sabotages_sent: i64,
    pub sabotages_received: i64,
    pub favorite_target: Option<PersonTally>,
    pub top_saboteur: Option<PersonTally>,
    pub helps_given: i64,
    pub helps_received: i64,
    /// Current state of the tunnel, not a yearly figure.
    pub prestige_level: i64,
    pub tunnel_name: Option<String>,
    pub rank: Option<ServerRank>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PetRecord {
    pub pet_id: i64,
    pub name: String,
    pub species: String,
    pub adopted_at: i64,
    pub died_at: Option<i64>,
    pub death_cause: Option<String>,
    pub times_fed: i64,
    pub is_active: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PetYear {
    /// Every pet the player kept at some point during the year, oldest first.
    pub pets: Vec<PetRecord>,
    pub jc_spent: i64,
    pub brawl_wins: i64,
    pub brawl_losses: i64,
    pub brawl_draws: i64,
    pub brawl_rival: Option<PersonTally>,
    pub keepsakes: i64,
    /// Ranked by this year's pet spending.
    pub rank: Option<ServerRank>,
}

impl PetYear {
    /// Lifetime feedings of the pets kept this year; not a yearly figure.
    #[must_use]
    pub fn lifetime_meals(&self) -> i64 {
        self.pets.iter().map(|pet| pet.times_fed).sum()
    }

    /// Pets lost during the year to anything but being traded in.
    #[must_use]
    pub fn losses(&self) -> Vec<&PetRecord> {
        self.pets
            .iter()
            .filter(|pet| pet.died_at.is_some())
            .filter(|pet| pet.death_cause.as_deref() != Some("sacrifice"))
            .collect()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MafiaYear {
    pub games: i64,
    pub wins: i64,
    pub mafia_games: i64,
    pub mafia_wins: i64,
    pub town_games: i64,
    pub town_wins: i64,
    pub jester_games: i64,
    pub jester_wins: i64,
    pub godfather_games: i64,
    pub doctor_games: i64,
    pub detective_games: i64,
    pub mvps: i64,
    pub survived: i64,
    pub killed_at_night: i64,
    pub voted_out: i64,
    pub first_night_deaths: i64,
    pub kills: i64,
    pub saves: i64,
    pub correct_reads: i64,
    pub votes_cast: i64,
    pub votes_on_mafia: i64,
    pub favorite_victim: Option<PersonTally>,
    pub top_accuser: Option<PersonTally>,
    pub jc_won: i64,
    pub bounties_placed: i64,
    pub favorite_alias: Option<(String, i64)>,
    /// Ranked by games played.
    pub rank: Option<ServerRank>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WheelYear {
    pub spins: i64,
    pub net: i64,
    pub best: i64,
    pub worst: i64,
    pub bankrupts: i64,
    pub golden: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TipYear {
    pub sent: i64,
    pub sent_jc: i64,
    pub received: i64,
    pub received_jc: i64,
    pub top_recipient: Option<PersonTally>,
}

/// One settled gamble in time order, as the profit curve plots it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GambleEvent {
    pub at: i64,
    pub profit: i64,
    pub kind: GambleKind,
    /// Wagered amount times leverage for bets; the stake otherwise.
    pub exposure: i64,
    pub leverage: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GambleKind {
    Bet,
    Wheel,
    DoubleOrNothing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GambaYear {
    /// Every settled bet, wheel spin, and double-or-nothing this year, oldest
    /// first.
    pub events: Vec<GambleEvent>,
    pub wheel: WheelYear,
    pub tips: TipYear,
    /// Ranked by total wagered on matches.
    pub rank: Option<ServerRank>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_shares_ties_and_ignores_people_who_sat_out() {
        let totals = [(1, 10), (2, 30), (3, 30), (4, 0), (5, 5)];
        assert_eq!(
            ServerRank::among(&totals, 2),
            Some(ServerRank { rank: 1, of: 4 })
        );
        assert_eq!(
            ServerRank::among(&totals, 3),
            Some(ServerRank { rank: 1, of: 4 })
        );
        assert_eq!(
            ServerRank::among(&totals, 5),
            Some(ServerRank { rank: 4, of: 4 })
        );
        assert_eq!(ServerRank::among(&totals, 4), None);
        assert_eq!(ServerRank::among(&totals, 9), None);
    }

    #[test]
    fn ahead_of_percent_needs_company() {
        assert_eq!(ServerRank { rank: 1, of: 1 }.ahead_of_percent(), None);
        assert_eq!(
            ServerRank { rank: 1, of: 5 }.ahead_of_percent(),
            Some(100.0)
        );
        assert_eq!(ServerRank { rank: 5, of: 5 }.ahead_of_percent(), Some(0.0));
        assert_eq!(ServerRank { rank: 2, of: 5 }.ahead_of_percent(), Some(75.0));
    }

    #[test]
    fn traded_in_pets_are_not_mourned() {
        let year = PetYear {
            pets: vec![
                PetRecord {
                    times_fed: 4,
                    died_at: Some(5),
                    death_cause: Some("starvation".into()),
                    ..PetRecord::default()
                },
                PetRecord {
                    times_fed: 6,
                    died_at: Some(9),
                    death_cause: Some("sacrifice".into()),
                    ..PetRecord::default()
                },
                PetRecord {
                    times_fed: 1,
                    is_active: true,
                    ..PetRecord::default()
                },
            ],
            ..PetYear::default()
        };
        assert_eq!(year.lifetime_meals(), 11);
        assert_eq!(year.losses().len(), 1);
    }
}
