//! Canonical Dig DM NPC roster.
//!
//! The Python roster is frozen prompt data: each titled figure has a stable
//! snake-case handle, one of three tone profiles, trigger guidance, and a few
//! sample lines.  Keeping the fields private and exposing only immutable
//! accessors gives the same runtime immutability guarantee as the frozen
//! Python dataclass while remaining a pure, Discord-neutral policy module.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigNpc {
    npc_id: &'static str,
    title: &'static str,
    voice: &'static str,
    triggers: &'static str,
    sample_lines: &'static [&'static str],
}

impl DigNpc {
    #[must_use]
    pub const fn npc_id(self) -> &'static str {
        self.npc_id
    }

    #[must_use]
    pub const fn title(self) -> &'static str {
        self.title
    }

    #[must_use]
    pub const fn voice(self) -> &'static str {
        self.voice
    }

    #[must_use]
    pub const fn triggers(self) -> &'static str {
        self.triggers
    }

    #[must_use]
    pub const fn sample_lines(self) -> &'static [&'static str] {
        self.sample_lines
    }
}

const THE_SURVEYOR_LINES: [&str; 3] = [
    "Shaft 1923 went bad here too. Nothing rebuilt it.",
    "Stone reads the same in any layer. Grain runs east. Yours runs through it.",
    "I marked this passage twenty years ago. Nobody asked me back.",
];

const THE_OLD_HAND_LINES: [&str; 3] = [
    "Walked away from worse. Walk on.",
    "First one's the bad one. Each after, you carry a little less.",
    "You're alright, kid. Keep your hands where you can see them.",
];

const THE_ONE_WHO_COUNTS_LINES: [&str; 3] = [
    "There is a wall somewhere. New marks have appeared on it.",
    "You feel counted. You do not know by what.",
    "Something in the dark has finished tallying. It begins again.",
];

const THE_LISTENER_LINES: [&str; 3] = [
    "The dark has been listening. The dark has been listening.",
    "They say if you hear your own name down here, do not turn.",
    "The names go in. The names do not always come back.",
];

const THE_FOREMAN_LINES: [&str; 3] = [
    "Half of what you dug today, gone before sundown.",
    "You earn it down here. You spend it up there. The arithmetic does the rest.",
    "Nobody's asking how you sleep. They're asking what you owe.",
];

/// Canonical insertion order from Python's dict literal.  It is intentionally
/// a slice rather than a mutable map so roster order and contents are stable.
pub const NPCS: &[DigNpc] = &[
    DigNpc {
        npc_id: "the_surveyor",
        title: "the Surveyor",
        voice: "industrial_grim",
        triggers: "Layer transitions, hesitation at boundaries, depth milestones. Speaks in short fragments about old shafts.",
        sample_lines: &THE_SURVEYOR_LINES,
    },
    DigNpc {
        npc_id: "the_old_hand",
        title: "the Old Hand",
        voice: "industrial_grim",
        triggers: "After cave-ins, after long streaks, after debt or loss. Sympathetic but dry. Never sentimental.",
        sample_lines: &THE_OLD_HAND_LINES,
    },
    DigNpc {
        npc_id: "the_one_who_counts",
        title: "the One Who Counts",
        voice: "cosmic_dread",
        triggers: "Boss boundaries, prestige resets, deep layers. Rarely speaks. Is felt rather than seen. Marks tally on the wall.",
        sample_lines: &THE_ONE_WHO_COUNTS_LINES,
    },
    DigNpc {
        npc_id: "the_listener",
        title: "the Listener",
        voice: "cryptic_folkloric",
        triggers: "Low luminosity, risky-streak digs, after a vow or grudge. Speaks like local superstition. Repeats herself.",
        sample_lines: &THE_LISTENER_LINES,
    },
    DigNpc {
        npc_id: "the_foreman",
        title: "the Foreman",
        voice: "industrial_grim",
        triggers: "Big JC hauls, after debt, near shop activity, after losses to the bombs / bets. Pragmatic, transactional, mocks waste.",
        sample_lines: &THE_FOREMAN_LINES,
    },
];

pub const VALID_VOICES: [&str; 3] = ["cosmic_dread", "industrial_grim", "cryptic_folkloric"];

#[must_use]
pub fn npc_by_id(npc_id: &str) -> Option<DigNpc> {
    NPCS.iter().copied().find(|npc| npc.npc_id() == npc_id)
}

/// Flatten the roster into the exact prompt-injection bullet representation.
#[must_use]
pub fn roster_lines() -> Vec<String> {
    NPCS.iter()
        .map(|npc| {
            format!(
                "- {} ({}, {}): {}",
                npc.npc_id(),
                npc.title(),
                npc.voice(),
                npc.triggers()
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "dig_npc_policy_tests.rs"]
mod tests;
