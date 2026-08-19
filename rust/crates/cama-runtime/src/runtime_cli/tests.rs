use tempfile::tempdir;

use super::*;

#[test]
fn default_and_explicit_serve_select_the_gateway_runtime() {
    assert_eq!(parse_command(std::iter::empty()), Ok(Command::Serve));
    assert_eq!(
        parse_command(["serve".to_owned()].into_iter()),
        Ok(Command::Serve)
    );
}

#[test]
fn db_check_accepts_an_explicit_path() {
    assert_eq!(
        parse_command(
            ["db-check", "--db-path", "/tmp/test.db"]
                .map(str::to_owned)
                .into_iter()
        ),
        Ok(Command::DatabaseCheck {
            path: PathBuf::from("/tmp/test.db")
        })
    );
}

#[test]
fn db_admit_requires_an_explicit_distinct_copy_contract() {
    assert_eq!(
        parse_command(
            [
                "db-admit",
                "--db-path",
                "/tmp/disposable.db",
                "--source-db",
                "/tmp/live.db",
                "--disposable-copy",
            ]
            .map(str::to_owned)
            .into_iter()
        ),
        Ok(Command::DatabaseAdmit {
            path: PathBuf::from("/tmp/disposable.db"),
            source: PathBuf::from("/tmp/live.db"),
        })
    );
    assert!(
        parse_command(
            [
                "db-admit",
                "--db-path",
                "/tmp/disposable.db",
                "--source-db",
                "/tmp/live.db",
            ]
            .map(str::to_owned)
            .into_iter()
        )
        .is_err()
    );
}

#[test]
fn db_admit_refuses_the_source_and_accepts_a_distinct_file() {
    let directory = tempdir().expect("temporary admission directory");
    let source = directory.path().join("live.db");
    let candidate = directory.path().join("candidate.db");
    fs::write(&source, b"source").expect("write source identity fixture");
    fs::write(&candidate, b"candidate").expect("write candidate identity fixture");

    assert!(verify_disposable_database(&source, &source).is_err());
    verify_disposable_database(&source, &candidate).expect("distinct file is disposable");
}

#[cfg(unix)]
#[test]
fn db_admit_refuses_a_hard_link_to_the_source() {
    let directory = tempdir().expect("temporary admission directory");
    let source = directory.path().join("live.db");
    let candidate = directory.path().join("candidate.db");
    fs::write(&source, b"source").expect("write source identity fixture");
    fs::hard_link(&source, &candidate).expect("create hard-link fixture");

    assert!(
        verify_disposable_database(&source, &candidate)
            .expect_err("hard-linked live DB must be refused")
            .contains("source SQLite namespace")
    );
}

#[cfg(unix)]
#[test]
fn db_admit_refuses_a_candidate_sidecar_linked_to_the_source_namespace() {
    let directory = tempdir().expect("temporary admission directory");
    let source = directory.path().join("live.db");
    let candidate = directory.path().join("candidate.db");
    let candidate_wal = PathBuf::from(format!("{}-wal", candidate.display()));
    fs::write(&source, b"source").expect("write source identity fixture");
    fs::write(&candidate, b"candidate").expect("write candidate identity fixture");
    fs::hard_link(&source, &candidate_wal).expect("create sidecar hard-link fixture");

    assert!(
        verify_disposable_database(&source, &candidate)
            .expect_err("candidate sidecar linked to live DB must be refused")
            .contains("source SQLite namespace")
    );
}

#[test]
fn health_check_accepts_path_and_maximum_age() {
    assert_eq!(
        parse_command(
            [
                "health-check",
                "--db-path",
                "/tmp/test.db",
                "--max-age-seconds",
                "45",
            ]
            .map(str::to_owned)
            .into_iter()
        ),
        Ok(Command::HealthCheck {
            path: PathBuf::from("/tmp/test.db"),
            maximum_age: Duration::from_secs(45),
        })
    );
    assert!(
        parse_command(
            ["health-check", "--max-age-seconds", "0"]
                .map(str::to_owned)
                .into_iter()
        )
        .is_err()
    );
}

#[test]
fn health_smoke_accepts_an_explicit_database_path() {
    assert_eq!(
        parse_command(
            ["health-smoke", "--db-path", "/tmp/health-smoke.db"]
                .map(str::to_owned)
                .into_iter()
        ),
        Ok(Command::HealthSmoke {
            path: PathBuf::from("/tmp/health-smoke.db")
        })
    );
    assert!(
        parse_command(
            ["health-smoke", "--unexpected"]
                .map(str::to_owned)
                .into_iter()
        )
        .is_err()
    );
}

#[test]
fn catalog_check_accepts_an_explicit_path() {
    assert_eq!(
        parse_command(
            ["catalog-check", "--path", "/tmp/dotabase.db"]
                .map(str::to_owned)
                .into_iter()
        ),
        Ok(Command::CatalogCheck {
            path: PathBuf::from("/tmp/dotabase.db")
        })
    );
}

#[test]
fn runtime_rejects_unknown_commands_and_extra_arguments() {
    assert!(parse_command(["wat".to_owned()].into_iter()).is_err());
    assert!(parse_command(["serve".to_owned(), "unexpected".to_owned()].into_iter()).is_err());
}

#[test]
fn serve_process_lock_rejects_overlap_and_releases_on_drop() {
    let directory = tempdir().expect("temporary database directory");
    let database_path = directory.path().join("cama.db");
    let first = acquire_runtime_lock(&database_path).expect("first runtime owns lock");
    assert!(acquire_runtime_lock(&database_path).is_err());
    drop(first);
    acquire_runtime_lock(&database_path).expect("replacement owns released lock");
}
