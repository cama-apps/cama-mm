//! Measure repository connection cost with and without runtime reuse.
//!
//! `cargo run --release --manifest-path rust/Cargo.toml -p cama-db --example runtime_connection_reuse`

use std::time::{Duration, Instant};

use cama_db::{RuntimeConnectionReuse, open_runtime_connection, schema_manager};

const ROUNDS: u32 = 2_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime_connection_reuse.db");
    schema_manager::initialize_or_migrate(&path)?;

    let held = open_runtime_connection(&path)?;
    let query_only = time(|| count_rows(&held))?;
    drop(held);
    let fresh = time(|| count_rows(&*open_runtime_connection(&path)?))?;
    let reuse = RuntimeConnectionReuse::enable(&path);
    let pooled = time(|| count_rows(&*open_runtime_connection(&path)?))?;
    drop(reuse);

    println!("rounds={ROUNDS}");
    println!("query on a held connection: {}", micros(query_only));
    println!("open + query without reuse: {}", micros(fresh));
    println!("open + query with reuse:    {}", micros(pooled));
    Ok(())
}

fn count_rows(connection: &rusqlite::Connection) -> Result<(), rusqlite::Error> {
    connection.query_row("SELECT COUNT(*) FROM app_kv", [], |row| {
        row.get::<_, i64>(0)
    })?;
    Ok(())
}

fn time(
    mut operation: impl FnMut() -> Result<(), rusqlite::Error>,
) -> Result<Duration, rusqlite::Error> {
    let started = Instant::now();
    for _ in 0..ROUNDS {
        operation()?;
    }
    Ok(started.elapsed() / ROUNDS)
}

fn micros(duration: Duration) -> String {
    format!("{:.1} us/op", duration.as_secs_f64() * 1_000_000.0)
}
