//! Executable guards for previously regressed command behavior.
//!
//! These contracts stay Discord-neutral: transport operations are emitted as
//! ordered plans, leaderboard names come from a prebuilt guild-member cache,
//! and command behavior is represented through transport-neutral contracts.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadAction {
    Send(String),
    StorePendingThread {
        guild_id: i64,
        pending_match_id: i64,
        thread_id: i64,
    },
    RenameAndArchive {
        name: String,
    },
}

/// Abort is immediate: there is deliberately no delay/sleep action.
#[must_use]
pub fn abort_lobby_thread_plan() -> Vec<ThreadAction> {
    vec![
        ThreadAction::Send("Lobby aborted.".to_owned()),
        ThreadAction::RenameAndArchive {
            name: "Lobby Aborted".to_owned(),
        },
    ]
}

/// Persist the known thread identity before attempting any fallible post.
#[must_use]
pub fn lock_lobby_thread_plan(
    guild_id: i64,
    pending_match_id: i64,
    thread_id: i64,
) -> Vec<ThreadAction> {
    vec![
        ThreadAction::StorePendingThread {
            guild_id,
            pending_match_id,
            thread_id,
        },
        ThreadAction::Send("Record this match when it finishes.".to_owned()),
    ]
}

/// Select only an explicit or recorded-match-scoped thread. An unrelated
/// unscoped pending match is never a fallback.
#[must_use]
pub const fn recorded_match_thread(
    explicit_thread_id: Option<i64>,
    scoped_pending_thread_id: Option<i64>,
) -> Option<i64> {
    match explicit_thread_id {
        Some(thread_id) => Some(thread_id),
        None => scoped_pending_thread_id,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuildMember {
    pub discord_id: i64,
    pub display_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GamblingEntry {
    pub discord_id: i64,
    pub net_pnl: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GamblingLeaderboardView {
    members: BTreeMap<i64, String>,
    entries: Vec<GamblingEntry>,
}

impl GamblingLeaderboardView {
    #[must_use]
    pub fn new(guild_members: Option<&[GuildMember]>, entries: Vec<GamblingEntry>) -> Self {
        let members = guild_members
            .unwrap_or_default()
            .iter()
            .map(|member| (member.discord_id, member.display_name.clone()))
            .collect();
        Self { members, entries }
    }

    #[must_use]
    pub const fn member_cache(&self) -> &BTreeMap<i64, String> {
        &self.members
    }

    /// Synchronous O(log n) cached name lookup; no network-capable port exists.
    #[must_use]
    pub fn get_name(&self, discord_id: i64) -> String {
        self.members
            .get(&discord_id)
            .cloned()
            .unwrap_or_else(|| format!("User {discord_id}"))
    }

    #[must_use]
    pub fn render_lines(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| {
                format!(
                    "{}: {:+} JC",
                    self.get_name(entry.discord_id),
                    entry.net_pnl
                )
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileField {
    pub name: &'static str,
    pub value: String,
    pub inline: bool,
}

impl ProfileField {
    fn named(name: &'static str) -> Self {
        Self {
            name,
            value: "No data".to_owned(),
            inline: true,
        }
    }

    fn spacer() -> Self {
        Self {
            name: "\u{200b}",
            value: "\u{200b}".to_owned(),
            inline: true,
        }
    }
}

/// Four complete three-column rows keep Discord's inline field grid aligned.
#[must_use]
pub fn teammates_field_layout() -> Vec<ProfileField> {
    [
        ("Best Teammates", "Worst Teammates"),
        ("Dominates", "Struggles Against"),
        ("Most Played With", "Most Played Against"),
        ("Even Teammates", "Even Opponents"),
    ]
    .into_iter()
    .flat_map(|(left, right)| {
        [
            ProfileField::named(left),
            ProfileField::named(right),
            ProfileField::spacer(),
        ]
    })
    .collect()
}

#[cfg(test)]
#[path = "bugfix_regressions/tests.rs"]
mod tests;
