//! Per-guild profit-withholding policy assembly shared by runtime providers.
//!
//! Every provider that settles generated profit reads the same three inputs:
//! the configured bankruptcy penalty, the guild's vanity-tax roster, and the
//! guild's active low-priority roster. Both rosters are SQLite/cache reads,
//! so [`ProfitDeductionSource::resolve`] must run inside the caller's
//! existing blocking closure, after the interaction is acknowledged. An
//! unreadable roster fails closed: the settlement is refused rather than paid
//! untaxed.

use std::path::PathBuf;
use std::sync::Arc;

use cama_app::service_container::PersistentVanityTaxService;
use cama_db::low_priority_repository::LowPriorityRepository;
use cama_db::profit_deductions::OwnedProfitDeductionPolicy;
use cama_domain::bankruptcy::BankruptcyPenaltyPolicy;

use crate::application_config::{ApplicationConfig, Values};

/// The configured bankruptcy penalty (`BANKRUPTCY_PENALTY_RATE_PER_GAME`).
pub(crate) const fn bankruptcy_penalty_policy(values: &Values) -> BankruptcyPenaltyPolicy {
    BankruptcyPenaltyPolicy {
        rate_per_game: values.bankruptcy_penalty_rate_per_game,
    }
}

/// Configured rates plus the per-guild vanity and low-priority rosters that
/// decide what is withheld from one profit payout.
#[derive(Clone, Debug)]
pub(crate) struct ProfitDeductionSource {
    pub(crate) database_path: PathBuf,
    pub(crate) bankruptcy: BankruptcyPenaltyPolicy,
    pub(crate) vanity_tax_rate: f64,
    pub(crate) low_priority_tax_rate: f64,
    /// Production passes the shared service; test constructors may pass
    /// `None`, which withholds no vanity tax.
    pub(crate) vanity: Option<Arc<PersistentVanityTaxService>>,
}

impl ProfitDeductionSource {
    pub(crate) fn from_config(
        config: &ApplicationConfig,
        database_path: PathBuf,
        vanity: Option<Arc<PersistentVanityTaxService>>,
    ) -> Self {
        Self {
            database_path,
            bankruptcy: bankruptcy_penalty_policy(&config.values),
            vanity_tax_rate: config.values.vanity_tax_rate,
            low_priority_tax_rate: config.values.low_priority_profit_tax_rate,
            vanity,
        }
    }

    /// Resolve both rosters for `guild_id`. Blocking SQLite read; call it
    /// from inside a blocking closure. Fails closed when the low-priority
    /// roster cannot be read.
    pub(crate) fn resolve(&self, guild_id: i64) -> Result<OwnedProfitDeductionPolicy, String> {
        let vanity_taxable_ids = self
            .vanity
            .as_ref()
            .map(|vanity| vanity.taxable_ids(guild_id))
            .unwrap_or_default();
        let low_priority_taxable_ids = LowPriorityRepository::new(&self.database_path)
            .active_taxable_ids(Some(guild_id))
            .map_err(|error| error.to_string())?;
        Ok(OwnedProfitDeductionPolicy {
            bankruptcy: Some(self.bankruptcy),
            vanity_tax_rate: self.vanity_tax_rate,
            vanity_taxable_ids,
            low_priority_tax_rate: self.low_priority_tax_rate,
            low_priority_taxable_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    use super::*;

    const GUILD: i64 = 42;

    fn source(database_path: PathBuf) -> ProfitDeductionSource {
        ProfitDeductionSource {
            database_path,
            bankruptcy: BankruptcyPenaltyPolicy {
                rate_per_game: 0.05,
            },
            vanity_tax_rate: 0.10,
            low_priority_tax_rate: 0.25,
            vanity: None,
        }
    }

    #[test]
    fn resolve_fails_closed_when_the_low_priority_roster_is_unreadable() {
        let missing = std::env::temp_dir().join(format!(
            "cama-profit-deductions-missing-{}.db",
            fastrand::u64(..)
        ));
        let error = source(missing)
            .resolve(GUILD)
            .expect_err("an unreadable roster must not pay untaxed");
        assert!(!error.is_empty());
    }

    #[test]
    fn resolve_reads_the_active_low_priority_roster_for_the_guild() {
        let file = NamedTempFile::new().expect("temp database");
        let connection = Connection::open(file.path()).expect("open");
        connection
            .execute_batch(
                "CREATE TABLE low_priority_state (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL,
                     active INTEGER NOT NULL,
                     wins_remaining INTEGER NOT NULL,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 INSERT INTO low_priority_state VALUES (1, 42, 1, 2);
                 INSERT INTO low_priority_state VALUES (2, 42, 0, 2);
                 INSERT INTO low_priority_state VALUES (3, 42, 1, 0);
                 INSERT INTO low_priority_state VALUES (4, 43, 1, 2);",
            )
            .expect("schema");
        drop(connection);
        let policy = source(file.path().to_path_buf())
            .resolve(GUILD)
            .expect("resolve");
        assert_eq!(
            policy.bankruptcy,
            Some(BankruptcyPenaltyPolicy {
                rate_per_game: 0.05,
            })
        );
        assert_eq!(policy.vanity_tax_rate, 0.10);
        assert_eq!(policy.vanity_taxable_ids, BTreeSet::new());
        assert_eq!(policy.low_priority_tax_rate, 0.25);
        assert_eq!(policy.low_priority_taxable_ids, BTreeSet::from([1]));
    }
}
