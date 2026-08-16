//! Pure Camagotchi upbringing and adult-evolution rules.
//!
//! Activity persistence remains outside this module. Given a score snapshot
//! and pet identity, the result is deterministic and mirrors Python's
//! SHA-256 tie breaking exactly.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PetInstinct {
    Delving,
    Competition,
    Fortune,
    Fellowship,
    Wisdom,
}

impl PetInstinct {
    pub const ALL: [Self; 5] = [
        Self::Delving,
        Self::Competition,
        Self::Fortune,
        Self::Fellowship,
        Self::Wisdom,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delving => "delving",
            Self::Competition => "competition",
            Self::Fortune => "fortune",
            Self::Fellowship => "fellowship",
            Self::Wisdom => "wisdom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PetActivity {
    DigCompleted,
    MatchRecorded,
    PetBrawlCompleted,
    DuelCompleted,
    WheelSpun,
    BetPlaced,
    PredictionTraded,
    PetCared,
    TipSent,
    TriviaCompleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PetCalling {
    Delver,
    Champion,
    Trickster,
    Companion,
    Sage,
    Pitfighter,
    Prospector,
    Trailkeeper,
    Archaeologist,
    Daredevil,
    Guardian,
    Tactician,
    Charmer,
    Oracle,
    Mentor,
    Wayfarer,
}

impl PetCalling {
    pub const ALL: [Self; 16] = [
        Self::Delver,
        Self::Champion,
        Self::Trickster,
        Self::Companion,
        Self::Sage,
        Self::Pitfighter,
        Self::Prospector,
        Self::Trailkeeper,
        Self::Archaeologist,
        Self::Daredevil,
        Self::Guardian,
        Self::Tactician,
        Self::Charmer,
        Self::Oracle,
        Self::Mentor,
        Self::Wayfarer,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delver => "delver",
            Self::Champion => "champion",
            Self::Trickster => "trickster",
            Self::Companion => "companion",
            Self::Sage => "sage",
            Self::Pitfighter => "pitfighter",
            Self::Prospector => "prospector",
            Self::Trailkeeper => "trailkeeper",
            Self::Archaeologist => "archaeologist",
            Self::Daredevil => "daredevil",
            Self::Guardian => "guardian",
            Self::Tactician => "tactician",
            Self::Charmer => "charmer",
            Self::Oracle => "oracle",
            Self::Mentor => "mentor",
            Self::Wayfarer => "wayfarer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityRule {
    pub instinct: PetInstinct,
    pub points: i64,
    pub once_per_day: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvolutionDecision {
    pub calling: PetCalling,
    pub primary: Option<PetInstinct>,
    pub secondary: Option<PetInstinct>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallingProfile {
    pub label: &'static str,
    pub emoji: &'static str,
    pub blurb: &'static str,
    pub color: u32,
}

pub type InstinctScores = BTreeMap<PetInstinct, i64>;

const MIN_MEANINGFUL_SCORE: i64 = 3;

#[must_use]
pub const fn activity_rule(activity: PetActivity) -> ActivityRule {
    match activity {
        PetActivity::DigCompleted => ActivityRule::new(PetInstinct::Delving, 2, false),
        PetActivity::MatchRecorded
        | PetActivity::PetBrawlCompleted
        | PetActivity::DuelCompleted => ActivityRule::new(PetInstinct::Competition, 2, false),
        PetActivity::WheelSpun | PetActivity::BetPlaced | PetActivity::PredictionTraded => {
            ActivityRule::new(PetInstinct::Fortune, 2, false)
        }
        PetActivity::PetCared => ActivityRule::new(PetInstinct::Fellowship, 1, true),
        PetActivity::TipSent => ActivityRule::new(PetInstinct::Fellowship, 2, false),
        PetActivity::TriviaCompleted => ActivityRule::new(PetInstinct::Wisdom, 2, false),
    }
}

impl ActivityRule {
    const fn new(instinct: PetInstinct, points: i64, once_per_day: bool) -> Self {
        Self {
            instinct,
            points,
            once_per_day,
        }
    }
}

#[must_use]
pub const fn instinct_label(instinct: PetInstinct) -> &'static str {
    match instinct {
        PetInstinct::Delving => "Delving",
        PetInstinct::Competition => "Competition",
        PetInstinct::Fortune => "Fortune",
        PetInstinct::Fellowship => "Fellowship",
        PetInstinct::Wisdom => "Wisdom",
    }
}

#[must_use]
pub const fn calling_profile(calling: PetCalling) -> CallingProfile {
    match calling {
        PetCalling::Delver => CallingProfile::new(
            "Delver",
            "⛏️",
            "Hears hidden passages in every patch of stone.",
            0xB87333,
        ),
        PetCalling::Champion => CallingProfile::new(
            "Champion",
            "🏆",
            "Greets every challenge with bright eyes and planted hooves.",
            0xD94A4A,
        ),
        PetCalling::Trickster => CallingProfile::new(
            "Trickster",
            "🎲",
            "Treats chance as a dance partner and somehow knows the steps.",
            0xE3B341,
        ),
        PetCalling::Companion => CallingProfile::new(
            "Companion",
            "💞",
            "Keeps close to the herd and makes every shared road warmer.",
            0xE77FA1,
        ),
        PetCalling::Sage => CallingProfile::new(
            "Sage",
            "📚",
            "Collects questions carefully and answers them at inconvenient moments.",
            0x8067C6,
        ),
        PetCalling::Pitfighter => CallingProfile::new(
            "Pitfighter",
            "⚔️",
            "Found courage underground and brought it back sharpened.",
            0xC45A36,
        ),
        PetCalling::Prospector => CallingProfile::new(
            "Prospector",
            "✨",
            "Follows glitter, echoes, and extremely persuasive hunches.",
            0xD69A32,
        ),
        PetCalling::Trailkeeper => CallingProfile::new(
            "Trailkeeper",
            "🏕️",
            "Leaves every dark road safer for the herd behind.",
            0x8D8650,
        ),
        PetCalling::Archaeologist => CallingProfile::new(
            "Archaeologist",
            "🏺",
            "Digs patiently for stories the stone nearly forgot.",
            0x9B6D54,
        ),
        PetCalling::Daredevil => CallingProfile::new(
            "Daredevil",
            "🔥",
            "Believes the safest odds are the ones already sprinting away.",
            0xE05A3F,
        ),
        PetCalling::Guardian => CallingProfile::new(
            "Guardian",
            "🛡️",
            "Turns fierce the instant a friend needs shelter.",
            0xC35B70,
        ),
        PetCalling::Tactician => CallingProfile::new(
            "Tactician",
            "♟️",
            "Has considered the next move, and several snacks beyond it.",
            0x8A5EA8,
        ),
        PetCalling::Charmer => CallingProfile::new(
            "Charmer",
            "🌟",
            "Makes lucky breaks feel suspiciously like introductions.",
            0xE29A70,
        ),
        PetCalling::Oracle => CallingProfile::new(
            "Oracle",
            "🔮",
            "Reads tomorrow in patterns everyone else calls coincidence.",
            0x9A70C8,
        ),
        PetCalling::Mentor => CallingProfile::new(
            "Mentor",
            "🪶",
            "Always has a lesson, a listening ear, and emergency hay.",
            0x9A76B6,
        ),
        PetCalling::Wayfarer => CallingProfile::new(
            "Wayfarer",
            "🧭",
            "Carries a little of every path and belongs wherever it wanders.",
            0x5DADE2,
        ),
    }
}

impl CallingProfile {
    const fn new(
        label: &'static str,
        emoji: &'static str,
        blurb: &'static str,
        color: u32,
    ) -> Self {
        Self {
            label,
            emoji,
            blurb,
            color,
        }
    }
}

#[must_use]
pub fn hint_text(key: Option<&str>) -> Option<&'static str> {
    match key {
        Some("balanced") => Some("Every path seems to hold a little wonder."),
        Some("delving") => Some("Loose stones keep attracting curious little sniffs."),
        Some("delving_strong") => Some("The deep has started appearing in very serious dreams."),
        Some("competition") => Some("Any friendly challenge earns an immediate, eager stance."),
        Some("competition_strong") => Some("Every room looks like an arena waiting to happen."),
        Some("fortune") => Some("Coincidence keeps getting an expectant little side-eye."),
        Some("fortune_strong") => Some("Lucky omens are being collected with alarming confidence."),
        Some("fellowship") => Some("The herd is never allowed to wander too far away."),
        Some("fellowship_strong") => Some("Looking after friends has become a full-time vocation."),
        Some("wisdom") => Some("Every new fact receives a thoughtful, woolly pause."),
        Some("wisdom_strong") => Some("Questions are piling up faster than the snack supply."),
        None | Some(_) => None,
    }
}

#[must_use]
pub fn resolve_evolution(scores: &InstinctScores, pet_id: i64) -> EvolutionDecision {
    let mut ranked = PetInstinct::ALL.map(|instinct| {
        (
            instinct,
            scores.get(&instinct).copied().unwrap_or_default().max(0),
            tie_rank(pet_id, instinct),
        )
    });
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then(left.2.cmp(&right.2)));

    let (primary, top, _) = ranked[0];
    let (secondary, second, _) = ranked[1];
    let (_, third, _) = ranked[2];

    if top < MIN_MEANINGFUL_SCORE || is_balanced(top, third) {
        return EvolutionDecision {
            calling: PetCalling::Wayfarer,
            primary: None,
            secondary: None,
        };
    }

    if second >= MIN_MEANINGFUL_SCORE && second * 2 >= top {
        return EvolutionDecision {
            calling: hybrid_calling(primary, secondary),
            primary: Some(primary),
            secondary: Some(secondary),
        };
    }

    EvolutionDecision {
        calling: pure_calling(primary),
        primary: Some(primary),
        secondary: None,
    }
}

#[must_use]
pub fn hint_key_for_scores(scores: &InstinctScores) -> Option<String> {
    let mut ranked = PetInstinct::ALL.map(|instinct| {
        (
            instinct,
            scores.get(&instinct).copied().unwrap_or_default().max(0),
        )
    });
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then(left.0.as_str().cmp(right.0.as_str()))
    });
    let (primary, top) = ranked[0];
    let second = ranked[1].1;
    let third = ranked[2].1;
    if top < MIN_MEANINGFUL_SCORE {
        None
    } else if is_balanced(top, third) {
        Some("balanced".to_owned())
    } else if top >= 6 && top - second >= 3 {
        Some(format!("{}_strong", primary.as_str()))
    } else {
        Some(primary.as_str().to_owned())
    }
}

const fn is_balanced(top: i64, third: i64) -> bool {
    third >= MIN_MEANINGFUL_SCORE && third * 4 >= top * 3
}

const fn pure_calling(instinct: PetInstinct) -> PetCalling {
    match instinct {
        PetInstinct::Delving => PetCalling::Delver,
        PetInstinct::Competition => PetCalling::Champion,
        PetInstinct::Fortune => PetCalling::Trickster,
        PetInstinct::Fellowship => PetCalling::Companion,
        PetInstinct::Wisdom => PetCalling::Sage,
    }
}

fn hybrid_calling(left: PetInstinct, right: PetInstinct) -> PetCalling {
    use PetCalling as Calling;
    use PetInstinct as Instinct;
    match (left, right) {
        (Instinct::Delving, Instinct::Competition) | (Instinct::Competition, Instinct::Delving) => {
            Calling::Pitfighter
        }
        (Instinct::Delving, Instinct::Fortune) | (Instinct::Fortune, Instinct::Delving) => {
            Calling::Prospector
        }
        (Instinct::Delving, Instinct::Fellowship) | (Instinct::Fellowship, Instinct::Delving) => {
            Calling::Trailkeeper
        }
        (Instinct::Delving, Instinct::Wisdom) | (Instinct::Wisdom, Instinct::Delving) => {
            Calling::Archaeologist
        }
        (Instinct::Competition, Instinct::Fortune) | (Instinct::Fortune, Instinct::Competition) => {
            Calling::Daredevil
        }
        (Instinct::Competition, Instinct::Fellowship)
        | (Instinct::Fellowship, Instinct::Competition) => Calling::Guardian,
        (Instinct::Competition, Instinct::Wisdom) | (Instinct::Wisdom, Instinct::Competition) => {
            Calling::Tactician
        }
        (Instinct::Fortune, Instinct::Fellowship) | (Instinct::Fellowship, Instinct::Fortune) => {
            Calling::Charmer
        }
        (Instinct::Fortune, Instinct::Wisdom) | (Instinct::Wisdom, Instinct::Fortune) => {
            Calling::Oracle
        }
        (Instinct::Fellowship, Instinct::Wisdom) | (Instinct::Wisdom, Instinct::Fellowship) => {
            Calling::Mentor
        }
        _ => unreachable!("hybrid resolution always receives two distinct instincts"),
    }
}

fn tie_rank(pet_id: i64, instinct: PetInstinct) -> [u8; 32] {
    sha256(format!("{pet_id}:{}", instinct.as_str()).as_bytes())
}

fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let bit_length = (input.len() as u64) * 8;
    let mut message = input.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL;
    for chunk in message.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, bytes) in chunk.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let upper_e = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temporary1 = h
                .wrapping_add(upper_e)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let upper_a = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temporary2 = upper_a.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temporary1);
            d = c;
            c = b;
            b = a;
            a = temporary1.wrapping_add(temporary2);
        }
        for (current, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *current = current.wrapping_add(value);
        }
    }

    let mut digest = [0_u8; 32];
    for (bytes, value) in digest.chunks_exact_mut(4).zip(state) {
        bytes.copy_from_slice(&value.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn scores(entries: &[(PetInstinct, i64)]) -> InstinctScores {
        entries.iter().copied().collect()
    }

    fn assert_pure(instinct: PetInstinct, calling: PetCalling) {
        let other = if instinct == PetInstinct::Fellowship {
            PetInstinct::Delving
        } else {
            PetInstinct::Fellowship
        };
        let decision = resolve_evolution(&scores(&[(instinct, 10), (other, 4)]), 101);
        assert_eq!(decision.calling, calling);
        assert_eq!(decision.primary, Some(instinct));
        assert_eq!(decision.secondary, None);
    }

    macro_rules! pure_test {
        ($name:ident, $instinct:ident, $calling:ident) => {
            #[test]
            fn $name() {
                assert_pure(PetInstinct::$instinct, PetCalling::$calling);
            }
        };
    }

    pure_test!(
        test_one_dominant_instinct_resolves_to_its_pure_calling_delving_delver,
        Delving,
        Delver
    );
    pure_test!(
        test_one_dominant_instinct_resolves_to_its_pure_calling_competition_champion,
        Competition,
        Champion
    );
    pure_test!(
        test_one_dominant_instinct_resolves_to_its_pure_calling_fortune_trickster,
        Fortune,
        Trickster
    );
    pure_test!(
        test_one_dominant_instinct_resolves_to_its_pure_calling_fellowship_companion,
        Fellowship,
        Companion
    );
    pure_test!(
        test_one_dominant_instinct_resolves_to_its_pure_calling_wisdom_sage,
        Wisdom,
        Sage
    );

    fn assert_hybrid(left: PetInstinct, right: PetInstinct, calling: PetCalling) {
        let decision = resolve_evolution(&scores(&[(left, 10), (right, 6)]), 202);
        assert_eq!(decision.calling, calling);
        assert_eq!(
            BTreeSet::from_iter([decision.primary.unwrap(), decision.secondary.unwrap()]),
            BTreeSet::from_iter([left, right])
        );
    }

    macro_rules! hybrid_test {
        ($name:ident, $left:ident, $right:ident, $calling:ident) => {
            #[test]
            fn $name() {
                assert_hybrid(
                    PetInstinct::$left,
                    PetInstinct::$right,
                    PetCalling::$calling,
                );
            }
        };
    }

    hybrid_test!(
        test_two_substantial_instincts_resolve_to_their_hybrid_calling_delving_competition_pitfighter,
        Delving,
        Competition,
        Pitfighter
    );
    hybrid_test!(
        test_two_substantial_instincts_resolve_to_their_hybrid_calling_delving_fortune_prospector,
        Delving,
        Fortune,
        Prospector
    );
    hybrid_test!(
        test_two_substantial_instincts_resolve_to_their_hybrid_calling_delving_fellowship_trailkeeper,
        Delving,
        Fellowship,
        Trailkeeper
    );
    hybrid_test!(
        test_two_substantial_instincts_resolve_to_their_hybrid_calling_delving_wisdom_archaeologist,
        Delving,
        Wisdom,
        Archaeologist
    );
    hybrid_test!(
        test_two_substantial_instincts_resolve_to_their_hybrid_calling_competition_fortune_daredevil,
        Competition,
        Fortune,
        Daredevil
    );
    hybrid_test!(
        test_two_substantial_instincts_resolve_to_their_hybrid_calling_competition_fellowship_guardian,
        Competition,
        Fellowship,
        Guardian
    );
    hybrid_test!(
        test_two_substantial_instincts_resolve_to_their_hybrid_calling_competition_wisdom_tactician,
        Competition,
        Wisdom,
        Tactician
    );
    hybrid_test!(
        test_two_substantial_instincts_resolve_to_their_hybrid_calling_fortune_fellowship_charmer,
        Fortune,
        Fellowship,
        Charmer
    );
    hybrid_test!(
        test_two_substantial_instincts_resolve_to_their_hybrid_calling_fortune_wisdom_oracle,
        Fortune,
        Wisdom,
        Oracle
    );
    hybrid_test!(
        test_two_substantial_instincts_resolve_to_their_hybrid_calling_fellowship_wisdom_mentor,
        Fellowship,
        Wisdom,
        Mentor
    );

    #[test]
    fn test_no_meaningful_activity_resolves_to_wayfarer() {
        assert_eq!(
            resolve_evolution(&scores(&[]), 1).calling,
            PetCalling::Wayfarer
        );
        assert_eq!(
            resolve_evolution(&scores(&[(PetInstinct::Delving, 2)]), 1).calling,
            PetCalling::Wayfarer
        );
    }

    #[test]
    fn test_three_balanced_instincts_resolve_to_wayfarer() {
        let decision = resolve_evolution(
            &scores(&[
                (PetInstinct::Delving, 8),
                (PetInstinct::Competition, 7),
                (PetInstinct::Wisdom, 6),
            ]),
            303,
        );
        assert_eq!(
            decision,
            EvolutionDecision {
                calling: PetCalling::Wayfarer,
                primary: None,
                secondary: None,
            }
        );
    }

    #[test]
    fn test_exact_two_way_tie_is_stable_and_remains_a_hybrid() {
        let upbringing = scores(&[(PetInstinct::Delving, 6), (PetInstinct::Fortune, 6)]);
        let first = resolve_evolution(&upbringing, 404);
        assert_eq!(first, resolve_evolution(&upbringing, 404));
        assert_eq!(first.calling, PetCalling::Prospector);
        assert_eq!(
            BTreeSet::from_iter([first.primary.unwrap(), first.secondary.unwrap()]),
            BTreeSet::from_iter([PetInstinct::Delving, PetInstinct::Fortune])
        );
    }

    fn assert_activity(
        activity: PetActivity,
        instinct: PetInstinct,
        points: i64,
        once_per_day: bool,
    ) {
        assert_eq!(
            activity_rule(activity),
            ActivityRule {
                instinct,
                points,
                once_per_day,
            }
        );
    }

    macro_rules! activity_test {
        ($name:ident, $activity:ident, $instinct:ident, $points:literal, $once:literal) => {
            #[test]
            fn $name() {
                assert_activity(
                    PetActivity::$activity,
                    PetInstinct::$instinct,
                    $points,
                    $once,
                );
            }
        };
    }

    activity_test!(
        test_activity_rules_are_normalized_by_participation_dig_completed_delving_2_false,
        DigCompleted,
        Delving,
        2,
        false
    );
    activity_test!(
        test_activity_rules_are_normalized_by_participation_match_recorded_competition_2_false,
        MatchRecorded,
        Competition,
        2,
        false
    );
    activity_test!(
        test_activity_rules_are_normalized_by_participation_pet_brawl_completed_competition_2_false,
        PetBrawlCompleted,
        Competition,
        2,
        false
    );
    activity_test!(
        test_activity_rules_are_normalized_by_participation_duel_completed_competition_2_false,
        DuelCompleted,
        Competition,
        2,
        false
    );
    activity_test!(
        test_activity_rules_are_normalized_by_participation_wheel_spun_fortune_2_false,
        WheelSpun,
        Fortune,
        2,
        false
    );
    activity_test!(
        test_activity_rules_are_normalized_by_participation_bet_placed_fortune_2_false,
        BetPlaced,
        Fortune,
        2,
        false
    );
    activity_test!(
        test_activity_rules_are_normalized_by_participation_prediction_traded_fortune_2_false,
        PredictionTraded,
        Fortune,
        2,
        false
    );
    activity_test!(
        test_activity_rules_are_normalized_by_participation_pet_cared_fellowship_1_true,
        PetCared,
        Fellowship,
        1,
        true
    );
    activity_test!(
        test_activity_rules_are_normalized_by_participation_tip_sent_fellowship_2_false,
        TipSent,
        Fellowship,
        2,
        false
    );
    activity_test!(
        test_activity_rules_are_normalized_by_participation_trivia_completed_wisdom_2_false,
        TriviaCompleted,
        Wisdom,
        2,
        false
    );

    #[test]
    fn test_hints_stay_hidden_until_activity_is_meaningful() {
        assert_eq!(
            hint_key_for_scores(&scores(&[(PetInstinct::Delving, 2)])),
            None
        );
        assert_eq!(
            hint_key_for_scores(&scores(&[(PetInstinct::Delving, 3)])).as_deref(),
            Some("delving")
        );
        assert_eq!(
            hint_key_for_scores(&scores(&[
                (PetInstinct::Delving, 8),
                (PetInstinct::Competition, 1),
            ]))
            .as_deref(),
            Some("delving_strong")
        );
        assert_eq!(
            hint_key_for_scores(&scores(&[
                (PetInstinct::Delving, 8),
                (PetInstinct::Competition, 7),
                (PetInstinct::Wisdom, 6),
            ]))
            .as_deref(),
            Some("balanced")
        );
    }

    #[test]
    fn test_every_calling_has_distinct_player_facing_identity() {
        let labels: BTreeSet<_> = PetCalling::ALL
            .into_iter()
            .map(|calling| calling_profile(calling).label)
            .collect();
        assert_eq!(labels.len(), PetCalling::ALL.len());
        for calling in PetCalling::ALL {
            let profile = calling_profile(calling);
            assert!(!profile.label.is_empty());
            assert!(!profile.emoji.is_empty());
            assert!(!profile.blurb.is_empty());
            assert!(profile.color > 0);
        }
    }

    fn assert_narrative_hint(key: &str) {
        let text = hint_text(Some(key)).expect("known hint key");
        assert!(!text.is_empty());
        assert!(!text.chars().any(|character| character.is_ascii_digit()));
    }

    macro_rules! hint_test {
        ($name:ident, $key:literal) => {
            #[test]
            fn $name() {
                assert_narrative_hint($key);
            }
        };
    }

    hint_test!(
        test_upbringing_hints_are_narrative_and_hide_scores_balanced,
        "balanced"
    );
    hint_test!(
        test_upbringing_hints_are_narrative_and_hide_scores_delving,
        "delving"
    );
    hint_test!(
        test_upbringing_hints_are_narrative_and_hide_scores_delving_strong,
        "delving_strong"
    );
    hint_test!(
        test_upbringing_hints_are_narrative_and_hide_scores_competition,
        "competition"
    );
    hint_test!(
        test_upbringing_hints_are_narrative_and_hide_scores_competition_strong,
        "competition_strong"
    );
    hint_test!(
        test_upbringing_hints_are_narrative_and_hide_scores_fortune,
        "fortune"
    );
    hint_test!(
        test_upbringing_hints_are_narrative_and_hide_scores_fortune_strong,
        "fortune_strong"
    );
    hint_test!(
        test_upbringing_hints_are_narrative_and_hide_scores_fellowship,
        "fellowship"
    );
    hint_test!(
        test_upbringing_hints_are_narrative_and_hide_scores_fellowship_strong,
        "fellowship_strong"
    );
    hint_test!(
        test_upbringing_hints_are_narrative_and_hide_scores_wisdom,
        "wisdom"
    );
    hint_test!(
        test_upbringing_hints_are_narrative_and_hide_scores_wisdom_strong,
        "wisdom_strong"
    );
}
