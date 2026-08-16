//! Per-guild configuration service contract.

use crate::guild::normalize_guild_id;

/// One row from Python's `guild_config` table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuildConfig {
    pub guild_id: i64,
    pub league_id: Option<i64>,
    pub auto_enrich_matches: bool,
    pub ai_features_enabled: Option<bool>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// Storage operations required by [`GuildConfigService`].
pub trait GuildConfigStore {
    type Error;

    fn get_config(&self, guild_id: i64) -> Result<Option<GuildConfig>, Self::Error>;
    fn set_league_id(&self, guild_id: i64, league_id: i64) -> Result<(), Self::Error>;
    fn get_league_id(&self, guild_id: i64) -> Result<Option<i64>, Self::Error>;
    fn set_auto_enrich(&self, guild_id: i64, enabled: bool) -> Result<(), Self::Error>;
    fn get_auto_enrich(&self, guild_id: i64) -> Result<bool, Self::Error>;
    fn set_ai_enabled(&self, guild_id: i64, enabled: bool) -> Result<(), Self::Error>;
    fn get_ai_enabled(&self, guild_id: i64) -> Result<bool, Self::Error>;
}

/// Domain-facing service that keeps guild normalization out of callers.
#[derive(Debug)]
pub struct GuildConfigService<S> {
    store: S,
}

impl<S> GuildConfigService<S>
where
    S: GuildConfigStore,
{
    #[must_use]
    pub const fn new(store: S) -> Self {
        Self { store }
    }

    pub fn get_config(&self, guild_id: Option<i64>) -> Result<Option<GuildConfig>, S::Error> {
        self.store.get_config(normalize_guild_id(guild_id))
    }

    pub fn get_league_id(&self, guild_id: Option<i64>) -> Result<Option<i64>, S::Error> {
        self.store.get_league_id(normalize_guild_id(guild_id))
    }

    pub fn set_league_id(&self, guild_id: Option<i64>, league_id: i64) -> Result<(), S::Error> {
        self.store
            .set_league_id(normalize_guild_id(guild_id), league_id)
    }

    pub fn is_auto_enrich_enabled(&self, guild_id: Option<i64>) -> Result<bool, S::Error> {
        self.store.get_auto_enrich(normalize_guild_id(guild_id))
    }

    pub fn set_auto_enrich(&self, guild_id: Option<i64>, enabled: bool) -> Result<(), S::Error> {
        self.store
            .set_auto_enrich(normalize_guild_id(guild_id), enabled)
    }

    pub fn is_ai_enabled(&self, guild_id: Option<i64>) -> Result<bool, S::Error> {
        self.store.get_ai_enabled(normalize_guild_id(guild_id))
    }

    pub fn set_ai_enabled(&self, guild_id: Option<i64>, enabled: bool) -> Result<(), S::Error> {
        self.store
            .set_ai_enabled(normalize_guild_id(guild_id), enabled)
    }

    #[must_use]
    pub const fn store(&self) -> &S {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use super::{GuildConfig, GuildConfigService, GuildConfigStore};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::convert::Infallible;

    const TEST_GUILD_ID: i64 = 12_345;
    const TEST_GUILD_ID_SECONDARY: i64 = 67_890;

    #[derive(Debug, Default)]
    struct MemoryStore {
        rows: RefCell<HashMap<i64, GuildConfig>>,
        ai_default: bool,
    }

    impl MemoryStore {
        fn row(guild_id: i64) -> GuildConfig {
            GuildConfig {
                guild_id,
                league_id: None,
                auto_enrich_matches: true,
                ai_features_enabled: Some(false),
                created_at: None,
                updated_at: None,
            }
        }
    }

    impl GuildConfigStore for MemoryStore {
        type Error = Infallible;

        fn get_config(&self, guild_id: i64) -> Result<Option<GuildConfig>, Self::Error> {
            Ok(self.rows.borrow().get(&guild_id).cloned())
        }

        fn set_league_id(&self, guild_id: i64, league_id: i64) -> Result<(), Self::Error> {
            self.rows
                .borrow_mut()
                .entry(guild_id)
                .or_insert_with(|| Self::row(guild_id))
                .league_id = Some(league_id);
            Ok(())
        }

        fn get_league_id(&self, guild_id: i64) -> Result<Option<i64>, Self::Error> {
            Ok(self
                .rows
                .borrow()
                .get(&guild_id)
                .and_then(|row| row.league_id))
        }

        fn set_auto_enrich(&self, guild_id: i64, enabled: bool) -> Result<(), Self::Error> {
            self.rows
                .borrow_mut()
                .entry(guild_id)
                .or_insert_with(|| Self::row(guild_id))
                .auto_enrich_matches = enabled;
            Ok(())
        }

        fn get_auto_enrich(&self, guild_id: i64) -> Result<bool, Self::Error> {
            Ok(self
                .rows
                .borrow()
                .get(&guild_id)
                .is_none_or(|row| row.auto_enrich_matches))
        }

        fn set_ai_enabled(&self, guild_id: i64, enabled: bool) -> Result<(), Self::Error> {
            self.rows
                .borrow_mut()
                .entry(guild_id)
                .or_insert_with(|| Self::row(guild_id))
                .ai_features_enabled = Some(enabled);
            Ok(())
        }

        fn get_ai_enabled(&self, guild_id: i64) -> Result<bool, Self::Error> {
            Ok(self
                .rows
                .borrow()
                .get(&guild_id)
                .and_then(|row| row.ai_features_enabled)
                .unwrap_or(self.ai_default))
        }
    }

    fn service() -> GuildConfigService<MemoryStore> {
        GuildConfigService::new(MemoryStore::default())
    }

    #[test]
    fn test_get_config_returns_none_for_new_guild() {
        assert_eq!(service().get_config(Some(TEST_GUILD_ID)), Ok(None));
    }

    #[test]
    fn test_set_and_get_league_id() {
        let service = service();
        service.set_league_id(Some(TEST_GUILD_ID), 12_345).unwrap();
        assert_eq!(service.get_league_id(Some(TEST_GUILD_ID)), Ok(Some(12_345)));
    }

    #[test]
    fn test_get_league_id_returns_none_when_not_set() {
        assert_eq!(service().get_league_id(Some(TEST_GUILD_ID)), Ok(None));
    }

    #[test]
    fn test_league_id_per_guild_isolation() {
        let service = service();
        service.set_league_id(Some(TEST_GUILD_ID), 111).unwrap();
        service
            .set_league_id(Some(TEST_GUILD_ID_SECONDARY), 222)
            .unwrap();
        assert_eq!(service.get_league_id(Some(TEST_GUILD_ID)), Ok(Some(111)));
        assert_eq!(
            service.get_league_id(Some(TEST_GUILD_ID_SECONDARY)),
            Ok(Some(222))
        );
    }

    #[test]
    fn test_auto_enrich_defaults_to_true() {
        assert_eq!(
            service().is_auto_enrich_enabled(Some(TEST_GUILD_ID)),
            Ok(true)
        );
    }

    #[test]
    fn test_set_auto_enrich_false() {
        let service = service();
        service.set_auto_enrich(Some(TEST_GUILD_ID), false).unwrap();
        assert_eq!(
            service.is_auto_enrich_enabled(Some(TEST_GUILD_ID)),
            Ok(false)
        );
    }

    #[test]
    fn test_set_auto_enrich_true() {
        let service = service();
        service.set_auto_enrich(Some(TEST_GUILD_ID), false).unwrap();
        service.set_auto_enrich(Some(TEST_GUILD_ID), true).unwrap();
        assert_eq!(
            service.is_auto_enrich_enabled(Some(TEST_GUILD_ID)),
            Ok(true)
        );
    }

    #[test]
    fn test_ai_enabled_defaults_to_config_value() {
        assert!(
            service()
                .is_ai_enabled(Some(TEST_GUILD_ID))
                .is_ok_and(|value| !value)
        );
    }

    #[test]
    fn test_set_ai_enabled() {
        let service = service();
        service.set_ai_enabled(Some(TEST_GUILD_ID), true).unwrap();
        assert_eq!(service.is_ai_enabled(Some(TEST_GUILD_ID)), Ok(true));
        service.set_ai_enabled(Some(TEST_GUILD_ID), false).unwrap();
        assert_eq!(service.is_ai_enabled(Some(TEST_GUILD_ID)), Ok(false));
    }

    #[test]
    fn test_null_guild_id_normalized_to_zero() {
        let service = service();
        service.set_league_id(None, 99_999).unwrap();
        assert_eq!(service.get_league_id(None), Ok(Some(99_999)));
        assert_eq!(service.get_league_id(Some(0)), Ok(Some(99_999)));
    }

    #[test]
    fn test_get_config_after_setting_values() {
        let service = service();
        service.set_league_id(Some(TEST_GUILD_ID), 55_555).unwrap();
        let config = service
            .get_config(Some(TEST_GUILD_ID))
            .unwrap()
            .expect("configured row");
        assert_eq!(config.guild_id, TEST_GUILD_ID);
        assert_eq!(config.league_id, Some(55_555));
    }

    #[test]
    fn test_settings_isolated_between_guilds() {
        let service = service();
        service.set_league_id(Some(TEST_GUILD_ID), 111).unwrap();
        service.set_auto_enrich(Some(TEST_GUILD_ID), false).unwrap();
        service.set_ai_enabled(Some(TEST_GUILD_ID), true).unwrap();

        service
            .set_league_id(Some(TEST_GUILD_ID_SECONDARY), 222)
            .unwrap();
        service
            .set_auto_enrich(Some(TEST_GUILD_ID_SECONDARY), true)
            .unwrap();
        service
            .set_ai_enabled(Some(TEST_GUILD_ID_SECONDARY), false)
            .unwrap();

        assert_eq!(service.get_league_id(Some(TEST_GUILD_ID)), Ok(Some(111)));
        assert_eq!(
            service.is_auto_enrich_enabled(Some(TEST_GUILD_ID)),
            Ok(false)
        );
        assert_eq!(service.is_ai_enabled(Some(TEST_GUILD_ID)), Ok(true));
        assert_eq!(
            service.get_league_id(Some(TEST_GUILD_ID_SECONDARY)),
            Ok(Some(222))
        );
        assert_eq!(
            service.is_auto_enrich_enabled(Some(TEST_GUILD_ID_SECONDARY)),
            Ok(true)
        );
        assert_eq!(
            service.is_ai_enabled(Some(TEST_GUILD_ID_SECONDARY)),
            Ok(false)
        );
    }
}
