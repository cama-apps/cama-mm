//! Runtime SQLite connections with the canonical policy and optional reuse.
//!
//! Opening a SQLite connection costs far more than a typical repository
//! query, and repositories open one per call. A database registered with
//! [`RuntimeConnectionReuse`] keeps a small idle pool so later calls skip the
//! open. Every other path (temporary test databases, disposable copies, audit
//! targets) opens and closes a connection per call exactly as before, so no
//! handle outlives its caller on those files.
//!
//! A pooled connection is handed out only in autocommit mode and has the
//! runtime policy (5-second busy timeout, foreign keys off) re-applied on
//! every checkout, so no caller can observe another caller's transaction or
//! policy change.

use std::collections::HashMap;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// Idle connections kept per registered database.
const MAX_IDLE_CONNECTIONS: usize = 16;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

struct IdleConnections {
    enabled: AtomicBool,
    idle: Mutex<Vec<Connection>>,
}

impl IdleConnections {
    fn idle(&self) -> MutexGuard<'_, Vec<Connection>> {
        self.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn take(&self) -> Option<Connection> {
        self.idle().pop()
    }

    fn give_back(&self, connection: Connection) {
        // A connection left inside a transaction is closed, which rolls the
        // transaction back, instead of leaking it to the next caller.
        if !connection.is_autocommit() {
            return;
        }
        let mut idle = self.idle();
        if self.enabled.load(Ordering::Acquire) && idle.len() < MAX_IDLE_CONNECTIONS {
            idle.push(connection);
        }
    }
}

struct Registration {
    pool: Arc<IdleConnections>,
    guards: usize,
}

fn registry() -> MutexGuard<'static, HashMap<PathBuf, Registration>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Registration>>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Keeps idle runtime connections to one database for reuse until dropped.
///
/// The production composition root holds one for its database for the life
/// of the process. Paths are matched exactly as given to
/// [`open_runtime_connection`]. Dropping the last guard for a path closes its
/// idle connections; connections checked out at that moment close when their
/// caller drops them.
#[must_use = "connection reuse ends when the guard is dropped"]
pub struct RuntimeConnectionReuse {
    path: PathBuf,
}

impl RuntimeConnectionReuse {
    pub fn enable(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        registry()
            .entry(path.clone())
            .and_modify(|registration| registration.guards += 1)
            .or_insert_with(|| Registration {
                pool: Arc::new(IdleConnections {
                    enabled: AtomicBool::new(true),
                    idle: Mutex::new(Vec::new()),
                }),
                guards: 1,
            });
        Self { path }
    }

    /// Idle connections currently kept for this database.
    #[must_use]
    pub fn idle_connections(&self) -> usize {
        registry()
            .get(&self.path)
            .map_or(0, |registration| registration.pool.idle().len())
    }
}

impl Drop for RuntimeConnectionReuse {
    fn drop(&mut self) {
        let pool = {
            let mut registry = registry();
            let Some(registration) = registry.get_mut(&self.path) else {
                return;
            };
            registration.guards -= 1;
            if registration.guards > 0 {
                return;
            }
            registry
                .remove(&self.path)
                .map(|registration| registration.pool)
        };
        if let Some(pool) = pool {
            let mut idle = pool.idle();
            pool.enabled.store(false, Ordering::Release);
            idle.clear();
        }
    }
}

impl fmt::Debug for RuntimeConnectionReuse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeConnectionReuse")
            .field("path", &self.path)
            .finish()
    }
}

/// A repository connection with the canonical runtime policy.
///
/// It dereferences to [`rusqlite::Connection`]. When its database is
/// registered for reuse, dropping it returns the connection to the idle pool
/// if no transaction is open.
pub struct RuntimeConnection {
    connection: Option<Connection>,
    pool: Option<Arc<IdleConnections>>,
}

impl Deref for RuntimeConnection {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.connection
            .as_ref()
            .expect("runtime connection is present until dropped")
    }
}

impl DerefMut for RuntimeConnection {
    fn deref_mut(&mut self) -> &mut Connection {
        self.connection
            .as_mut()
            .expect("runtime connection is present until dropped")
    }
}

impl Drop for RuntimeConnection {
    fn drop(&mut self) {
        if let (Some(connection), Some(pool)) = (self.connection.take(), self.pool.take()) {
            pool.give_back(connection);
        }
    }
}

impl fmt::Debug for RuntimeConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeConnection")
            .field("pooled", &self.pool.is_some())
            .finish_non_exhaustive()
    }
}

fn apply_runtime_policy(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", false)
}

/// Open an existing database for normal repository reads and writes.
///
/// Schema creation and migration are handled by the startup schema manager,
/// never by repository construction. The explicit foreign-key pragma enforces
/// the canonical runtime setting (`OFF`) regardless of bundled SQLite defaults.
#[doc(hidden)]
pub fn open_runtime_connection(path: &Path) -> Result<RuntimeConnection, rusqlite::Error> {
    let pool = registry()
        .get(path)
        .map(|registration| Arc::clone(&registration.pool));
    if let Some(pool) = &pool {
        while let Some(connection) = pool.take() {
            if apply_runtime_policy(&connection).is_ok() {
                return Ok(RuntimeConnection {
                    connection: Some(connection),
                    pool: Some(Arc::clone(pool)),
                });
            }
        }
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    apply_runtime_policy(&connection)?;
    Ok(RuntimeConnection {
        connection: Some(connection),
        pool,
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::TransactionBehavior;
    use tempfile::NamedTempFile;

    use super::*;

    fn database() -> NamedTempFile {
        let file = NamedTempFile::new().expect("temporary database");
        crate::test_support::copy_migrated_database(file.path()).expect("migrated database");
        file
    }

    #[test]
    fn unregistered_paths_never_keep_a_connection_open() {
        let file = database();
        drop(open_runtime_connection(file.path()).unwrap());
        // Nothing holds the file, so it can be removed (Windows refuses to
        // delete a file that any SQLite handle still has open).
        let path = file.path().to_path_buf();
        file.close().expect("remove the database");
        assert!(!path.exists());
    }

    #[test]
    fn registered_paths_reuse_idle_connections_with_the_runtime_policy() {
        let file = database();
        let reuse = RuntimeConnectionReuse::enable(file.path());
        {
            let connection = open_runtime_connection(file.path()).unwrap();
            connection
                .execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=1;")
                .unwrap();
            connection
                .execute(
                    "INSERT INTO app_kv(guild_id,key,value) VALUES (1,'reuse','first')",
                    [],
                )
                .unwrap();
        }
        assert_eq!(reuse.idle_connections(), 1);
        let connection = open_runtime_connection(file.path()).unwrap();
        assert_eq!(
            reuse.idle_connections(),
            0,
            "the idle connection was reused"
        );
        let policy: (i64, i64) = connection
            .query_row(
                "SELECT * FROM pragma_busy_timeout, pragma_foreign_keys",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(policy, (5_000, 0), "a checkout restores the runtime policy");
        let second = open_runtime_connection(file.path()).unwrap();
        drop((connection, second));
        assert_eq!(reuse.idle_connections(), 2);
        drop(reuse);
        // Dropping the guard closed every idle handle on the file.
        let path = file.path().to_path_buf();
        file.close().expect("remove the database");
        assert!(!path.exists());
    }

    #[test]
    fn open_transactions_are_never_returned_to_the_pool() {
        let file = database();
        let reuse = RuntimeConnectionReuse::enable(file.path());
        {
            let connection = open_runtime_connection(file.path()).unwrap();
            connection
                .execute_batch(
                    "BEGIN IMMEDIATE;
                     INSERT INTO app_kv(guild_id,key,value) VALUES (1,'abandoned','x');",
                )
                .unwrap();
        }
        assert_eq!(
            reuse.idle_connections(),
            0,
            "a connection inside a transaction is closed, not pooled"
        );
        let mut connection = open_runtime_connection(file.path()).unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("the abandoned writer lock was released");
        let abandoned: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM app_kv WHERE key='abandoned'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(abandoned, 0, "the abandoned transaction rolled back");
        transaction.commit().unwrap();
        drop(connection);
        assert_eq!(reuse.idle_connections(), 1);
    }

    #[test]
    fn checked_out_connections_close_after_reuse_ends() {
        let file = database();
        let reuse = RuntimeConnectionReuse::enable(file.path());
        let connection = open_runtime_connection(file.path()).unwrap();
        let nested = RuntimeConnectionReuse::enable(file.path());
        drop(reuse);
        assert_eq!(nested.idle_connections(), 0);
        drop(nested);
        drop(connection);
        let path = file.path().to_path_buf();
        file.close()
            .expect("no pooled handle outlives the last guard");
        assert!(!path.exists());
    }
}
