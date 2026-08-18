//! Process-boundary parsing and database admission safety.
//!
//! Keeping this deterministic seam in the library lets its tests share the
//! runtime's primary test harness without compiling the full process wiring.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::DEFAULT_MAX_HEARTBEAT_AGE;
use crate::process_lock::ProcessLock;

#[derive(Debug, Eq, PartialEq)]
pub enum Command {
    Serve,
    DatabaseCheck {
        path: PathBuf,
    },
    DatabaseAdmit {
        path: PathBuf,
        source: PathBuf,
    },
    HealthCheck {
        path: PathBuf,
        maximum_age: Duration,
    },
    HealthSmoke {
        path: PathBuf,
    },
    Inventory,
    CatalogCheck {
        path: PathBuf,
    },
}

pub fn parse_command(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    match args.next().as_deref() {
        None | Some("serve") => {
            reject_remaining(args, "serve")?;
            Ok(Command::Serve)
        }
        Some("inventory") => {
            reject_remaining(args, "inventory")?;
            Ok(Command::Inventory)
        }
        Some("catalog-check") => parse_catalog_check(args),
        Some("db-admit") => parse_db_admit(args),
        Some("db-check") => parse_db_check(args),
        Some("health-check") => parse_health_check(args),
        Some("health-smoke") => parse_health_smoke(args),
        Some(command) => Err(format!(
            "unknown command {command:?}; expected `serve`, `db-admit`, `db-check`, `health-check`, `health-smoke`, `catalog-check`, or `inventory`"
        )),
    }
}

fn parse_catalog_check(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut explicit_path = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--path" => {
                explicit_path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--path requires a path".to_owned())?,
                ));
            }
            _ => return Err(format!("unexpected catalog-check argument {argument:?}")),
        }
    }
    Ok(Command::CatalogCheck {
        path: explicit_path
            .unwrap_or_else(|| PathBuf::from(cama_app::dotabase_sqlite::PRODUCTION_DOTABASE_PATH)),
    })
}

fn reject_remaining(mut args: impl Iterator<Item = String>, command: &str) -> Result<(), String> {
    args.next().map_or(Ok(()), |argument| {
        Err(format!("unexpected {command} argument {argument:?}"))
    })
}

fn parse_db_check(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut explicit_path = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--db-path" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--db-path requires a path".to_owned())?;
                explicit_path = Some(PathBuf::from(value));
            }
            _ => return Err(format!("unexpected db-check argument {argument:?}")),
        }
    }

    let path = explicit_path
        .or_else(|| env::var_os("DB_PATH").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("cama_shuffle.db"));
    Ok(Command::DatabaseCheck { path })
}

fn parse_db_admit(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut explicit_path = None;
    let mut source = None;
    let mut confirmation_count = 0;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--db-path" => {
                explicit_path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--db-path requires a path".to_owned())?,
                ));
            }
            "--source-db" => {
                source = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--source-db requires a path".to_owned())?,
                ));
            }
            "--disposable-copy" => confirmation_count += 1,
            _ => return Err(format!("unexpected db-admit argument {argument:?}")),
        }
    }
    if confirmation_count != 1 {
        return Err("db-admit requires exactly one --disposable-copy".to_owned());
    }
    Ok(Command::DatabaseAdmit {
        path: explicit_path.ok_or_else(|| "db-admit requires --db-path".to_owned())?,
        source: source.ok_or_else(|| "db-admit requires --source-db".to_owned())?,
    })
}

fn parse_health_check(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut explicit_path = None;
    let mut maximum_age = DEFAULT_MAX_HEARTBEAT_AGE;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--db-path" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--db-path requires a path".to_owned())?;
                explicit_path = Some(PathBuf::from(value));
            }
            "--max-age-seconds" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--max-age-seconds requires a positive integer".to_owned())?;
                let seconds = value
                    .parse::<u64>()
                    .map_err(|_| "--max-age-seconds requires a positive integer".to_owned())?;
                if seconds == 0 {
                    return Err("--max-age-seconds requires a positive integer".to_owned());
                }
                maximum_age = Duration::from_secs(seconds);
            }
            _ => return Err(format!("unexpected health-check argument {argument:?}")),
        }
    }

    let path = explicit_path
        .or_else(|| env::var_os("DB_PATH").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("cama_shuffle.db"));
    Ok(Command::HealthCheck { path, maximum_age })
}

fn parse_health_smoke(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut explicit_path = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--db-path" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--db-path requires a path".to_owned())?;
                explicit_path = Some(PathBuf::from(value));
            }
            _ => return Err(format!("unexpected health-smoke argument {argument:?}")),
        }
    }

    Ok(Command::HealthSmoke {
        path: explicit_path
            .or_else(|| env::var_os("DB_PATH").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("cama_shuffle.db")),
    })
}

pub fn acquire_runtime_lock(database_path: &Path) -> Result<ProcessLock, String> {
    let lock_path = ProcessLock::path_for_database(database_path);
    ProcessLock::try_acquire(&lock_path).map_err(|error| error.to_string())
}

fn sqlite_namespace(path: &Path) -> [PathBuf; 4] {
    let path_text = path.as_os_str().to_string_lossy();
    [
        path.to_path_buf(),
        PathBuf::from(format!("{path_text}-wal")),
        PathBuf::from(format!("{path_text}-shm")),
        PathBuf::from(format!("{path_text}-journal")),
    ]
}

fn paths_share_existing_identity(left: &Path, right: &Path) -> Result<bool, String> {
    if left == right {
        return Ok(true);
    }
    let left_metadata = match fs::metadata(left) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("could not inspect {left:?}: {error}")),
    };
    let right_metadata = match fs::metadata(right) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("could not inspect {right:?}: {error}")),
    };
    if fs::canonicalize(left).map_err(|error| format!("could not resolve {left:?}: {error}"))?
        == fs::canonicalize(right)
            .map_err(|error| format!("could not resolve {right:?}: {error}"))?
    {
        return Ok(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if left_metadata.dev() == right_metadata.dev()
            && left_metadata.ino() == right_metadata.ino()
        {
            return Ok(true);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (left_metadata, right_metadata);
    }
    Ok(false)
}

pub fn verify_disposable_database(source: &Path, candidate: &Path) -> Result<(), String> {
    if fs::symlink_metadata(source)
        .map_err(|error| format!("could not inspect source database: {error}"))?
        .file_type()
        .is_symlink()
        || fs::symlink_metadata(candidate)
            .map_err(|error| format!("could not inspect disposable database: {error}"))?
            .file_type()
            .is_symlink()
    {
        return Err("db-admit refuses symbolic-link database paths".to_owned());
    }
    let source = fs::canonicalize(source)
        .map_err(|error| format!("could not resolve source database: {error}"))?;
    let candidate = fs::canonicalize(candidate)
        .map_err(|error| format!("could not resolve disposable database: {error}"))?;
    let source_namespace = sqlite_namespace(&source);
    let candidate_namespace = sqlite_namespace(&candidate);
    for candidate_path in &candidate_namespace {
        match fs::symlink_metadata(candidate_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(
                    "db-admit refuses symbolic links in the disposable SQLite namespace".to_owned(),
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect disposable SQLite namespace path {candidate_path:?}: {error}"
                ));
            }
        }
        for source_path in &source_namespace {
            if paths_share_existing_identity(candidate_path, source_path)? {
                return Err("db-admit refuses overlap with the source SQLite namespace".to_owned());
            }
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "runtime-test-core"))]
mod tests;
