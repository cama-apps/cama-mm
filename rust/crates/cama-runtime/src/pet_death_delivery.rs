//! Process-local coordination for direct pet-death announcements.
//!
//! A confirmed `/pet eat` (and altar replacement) owns the public delivery
//! while its durable `death_announced_at` stamp is still unset.  The sweep
//! must therefore skip that pet until the direct interaction either marks the
//! row or releases the guard for a retry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

type DeliveryKey = (PathBuf, i64);

static ACTIVE: OnceLock<Mutex<BTreeMap<DeliveryKey, usize>>> = OnceLock::new();

fn state() -> &'static Mutex<BTreeMap<DeliveryKey, usize>> {
    ACTIVE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn database_key(path: impl AsRef<Path>) -> PathBuf {
    std::fs::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf())
}

/// Return whether a direct death delivery currently owns this pet id.
#[must_use]
pub(crate) fn is_active(path: impl AsRef<Path>, pet_id: i64) -> bool {
    let key = (database_key(path), pet_id);
    state()
        .lock()
        .ok()
        .and_then(|deliveries| deliveries.get(&key).copied())
        .is_some_and(|count| count > 0)
}

/// Increment the process-local direct-delivery lease for a pet.
pub(crate) fn begin(path: impl AsRef<Path>, pet_id: i64) {
    let key = (database_key(path), pet_id);
    if let Ok(mut deliveries) = state().lock() {
        *deliveries.entry(key).or_default() += 1;
    }
}

/// Release one direct-delivery lease. Repeated/concurrent deliveries are
/// reference-counted so a sweep cannot race the final owner.
pub(crate) fn end(path: impl AsRef<Path>, pet_id: i64) {
    let key = (database_key(path), pet_id);
    if let Ok(mut deliveries) = state().lock()
        && let Some(count) = deliveries.get_mut(&key)
    {
        *count = count.saturating_sub(1);
        if *count == 0 {
            deliveries.remove(&key);
        }
    }
}

/// RAII lease for direct delivery. It is intentionally crate-visible so the
/// live provider and the lifecycle worker share the same process-local state.
pub(crate) struct DirectDeathDeliveryGuard {
    database_path: PathBuf,
    pet_id: i64,
}

impl DirectDeathDeliveryGuard {
    pub(crate) fn new(database_path: impl AsRef<Path>, pet_id: i64) -> Self {
        let database_path = database_key(database_path);
        begin(&database_path, pet_id);
        Self {
            database_path,
            pet_id,
        }
    }
}

impl Drop for DirectDeathDeliveryGuard {
    fn drop(&mut self) {
        end(&self.database_path, self.pet_id);
    }
}
