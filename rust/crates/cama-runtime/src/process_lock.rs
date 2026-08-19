//! Cross-runtime single-owner guard for the production Discord process.
//!
//! Both deployment artifacts lock the same file before touching SQLite or the
//! Discord gateway. The Python container uses `flock(1)` and Rust uses the
//! matching advisory file-lock primitive through `fs2`, so an accidental
//! overlapping deploy fails closed instead of creating two event consumers or
//! writers.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use thiserror::Error;

pub const DEFAULT_LOCK_FILE_NAME: &str = ".cama-runtime.lock";

/// Held for the lifetime of the sole production runtime.
#[derive(Debug)]
pub struct ProcessLock {
    file: File,
    path: PathBuf,
}

impl ProcessLock {
    /// Resolve the shared lock next to the configured SQLite database.
    #[must_use]
    pub fn path_for_database(database_path: &Path) -> PathBuf {
        database_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .join(DEFAULT_LOCK_FILE_NAME)
    }

    /// Acquire the shared runtime lock without waiting.
    ///
    /// Failing immediately is deliberate: container restart policy and deploy
    /// tooling can surface an overlapping process, while blocking would leave
    /// a seemingly healthy but inert replacement indefinitely.
    pub fn try_acquire(path: impl AsRef<Path>) -> Result<Self, ProcessLockError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| ProcessLockError::Open {
                path: path.clone(),
                source,
            })?;
        file.try_lock_exclusive().map_err(|source| {
            if source.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                ProcessLockError::AlreadyRunning { path: path.clone() }
            } else {
                ProcessLockError::Lock {
                    path: path.clone(),
                    source,
                }
            }
        })?;
        Ok(Self { file, path })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug, Error)]
pub enum ProcessLockError {
    #[error("cannot open runtime lock {path}: {source}")]
    Open { path: PathBuf, source: io::Error },
    #[error("another Cama runtime already owns {path}")]
    AlreadyRunning { path: PathBuf },
    #[error("cannot lock runtime guard {path}: {source}")]
    Lock { path: PathBuf, source: io::Error },
}

#[cfg(all(test, feature = "runtime-test-match"))]
mod tests {
    #[cfg(target_os = "linux")]
    use std::process::{Command, Stdio};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn lock_path_is_shared_with_the_database_directory() {
        assert_eq!(
            ProcessLock::path_for_database(Path::new("/app/data/cama_shuffle.db")),
            PathBuf::from("/app/data/.cama-runtime.lock")
        );
        assert_eq!(
            ProcessLock::path_for_database(Path::new("cama_shuffle.db")),
            PathBuf::from("./.cama-runtime.lock")
        );
    }

    #[test]
    fn overlapping_runtime_fails_closed_and_release_allows_restart() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join(DEFAULT_LOCK_FILE_NAME);
        let first = ProcessLock::try_acquire(&path).expect("first runtime owns lock");
        let second = ProcessLock::try_acquire(&path).expect_err("second runtime rejected");
        assert!(matches!(second, ProcessLockError::AlreadyRunning { .. }));

        drop(first);
        ProcessLock::try_acquire(&path).expect("restart acquires released lock");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn util_linux_flock_and_rust_share_the_same_guard() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join(DEFAULT_LOCK_FILE_NAME);
        let mut python_wrapper = Command::new("flock")
            .args(["--nonblock", "--no-fork"])
            .arg(&path)
            .args(["sh", "-c", "printf ready; read _ || true"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn the same util-linux wrapper used by Dockerfile");

        let mut ready = [0_u8; 5];
        std::io::Read::read_exact(
            python_wrapper.stdout.as_mut().expect("wrapper stdout"),
            &mut ready,
        )
        .expect("wrapper acquired the lock");
        assert_eq!(&ready, b"ready");
        assert!(matches!(
            ProcessLock::try_acquire(&path),
            Err(ProcessLockError::AlreadyRunning { .. })
        ));

        drop(python_wrapper.stdin.take());
        assert!(python_wrapper.wait().expect("wrapper exit").success());
        let rust_runtime =
            ProcessLock::try_acquire(&path).expect("Rust owns the lock after Python exits");
        assert!(
            !Command::new("flock")
                .args(["--nonblock"])
                .arg(&path)
                .arg("true")
                .status()
                .expect("run Python container's lock primitive")
                .success(),
            "util-linux flock must reject overlap while Rust owns the guard"
        );
        drop(rust_runtime);
        assert!(
            Command::new("flock")
                .args(["--nonblock"])
                .arg(&path)
                .arg("true")
                .status()
                .expect("run lock primitive after Rust shutdown")
                .success(),
            "Python restart must acquire the guard after Rust exits"
        );
    }
}
