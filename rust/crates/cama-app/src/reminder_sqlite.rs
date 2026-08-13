//! Existing-schema SQLite adapter for reminder policy.
//!
//! The adapter is deliberately synchronous. Runtime callers must invoke it
//! from `spawn_blocking`; this keeps every SQLite open/query/write off Tokio's
//! executor while preserving the application service's deterministic API.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::core_repositories::PlayerRepository;
use cama_db::dig_event_threats::ThreatCurseEffects;
use cama_db::mana_service_repository::ManaRepository;
use cama_db::notifications::{NotificationKind, NotificationPreferences, NotificationRepository};
use cama_db::pet_repository::PetRepository;
use cama_db::reminder_repository::{DigReminderAnchor, ReminderRepository};
use cama_domain::game_date::get_game_date;
use cama_domain::mana::ManaEffects;
use cama_domain::pet::UNHATCHED_SPECIES;
use rusqlite::types::Value;
use serde_json::Value as JsonValue;

use crate::dig_event_threats::cursed_cooldown;
use crate::dig_service::cooldown_duration;
use crate::dig_tunnels::{mutation_effects, mutations_from_json};
use crate::reminders::{
    GuildId, PetWarning, PlayerRating, PortError, ReminderKind, ReminderPreferences, ReminderStore,
    ReminderTimestamps, UserId,
};

#[derive(Clone, Debug)]
pub struct SqliteReminderStore {
    notifications: NotificationRepository,
    players: PlayerRepository,
    reminders: ReminderRepository,
    mana: ManaRepository,
    pets: PetRepository,
    pet_hunger_decay_per_day: i64,
}

impl SqliteReminderStore {
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>, pet_hunger_decay_per_day: i64) -> Self {
        let path = database_path.as_ref();
        Self {
            notifications: NotificationRepository::new(path),
            players: PlayerRepository::new(path),
            reminders: ReminderRepository::new(path),
            mana: ManaRepository::new(path),
            pets: PetRepository::new(path),
            pet_hunger_decay_per_day,
        }
    }

    fn dig_ready_times(
        &self,
        user_ids: &[UserId],
        guild_id: GuildId,
        now: i64,
    ) -> Result<BTreeMap<UserId, Option<i64>>, PortError> {
        let raw_ids: Vec<_> = user_ids.iter().map(|user_id| user_id.value()).collect();
        let anchors = self
            .reminders
            .dig_anchors_bulk(&raw_ids, Some(guild_id.value()))
            .map_err(|error| port_error("get_free_dig_ready_times_bulk", error))?;
        let mana = self
            .mana
            .get_all_mana(Some(guild_id.value()))
            .map_err(|error| port_error("get_free_dig_ready_times_bulk", error))?
            .into_iter()
            .map(|row| (row.discord_id, row.mana))
            .collect::<BTreeMap<_, _>>();
        let today = get_game_date();

        Ok(user_ids
            .iter()
            .copied()
            .map(|user_id| {
                let ready_at = anchors
                    .get(&user_id.value())
                    .and_then(Option::as_ref)
                    .map(|anchor| {
                        let mana_reduction = mana
                            .get(&user_id.value())
                            .filter(|row| {
                                row.assigned_date == today
                                    && !row.consumed_today
                                    && row.current_land == "Forest"
                            })
                            .map(|_| ManaEffects::for_color(Some("Green"), Some("Forest")))
                            .map_or(0, |effects| effects.dig_cooldown_reduction_seconds);
                        anchor
                            .last_dig_at
                            .saturating_add(effective_dig_cooldown(anchor, mana_reduction))
                    })
                    .filter(|ready_at| *ready_at > now);
                (user_id, ready_at)
            })
            .collect())
    }
}

impl ReminderStore for SqliteReminderStore {
    fn get_preferences(
        &self,
        user_id: UserId,
        guild_id: GuildId,
    ) -> Result<ReminderPreferences, PortError> {
        self.notifications
            .get_preferences(user_id.value(), Some(guild_id.value()))
            .map(preferences_from_database)
            .map_err(|error| port_error("get_preferences", error))
    }

    fn set_preference(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        kind: ReminderKind,
        enabled: bool,
    ) -> Result<(), PortError> {
        self.notifications
            .set_preference(
                user_id.value(),
                Some(guild_id.value()),
                notification_kind(kind),
                enabled,
                unix_seconds(),
            )
            .map_err(|error| port_error("set_preference", error))
    }

    fn get_enabled_users_for_type(
        &self,
        guild_id: GuildId,
        kind: ReminderKind,
    ) -> Result<Vec<UserId>, PortError> {
        self.notifications
            .enabled_users(Some(guild_id.value()), notification_kind(kind))
            .map(|ids| ids.into_iter().map(UserId::new).collect())
            .map_err(|error| port_error("get_enabled_users_for_type", error))
    }

    fn get_enabled_users_by_type_bulk(
        &self,
        guild_id: GuildId,
        kinds: &[ReminderKind],
    ) -> Result<BTreeMap<ReminderKind, Vec<UserId>>, PortError> {
        let database_kinds: Vec<_> = kinds.iter().copied().map(notification_kind).collect();
        let subscribers = self
            .notifications
            .enabled_users_bulk(Some(guild_id.value()), &database_kinds)
            .map_err(|error| port_error("get_enabled_users_by_type_bulk", error))?;
        Ok(kinds
            .iter()
            .copied()
            .map(|kind| {
                let users = subscribers
                    .get(&notification_kind(kind))
                    .into_iter()
                    .flatten()
                    .copied()
                    .map(UserId::new)
                    .collect();
                (kind, users)
            })
            .collect())
    }

    fn add_lobby_target_subscription(
        &self,
        subscriber_id: UserId,
        target_id: UserId,
        guild_id: GuildId,
        created_at_ns: i64,
    ) -> Result<bool, PortError> {
        self.notifications
            .add_lobby_target_subscription(
                subscriber_id.value(),
                target_id.value(),
                Some(guild_id.value()),
                created_at_ns,
            )
            .map_err(|error| port_error("add_lobby_target_subscription", error))
    }

    fn has_lobby_target_subscription(
        &self,
        subscriber_id: UserId,
        target_id: UserId,
        guild_id: GuildId,
    ) -> Result<bool, PortError> {
        self.notifications
            .has_lobby_target_subscription(
                subscriber_id.value(),
                target_id.value(),
                Some(guild_id.value()),
            )
            .map_err(|error| port_error("has_lobby_target_subscription", error))
    }

    fn claim_lobby_target_subscribers(
        &self,
        target_id: UserId,
        guild_id: GuildId,
        created_at_cutoff_ns: i64,
    ) -> Result<Vec<UserId>, PortError> {
        self.notifications
            .claim_lobby_target_subscribers(
                target_id.value(),
                Some(guild_id.value()),
                created_at_cutoff_ns,
            )
            .map(|ids| ids.into_iter().map(UserId::new).collect())
            .map_err(|error| port_error("claim_lobby_target_subscribers", error))
    }

    fn get_player_ratings(
        &self,
        user_ids: &[UserId],
        guild_id: GuildId,
    ) -> Result<Vec<PlayerRating>, PortError> {
        let raw_ids: Vec<_> = user_ids.iter().map(|user_id| user_id.value()).collect();
        self.players
            .get_by_ids(&raw_ids, Some(guild_id.value()))
            .map(|players| {
                players
                    .into_iter()
                    .filter_map(|player| {
                        player.discord_id.map(|user_id| PlayerRating {
                            user_id: UserId::new(user_id),
                            glicko_rating: player.glicko_rating,
                        })
                    })
                    .collect()
            })
            .map_err(|error| port_error("get_player_ratings", error))
    }

    fn get_reminder_timestamps_bulk(
        &self,
        user_ids: &[UserId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<UserId, ReminderTimestamps>, PortError> {
        let raw_ids: Vec<_> = user_ids.iter().map(|user_id| user_id.value()).collect();
        self.players
            .get_reminder_timestamps_bulk(&raw_ids, Some(guild_id.value()))
            .map(|timestamps| {
                timestamps
                    .into_iter()
                    .map(|(user_id, timestamps)| {
                        (
                            UserId::new(user_id),
                            ReminderTimestamps {
                                last_wheel_spin: optional_timestamp(&timestamps.last_wheel_spin),
                                last_trivia_session: optional_timestamp(
                                    &timestamps.last_trivia_session,
                                ),
                            },
                        )
                    })
                    .collect()
            })
            .map_err(|error| port_error("get_reminder_timestamps_bulk", error))
    }

    fn get_free_dig_ready_at(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        now: i64,
    ) -> Result<Option<i64>, PortError> {
        Ok(self
            .dig_ready_times(&[user_id], guild_id, now)?
            .remove(&user_id)
            .flatten())
    }

    fn get_free_dig_ready_times_bulk(
        &self,
        user_ids: &[UserId],
        guild_id: GuildId,
        now: i64,
    ) -> Result<BTreeMap<UserId, Option<i64>>, PortError> {
        self.dig_ready_times(user_ids, guild_id, now)
    }

    fn get_pet_warning(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        now: i64,
    ) -> Result<Option<PetWarning>, PortError> {
        let pet = self
            .pets
            .get_active_pet(user_id.value(), Some(guild_id.value()))
            .map_err(|error| port_error("get_pet_warning", error))?;
        let Some(pet) = pet.filter(|pet| {
            pet.species != UNHATCHED_SPECIES && now >= pet.hatched_at && pet.died_at.is_none()
        }) else {
            return Ok(None);
        };
        if pet
            .is_starved(now, self.pet_hunger_decay_per_day)
            .map_err(|error| port_error("get_pet_warning", error))?
        {
            return Ok(None);
        }
        let warning_at = pet
            .hunger_crossing_time(
                cama_domain::pet::WARNING_HUNGER,
                self.pet_hunger_decay_per_day,
            )
            .map_err(|error| port_error("get_pet_warning", error))?;
        Ok(Some(PetWarning {
            warning_at,
            pet_name: pet.name,
        }))
    }
}

fn effective_dig_cooldown(anchor: &DigReminderAnchor, mana_reduction_seconds: i64) -> i64 {
    let mutations = mutations_from_json(anchor.mutations_json.as_deref());
    let restless = mutation_effects(&mutations)
        .get("cooldown_bonus_seconds")
        .and_then(|effect| effect.number())
        .is_some_and(|seconds| seconds > 0.0);
    let slower_cooldown = json_string_eq(
        anchor.injury_state_json.as_deref(),
        "type",
        "slower_cooldown",
    );
    let after_stamina = cooldown_duration(anchor.stat_stamina, restless, slower_cooldown, 0);
    let curse = active_cooldown_curse(anchor.temp_curse_json.as_deref());
    cursed_cooldown(after_stamina, curse, mana_reduction_seconds)
}

fn active_cooldown_curse(raw: Option<&str>) -> ThreatCurseEffects {
    let Some(value) = raw.and_then(|raw| serde_json::from_str::<JsonValue>(raw).ok()) else {
        return ThreatCurseEffects::default();
    };
    if value
        .get("digs_remaining")
        .and_then(JsonValue::as_i64)
        .unwrap_or_default()
        <= 0
    {
        return ThreatCurseEffects::default();
    }
    ThreatCurseEffects {
        cooldown_penalty: value
            .get("effect")
            .and_then(|effect| effect.get("cooldown_penalty"))
            .and_then(JsonValue::as_f64),
        ..ThreatCurseEffects::default()
    }
}

fn json_string_eq(raw: Option<&str>, key: &str, expected: &str) -> bool {
    raw.and_then(|raw| serde_json::from_str::<JsonValue>(raw).ok())
        .and_then(|value| {
            value
                .get(key)
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|value| value == expected)
}

fn optional_timestamp(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) => Some(*value),
        Value::Real(value) if value.is_finite() => Some(*value as i64),
        Value::Text(value) => value.parse().ok(),
        Value::Null | Value::Blob(_) | Value::Real(_) => None,
    }
}

fn preferences_from_database(database: NotificationPreferences) -> ReminderPreferences {
    let mut preferences = ReminderPreferences::default();
    for kind in ReminderKind::ALL {
        preferences.set(kind, database.enabled(notification_kind(kind)));
    }
    preferences
}

const fn notification_kind(kind: ReminderKind) -> NotificationKind {
    match kind {
        ReminderKind::Wheel => NotificationKind::Wheel,
        ReminderKind::Trivia => NotificationKind::Trivia,
        ReminderKind::Betting => NotificationKind::Betting,
        ReminderKind::Dig => NotificationKind::Dig,
        ReminderKind::Lobby => NotificationKind::Lobby,
        ReminderKind::Pet => NotificationKind::Pet,
    }
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

fn port_error(operation: &'static str, error: impl std::fmt::Display) -> PortError {
    PortError::new(operation, error.to_string())
}

#[cfg(test)]
#[path = "reminder_sqlite/tests.rs"]
mod tests;
