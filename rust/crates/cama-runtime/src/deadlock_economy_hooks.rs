//! Restartable Deadlock post-payout effects. Run through the blocking DB port.
use std::path::Path;

use cama_db::deadlock_betting::DeadlockBettingRepository;
use cama_db::dig_blood_pact::{DigBloodPactRepository, DigBloodPactSettlementRequest};

pub type BloodPactRecovery = Vec<(String, Result<i64, String>)>;

/// The existing atomic Blood Pact engine handles cap reservation, protection,
/// transfer and replay receipts. Deadlock owns a separate durable outbox and
/// event namespace; a crash after transfer but before acknowledgement is safe.
pub fn recover_blood_pacts(path: &Path) -> Result<BloodPactRecovery, String> {
    let markets = DeadlockBettingRepository::new(path);
    let engine = DigBloodPactRepository::new(path);
    let jobs = markets
        .pending_blood_pacts()
        .map_err(|error| error.to_string())?;
    Ok(jobs
        .into_iter()
        .map(|job| {
            let result = (|| {
                let mana_date = crate::match_provider::pacific_mana_day(job.occurred_at)?;
                let receipt = engine
                    .settle(&DigBloodPactSettlementRequest {
                        target_id: job.discord_id,
                        guild_id: job.guild_id,
                        earning: job.earning,
                        event_key: job.event_key.clone(),
                        minigame_jc_delta_scale: job.minigame_jc_delta_scale,
                        occurred_at: job.occurred_at,
                        mana_date,
                    })
                    .map_err(|error| error.to_string())?;
                markets
                    .complete_blood_pact(&job, receipt.applied_amount)
                    .map_err(|error| error.to_string())?;
                Ok(receipt.applied_amount)
            })();
            (job.event_key, result)
        })
        .collect())
}

#[cfg(all(test, feature = "runtime-test-match"))]
mod tests {
    use super::*;
    use cama_db::deadlock_betting::DeadlockBloodPactJob;
    use rusqlite::params;

    #[test]
    fn recovery_replays_atomic_transfer_once_and_acknowledges_outbox() {
        let file = tempfile::NamedTempFile::new().unwrap();
        cama_db::schema_manager::initialize_or_migrate(file.path()).unwrap();
        let c = cama_db::open_runtime_connection(file.path()).unwrap();
        for id in [1, 2] {
            c.execute("INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance) VALUES(?1,9,'Player',100)",[id]).unwrap();
        }
        let now = 2_000_000_000;
        c.execute("INSERT INTO manashop_buffs(discord_id,guild_id,buff_type,target_id,granted_at,expires_at,triggered,data) VALUES(2,9,'blood_pact',1,?1,?2,0,?3)",params![now-1,now+86400,serde_json::json!({"skimmed_total":0,"cap":100,"skim_rate":0.2}).to_string()]).unwrap();
        // A later purchase must not capture a result that predates the pact,
        // even when its outbox job is recovered after the newer purchase.
        c.execute("INSERT INTO manashop_buffs(discord_id,guild_id,buff_type,target_id,granted_at,expires_at,triggered,data) VALUES(3,9,'blood_pact',1,?1,?2,0,?3)",params![now+1,now+86401,serde_json::json!({"skimmed_total":0,"cap":100,"skim_rate":0.8}).to_string()]).unwrap();
        let job = DeadlockBloodPactJob {
            market_id: "deadlock:1".into(),
            guild_id: 9,
            discord_id: 1,
            earning: 50,
            event_key: "deadlock-bet-blood-pact:1:1".into(),
            occurred_at: now,
            minigame_jc_delta_scale: 1.0,
        };
        c.execute("INSERT INTO deadlock_economic_receipts(receipt_id,market_id,guild_id,discord_id,kind,amount,detail_json,created_at) VALUES('deadlock:1:blood_pact_pending:1','deadlock:1',9,1,'blood_pact_pending',50,?1,?2)",params![serde_json::to_string(&job).unwrap(),now]).unwrap();
        // Emulate a successful transfer followed by a crash before the outbox
        // acknowledgement. Recovery must obtain the original engine receipt.
        let first = DigBloodPactRepository::new(file.path())
            .settle(&DigBloodPactSettlementRequest {
                target_id: 1,
                guild_id: 9,
                earning: 50,
                event_key: job.event_key.clone(),
                minigame_jc_delta_scale: 1.0,
                occurred_at: now,
                mana_date: crate::match_provider::pacific_mana_day(now).unwrap(),
            })
            .unwrap();
        assert_eq!(first.applied_amount, 10);
        let results = recover_blood_pacts(file.path()).unwrap();
        assert_eq!(results[0].1, Ok(10));
        assert!(recover_blood_pacts(file.path()).unwrap().is_empty());
        assert_eq!(
            c.query_row(
                "SELECT jopacoin_balance FROM players WHERE guild_id=9 AND discord_id=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            90
        );
        assert_eq!(
            c.query_row(
                "SELECT jopacoin_balance FROM players WHERE guild_id=9 AND discord_id=2",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            110
        );
    }
}
