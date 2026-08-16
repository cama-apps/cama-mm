//! Offline-safe narration for the Mafia subgame.
//!
//! Python deliberately uses static templates even when an AI service is
//! configured.  Keep that production contract here: every call returns usable
//! copy without network access, while an injected seed makes tests and replay
//! diagnostics deterministic.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::mafia_service::{Game, Player, Role, Twist, Winner};

const SETUP: &[&str] = &[
    "The lobby falls silent as suspicion settles over the night.",
    "Somewhere in this roster, knives are being sharpened.",
    "It's another day in the inhouse — and someone here is not who they claim to be.",
    "The sun sets on the gamba channel. Trust no one until dawn.",
    "Rumors of a plot have reached the village. Eyes narrow.",
    "A new game begins. May the most paranoid town survive.",
    "Tonight, the courier delivers more than wards.",
    "The captain's draft has nothing on this betrayal.",
    "The match is on. The mafia are already among you.",
    "Mid lane is quiet tonight. Too quiet.",
];

const DEATH: &[&str] = &[
    "{hero} ({name}) was found cold at first light. They were a {role}.",
    "{name} ({hero}) didn't make it through the night. {role} confirmed.",
    "The mafia struck. {name}, who carried {hero}, lies dead — a {role}.",
    "{hero} could not save them. {name} ({role}) is no more.",
    "Dawn reveals {name}'s body. Their role: {role}. Their hero: {hero}.",
    "A scream in the dark. A {role} named {name} ({hero}) has fallen.",
    "{name} ({hero}) was hooked into the void. The {role} is gone.",
    "Initiate, ult, dead. {name} ({hero}) — a {role} — has been removed from the game.",
    "{name} got Pudge'd in their sleep. A {role} ({hero}) lost.",
    "{hero} could not buy back from this one. {name} ({role}) is dead.",
    "{name} ({hero}) tabbed out for one second. One second was enough. A {role} falls.",
    "GG no re — {name} ({hero}), a {role}, disconnected from life permanently.",
    "{name} went to tip the courier and never came back. {hero} ({role}), deleted.",
    "Smoke of deceit, a dagger, silence. {name} ({hero}) — {role} — is mid-no-more.",
    "{name} ({hero}) had 1 HP and no salve. The {role} bleeds out at dawn.",
    "They found {name}'s {hero} bottle, still crackling. The {role} is gone.",
    "{name} ({hero}) went all-in on the gamba and the gamba went all-in back. {role}, busted.",
    "Dug too greedily and too deep — {name} ({hero}), a {role}, never resurfaced.",
    "{name} ({hero}) flamed the mafia in all-chat. The mafia muted them. Forever. {role} down.",
    "A perfectly placed ward saw {name} ({hero}) die anyway. {role}, dewarded.",
    "{name} ({hero}) said 'ez' at minute 12. The {role} did not make it to minute 13.",
    "Roshan stirs, the night ends, and {name} ({hero}) — a {role} — does not.",
    "{name} ({hero}) bought back into the wrong neighborhood. {role}, refunded to the void.",
];

const PLAGUE_DEATH: &[&str] = &[
    "The plague claimed {name} ({hero}) before sunrise. A {role} extinguished.",
    "{name} ({hero}) died of fever, not steel. They were a {role}.",
    "Even the doctor could not save {name} ({hero}) from the plague. A {role} lost.",
];

const LYNCH: &[&str] = &[
    "By a show of hands, {name} ({hero}) was hanged from the rax. Their role: {role}.",
    "The town has spoken. {name} ({hero}), a {role}, is executed.",
    "{name} ({hero}) was sent to the well. Confirmed: {role}.",
    "Cleaved at high noon. {name} ({hero}) — {role} — has been lynched.",
    "{name} dies trying to claim innocence. {role} ({hero}) was the truth.",
    "The vote lands like a Reverse Polarity. {name} ({hero}), a {role}, is gone.",
    "Town diff. {name} ({hero}) — {role} — gets reported, votekicked, and lynched.",
    "{name} ({hero}) typed a 6-paragraph defense. The town typed 'F1'. {role} executed.",
    "Pitchforks over jopacoin — {name} ({hero}), a {role}, is strung up at the fountain.",
];

const NO_LYNCH: &[&str] = &[
    "The town could not agree. No one is hanged today.",
    "The vote is tied. The rax stays empty.",
    "Cold feet at the well. No execution today.",
    "Town hall recesses without a verdict.",
];

const TOWN_WIN: &[&str] = &[
    "🏆 The town stands victorious. The mafia have been ground out of mid.",
    "🏆 Roshan is denied. The town wins this one.",
    "🏆 GG: the town has read every gank. The mafia fall.",
    "🏆 Buyback exhausted. The town wins.",
    "🏆 The town's wards held. Every mafia is dead.",
];

const MAFIA_WIN: &[&str] = &[
    "🔪 The mafia have wiped the town. Victory in the dark.",
    "🔪 Throne destroyed. The mafia win.",
    "🔪 The town never stood a chance. Mafia takes the game.",
    "🔪 GG WP — the mafia outnumber the living.",
    "🔪 The mafia walk away from a smoking village. They win.",
];

const JESTER_WIN: &[&str] = &[
    "🃏 The Jester laughs from the grave. They wanted to be lynched — and you obliged.",
    "🃏 You played yourselves. The Jester wins.",
    "🃏 A clown's victory. The Jester gets the last laugh.",
];

const NONE_WIN: &[&str] = &[
    "The dust settles with no clear victor. The game ends in silence.",
    "Neither side could close. The day ends inconclusive.",
];

#[derive(Debug)]
pub struct MafiaFlavorService {
    state: AtomicU64,
}

impl Default for MafiaFlavorService {
    fn default() -> Self {
        Self::new(0x4341_4d41_4d41_4649)
    }
}

impl MafiaFlavorService {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            state: AtomicU64::new(seed),
        }
    }

    fn choose<'a>(&self, choices: &'a [&'a str]) -> &'a str {
        let prior = self
            .state
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(
                    value
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1),
                )
            })
            .unwrap_or_default();
        choices[(prior as usize) % choices.len()]
    }

    #[must_use]
    pub fn setup_narration(&self, game: &Game) -> String {
        let base = self.choose(SETUP);
        let twist = match game.twist {
            Some(Twist::BloodMoon) => {
                "🌑 A blood moon rises. The mafia hungers for two souls tonight."
            }
            Some(Twist::TownHall) => {
                "🏛️ Town hall is in session. No lynch will be permitted today."
            }
            Some(Twist::MemoryFog) => {
                "🌫️ A thick fog rolls in. The detective's notes will fail them tonight."
            }
            Some(Twist::Plague) => {
                "☠️ A plague stalks the village. An additional life will be claimed at dawn."
            }
            Some(Twist::Resurrection) | None => "",
        };
        if twist.is_empty() {
            base.to_owned()
        } else {
            format!("{base}\n{twist}")
        }
    }

    #[must_use]
    pub fn death_narration(&self, victim: &Player, by_plague: bool) -> String {
        let pool = if by_plague { PLAGUE_DEATH } else { DEATH };
        render_player(self.choose(pool), victim)
    }

    #[must_use]
    pub fn lynch_narration(&self, victim: &Player) -> String {
        render_player(self.choose(LYNCH), victim)
    }

    #[must_use]
    pub fn no_lynch_narration(&self) -> String {
        self.choose(NO_LYNCH).to_owned()
    }

    #[must_use]
    pub fn resolution_narration(&self, winner: Winner) -> String {
        let pool = match winner {
            Winner::Town => TOWN_WIN,
            Winner::Mafia => MAFIA_WIN,
            Winner::Jester => JESTER_WIN,
            Winner::None => NONE_WIN,
        };
        self.choose(pool).to_owned()
    }
}

fn render_player(template: &str, victim: &Player) -> String {
    template
        .replace("{hero}", &victim.hero_name)
        .replace("{name}", &format!("<@{}>", victim.discord_id))
        .replace("{role}", role_label(victim.role))
}

const fn role_label(role: Role) -> &'static str {
    match role {
        Role::Mafia => "Mafia",
        Role::Doctor => "Doctor",
        Role::Detective => "Detective",
        Role::Vigilante => "Vigilante",
        Role::Townie => "Townie",
        Role::Jester => "Jester",
        Role::Bookie => "Bookie",
    }
}

#[cfg(test)]
mod tests;
