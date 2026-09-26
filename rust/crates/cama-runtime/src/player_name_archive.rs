//! Last-seen Discord display names, mirrored in memory and persisted to
//! SQLite.
//!
//! Every live name the gateway or a member fetch produces is recorded here, so
//! a player Discord cannot name later (they left the server, a lookup failed,
//! or the member list is still loading after a restart) renders under the
//! last name the bot saw instead of `Unknown player`. Reads never touch
//! SQLite; writes happen only when a name changes and run on the blocking pool.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::player_display_names::{PlayerDisplayName, PlayerDisplayNameRepository};
use cama_domain::discord_content::readable_player_name;
use tracing::warn;

use crate::discord_transport::DiscordGuildMemberRenderNames;

#[derive(Debug, Default)]
pub struct PlayerNameArchive {
    names: RwLock<BTreeMap<(u64, u64), String>>,
    repository: Option<PlayerDisplayNameRepository>,
}

impl PlayerNameArchive {
    /// Load every stored name. Blocking; call during startup composition.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let repository = PlayerDisplayNameRepository::new(path);
        let names = repository
            .load_all()
            .map_err(|error| format!("could not load player display names: {error}"))?
            .into_iter()
            .filter_map(|stored| {
                let guild_id = u64::try_from(stored.guild_id).ok()?;
                let user_id = u64::try_from(stored.discord_id).ok()?;
                Some(((guild_id, user_id), stored.display_name))
            })
            .collect();
        Ok(Self {
            names: RwLock::new(names),
            repository: Some(repository),
        })
    }

    #[must_use]
    pub fn names(&self, guild_id: u64, user_ids: &[u64]) -> DiscordGuildMemberRenderNames {
        let Ok(names) = self.names.read() else {
            return DiscordGuildMemberRenderNames::new();
        };
        user_ids
            .iter()
            .filter_map(|user_id| {
                names
                    .get(&(guild_id, *user_id))
                    .map(|name| (*user_id, name.clone()))
            })
            .collect()
    }

    /// Remember live names. Unreadable names (blank, numeric IDs, mention
    /// markup) are ignored so they never replace a good stored name.
    pub fn record(&self, guild_id: u64, names: impl IntoIterator<Item = (u64, String)>) {
        if guild_id == 0 {
            return;
        }
        let changed = {
            let Ok(mut stored) = self.names.write() else {
                return;
            };
            names
                .into_iter()
                .filter_map(|(user_id, name)| {
                    let name = readable_player_name(&name)?.to_owned();
                    let key = (guild_id, user_id);
                    if stored.get(&key) == Some(&name) {
                        return None;
                    }
                    stored.insert(key, name.clone());
                    Some(PlayerDisplayName {
                        discord_id: i64::try_from(user_id).ok()?,
                        guild_id: i64::try_from(guild_id).ok()?,
                        display_name: name,
                    })
                })
                .collect::<Vec<_>>()
        };
        if changed.is_empty() {
            return;
        }
        let Some(repository) = self.repository.clone() else {
            return;
        };
        let seen_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            });
        let persist = move || {
            if let Err(error) = repository.record(&changed, seen_at) {
                warn!(%error, count = changed.len(), "could not store player display names");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn_blocking(persist);
            }
            Err(_) => persist(),
        }
    }
}

#[cfg(test)]
#[path = "player_name_archive/tests.rs"]
mod tests;
