//! Durable cosmetic check-ins and a projection of pet life milestones.

use std::path::{Path, PathBuf};

use cama_domain::game_date::{GameDateError, game_day_start_ts};
use cama_domain::pet::{ADULT_AGE_SECONDS, SPECIES};
use cama_domain::pet_evolution::PetCalling;
use cama_domain::pet_keepsakes::{AlbumEntry, DiaryEntry, DiaryKind, GiftReceipt, choose_keepsake};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;
use crate::pet_repository::{PET_COLUMNS, PetRepositoryError, optional_pet};

#[derive(Debug, Error)]
pub enum PetKeepsakeRepositoryError {
    #[error("keepsake database operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid pet state: {0}")]
    Pet(#[from] PetRepositoryError),
    #[error("invalid game date: {0}")]
    GameDate(#[from] GameDateError),
}

#[derive(Clone)]
pub struct PetKeepsakeRepository {
    path: PathBuf,
}

impl PetKeepsakeRepository {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn check_in(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
        decay_per_day: i64,
        roll: u64,
    ) -> Result<Option<GiftReceipt>, PetKeepsakeRepositoryError> {
        let day = game_day_start_ts(now)?;
        let mut conn = open_runtime_connection(&self.path)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT pet_id,pet_name,keepsake_id,received_at FROM pet_keepsakes
             WHERE discord_id=?1 AND guild_id=?2 AND game_day=?3",
                params![discord_id, guild_id, day],
                |row| {
                    Ok(GiftReceipt {
                        pet_id: row.get(0)?,
                        pet_name: row.get(1)?,
                        keepsake_id: row.get(2)?,
                        received_at: row.get(3)?,
                    })
                },
            )
            .optional()?;
        if existing.is_some() {
            transaction.commit()?;
            return Ok(existing);
        }
        let Some(pet) = optional_pet(
            &transaction,
            &format!(
                "SELECT {PET_COLUMNS} FROM pets WHERE discord_id=?1 AND guild_id=?2 AND is_active=1 AND died_at IS NULL"
            ),
            params![discord_id, guild_id],
        )?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        if pet.hatched_at > now
            || !SPECIES
                .iter()
                .any(|species| species.species_id == pet.species)
            || pet.current_hunger(now, decay_per_day) <= 0
        {
            transaction.commit()?;
            return Ok(None);
        }
        let calling = if now.saturating_sub(pet.hatched_at) >= ADULT_AGE_SECONDS
            && pet.evolved_at.is_some_and(|at| at <= now)
        {
            PetCalling::ALL
                .into_iter()
                .find(|calling| Some(calling.as_str()) == pet.evolution_calling.as_deref())
        } else {
            None
        };
        let gift = choose_keepsake(calling, roll);
        transaction.execute(
            "INSERT INTO pet_keepsakes(
                 discord_id,guild_id,game_day,pet_id,pet_name,keepsake_id,received_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                discord_id, guild_id, day, pet.pet_id, pet.name, gift.id, now
            ],
        )?;
        transaction.commit()?;
        Ok(Some(GiftReceipt {
            pet_id: pet.pet_id,
            pet_name: pet.name,
            keepsake_id: gift.id.to_owned(),
            received_at: now,
        }))
    }

    pub fn album(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Vec<AlbumEntry>, PetKeepsakeRepositoryError> {
        let conn = open_runtime_connection(&self.path)?;
        let mut statement = conn.prepare(
            "SELECT keepsake_id,pet_name,received_at,total FROM (
                 SELECT keepsake_id,pet_name,received_at,
                     COUNT(*) OVER(PARTITION BY keepsake_id) AS total,
                     ROW_NUMBER() OVER(
                         PARTITION BY keepsake_id ORDER BY received_at,game_day
                     ) AS ordinal
                 FROM pet_keepsakes WHERE discord_id=?1 AND guild_id=?2
             ) WHERE ordinal=1 ORDER BY received_at,keepsake_id",
        )?;
        Ok(statement
            .query_map(params![discord_id, guild_id], |row| {
                Ok(AlbumEntry {
                    keepsake_id: row.get(0)?,
                    first_pet_name: row.get(1)?,
                    first_received_at: row.get(2)?,
                    count: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn diary(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<DiaryEntry>, PetKeepsakeRepositoryError> {
        let conn = open_runtime_connection(&self.path)?;
        let mut statement = conn.prepare(
            "WITH owned AS (
                 SELECT p.*,
                     COALESCE(h.hatched_at,p.hatched_at) AS original_hatched_at,
                     COALESCE(h.pet_name,p.name) AS hatch_name
                 FROM pets p LEFT JOIN pet_diary_hatches h
                     ON h.pet_id=p.pet_id AND h.guild_id=p.guild_id AND h.discord_id=p.discord_id
                 WHERE p.discord_id=?1 AND p.guild_id=?2 AND p.species!='unhatched'
                     AND COALESCE(h.hatched_at,p.hatched_at)<=?3
             ), events AS (
                 SELECT pet_id,hatch_name AS pet_name,original_hatched_at AS occurred_at,
                     0 AS kind,'' AS detail FROM owned
                 UNION ALL
                 SELECT pet_id,name,evolved_at,1,evolution_calling FROM owned
                     WHERE evolved_at IS NOT NULL AND evolved_at<=?3
                         AND evolution_calling IS NOT NULL
                 UNION ALL
                 SELECT p.pet_id,p.name,MIN(b.resolved_at),2,'' FROM owned p
                     JOIN pet_brawls b ON b.winner_pet_id=p.pet_id
                         AND b.winner_id=p.discord_id AND b.guild_id=p.guild_id
                     WHERE b.status='done' AND b.resolved_at<=?3 GROUP BY p.pet_id,p.name
                 UNION ALL
                 SELECT pet_id,pet_name,received_at,3,keepsake_id FROM pet_keepsakes
                     WHERE discord_id=?1 AND guild_id=?2 AND received_at<=?3
             ) SELECT pet_id,pet_name,occurred_at,kind,detail FROM events
                 ORDER BY occurred_at DESC,pet_id DESC,kind DESC,detail LIMIT ?4 OFFSET ?5",
        )?;
        Ok(statement
            .query_map(
                params![
                    discord_id,
                    guild_id,
                    now,
                    limit.clamp(0, 100),
                    offset.max(0)
                ],
                |row| {
                    let kind = match row.get::<_, i64>(3)? {
                        0 => DiaryKind::Hatched,
                        1 => DiaryKind::Calling(row.get(4)?),
                        2 => DiaryKind::FirstBrawlWin,
                        _ => DiaryKind::Gift(row.get(4)?),
                    };
                    Ok(DiaryEntry {
                        pet_id: row.get(0)?,
                        pet_name: row.get(1)?,
                        occurred_at: row.get(2)?,
                        kind,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    const NOW: i64 = 1_800_000_000;

    fn database() -> NamedTempFile {
        let file = NamedTempFile::new().unwrap();
        crate::schema_manager::initialize_or_migrate(file.path()).unwrap();
        file
    }

    fn pet(db: &NamedTempFile, owner: i64, guild: i64, name: &str) -> i64 {
        let conn = Connection::open(db.path()).unwrap();
        conn.execute("INSERT INTO pets(discord_id,guild_id,name,species,adopted_at,hatched_at,adopt_fee,last_fed_at,hunger_at_last_fed) VALUES (?1,?2,?3,'common_cama',?4,?4,0,?5,100)", params![owner,guild,name,NOW-100,NOW]).unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn daily_receipt_retries_survive_switches_and_preserve_donor() {
        let db = database();
        let first = pet(&db, 1, 2, "Pip");
        let repo = PetKeepsakeRepository::new(db.path());
        let receipt = repo.check_in(1, 2, NOW, 20, 0).unwrap().unwrap();
        Connection::open(db.path())
            .unwrap()
            .execute(
                "UPDATE pets SET is_active=0,name='Renamed',stabled_at=?1 WHERE pet_id=?2",
                params![NOW, first],
            )
            .unwrap();
        pet(&db, 1, 2, "Pop");
        assert_eq!(
            repo.check_in(1, 2, NOW + 1, 20, 999).unwrap(),
            Some(receipt)
        );
        repo.check_in(1, 2, NOW + 86400, 20, 0).unwrap().unwrap();
        let album = repo.album(1, 2).unwrap();
        assert_eq!(album.len(), 1);
        assert_eq!(album[0].count, 2);
        assert_eq!(album[0].first_pet_name, "Pip");
        assert!(repo.album(1, 3).unwrap().is_empty());
        Connection::open(db.path())
            .unwrap()
            .execute("UPDATE pets SET is_active=0,died_at=?1", [NOW + 86400])
            .unwrap();
        assert_eq!(repo.album(1, 2).unwrap()[0].count, 2);
        assert_eq!(
            repo.check_in(1, 2, NOW + 86401, 20, 5)
                .unwrap()
                .unwrap()
                .pet_name,
            "Pop"
        );
    }

    #[test]
    fn gifts_reset_at_the_game_day_boundary() {
        let db = database();
        pet(&db, 1, 2, "Pip");
        let repo = PetKeepsakeRepository::new(db.path());
        let boundary = game_day_start_ts(NOW).unwrap() + 86400;
        let before = repo.check_in(1, 2, boundary - 1, 20, 0).unwrap().unwrap();
        let after = repo.check_in(1, 2, boundary, 20, 1).unwrap().unwrap();
        assert_ne!(before.keepsake_id, after.keepsake_id);
        assert_eq!(
            repo.check_in(1, 2, boundary + 1, 20, 0).unwrap(),
            Some(after)
        );
        assert_eq!(repo.album(1, 2).unwrap().len(), 2);
    }

    #[test]
    fn hatch_diary_timestamp_and_name_survive_stabling() {
        let db = database();
        let id = pet(&db, 1, 2, "Pip");
        let mut conn = Connection::open(db.path()).unwrap();
        conn.execute("UPDATE pets SET species='unhatched' WHERE pet_id=?1", [id])
            .unwrap();
        crate::pet_repository::PetRepository::new(db.path())
            .resolve_hatch_species(id, Some(2), "common_cama", NOW)
            .unwrap();
        conn.execute(
            "UPDATE pets SET is_active=0,stabled_at=?1,name='Renamed' WHERE pet_id=?2",
            params![NOW, id],
        )
        .unwrap();
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let activated =
            crate::pet_repository::activate_oldest_stabled(&transaction, 1, 2, NOW + 86400)
                .unwrap()
                .unwrap();
        transaction.commit().unwrap();
        assert_eq!(activated.hatched_at, NOW - 100 + 86400);
        let diary = PetKeepsakeRepository::new(db.path())
            .diary(1, 2, NOW + 86400, 15, 0)
            .unwrap();
        assert_eq!(diary.len(), 1);
        assert_eq!(diary[0].occurred_at, NOW - 100);
        assert_eq!(diary[0].pet_name, "Pip");
    }

    #[test]
    fn ineligible_pets_cannot_claim_and_guilds_are_isolated() {
        for update in [
            "species='unhatched'",
            "species='unknown'",
            "hatched_at=1800000001",
            "is_active=0",
            "is_active=0,died_at=1800000000",
            "hunger_at_last_fed=0",
            "last_fed_at=1700000000",
        ] {
            let db = database();
            pet(&db, 1, 2, "Pip");
            Connection::open(db.path())
                .unwrap()
                .execute(&format!("UPDATE pets SET {update}"), [])
                .unwrap();
            assert!(
                PetKeepsakeRepository::new(db.path())
                    .check_in(1, 2, NOW, 20, 0)
                    .unwrap()
                    .is_none(),
                "{update}"
            );
        }
        let db = database();
        pet(&db, 1, 2, "Pip");
        pet(&db, 1, 3, "Pop");
        let repo = PetKeepsakeRepository::new(db.path());
        assert!(repo.check_in(1, 2, NOW, 20, 0).unwrap().is_some());
        assert!(repo.check_in(1, 3, NOW, 20, 0).unwrap().is_some());
        assert!(repo.check_in(2, 2, NOW, 20, 0).unwrap().is_none());
    }

    #[test]
    fn concurrent_checks_return_one_identical_receipt() {
        let db = database();
        pet(&db, 1, 2, "Pip");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|roll| {
                let repo = PetKeepsakeRepository::new(db.path());
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    repo.check_in(1, 2, NOW, 20, roll).unwrap().unwrap()
                })
            })
            .collect();
        let receipts: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(receipts.iter().all(|r| r == &receipts[0]));
        assert_eq!(
            PetKeepsakeRepository::new(db.path()).album(1, 2).unwrap()[0].count,
            1
        );
    }

    #[test]
    fn diary_projects_milestones_orders_pages_and_excludes_future_eggs() {
        let db = database();
        let id = pet(&db, 1, 2, "Pip");
        let conn = Connection::open(db.path()).unwrap();
        conn.execute(
            "UPDATE pets SET evolved_at=?1,evolution_calling='delver' WHERE pet_id=?2",
            params![NOW - 50, id],
        )
        .unwrap();
        for at in [NOW - 20, NOW - 10] {
            conn.execute("INSERT INTO pet_brawls(guild_id,channel_id,challenger_id,recipient_id,challenger_pet_id,status,created_at,expires_at,resolved_at,winner_id,winner_pet_id) VALUES (2,3,1,4,?1,'done',?2,?3,?3,1,?1)",params![id,NOW-30,at]).unwrap();
        }
        let repo = PetKeepsakeRepository::new(db.path());
        repo.check_in(1, 2, NOW, 20, 0).unwrap();
        conn.execute(
            "UPDATE pets SET is_active=0,died_at=?1 WHERE pet_id=?2",
            params![NOW, id],
        )
        .unwrap();
        let egg = pet(&db, 1, 2, "Egg");
        conn.execute(
            "UPDATE pets SET species='unhatched',hatched_at=?1 WHERE pet_id=?2",
            params![NOW + 100, egg],
        )
        .unwrap();
        pet(&db, 1, 3, "Other guild");
        pet(&db, 2, 2, "Other owner");
        let diary = repo.diary(1, 2, NOW, 15, 0).unwrap();
        assert_eq!(diary.len(), 4);
        assert!(matches!(diary[0].kind, DiaryKind::Gift(_)));
        assert_eq!(diary[1].kind, DiaryKind::FirstBrawlWin);
        assert_eq!(diary[1].occurred_at, NOW - 20);
        assert_eq!(diary[2].kind, DiaryKind::Calling("delver".to_owned()));
        assert_eq!(diary[3].kind, DiaryKind::Hatched);
        assert_eq!(repo.diary(1, 2, NOW, 2, 2).unwrap(), diary[2..]);
        assert!(repo.diary(1, 2, NOW, 15, 4).unwrap().is_empty());
        assert_eq!(repo.diary(1, 2, NOW - 15, 15, 0).unwrap().len(), 3);
    }

    #[test]
    fn calling_gifts_require_actual_adulthood_and_evolution() {
        for (age, evolved, expected_calling) in [
            (100, Some(NOW - 1), false),
            (ADULT_AGE_SECONDS, None, false),
            (ADULT_AGE_SECONDS, Some(NOW + 1), false),
            (ADULT_AGE_SECONDS, Some(NOW - 1), true),
        ] {
            let db = database();
            pet(&db, 1, 2, "Pip");
            Connection::open(db.path()).unwrap().execute("UPDATE pets SET adopted_at=?1,hatched_at=?1,evolved_at=?2,evolution_calling='delver'",params![NOW-age,evolved]).unwrap();
            let gift = PetKeepsakeRepository::new(db.path())
                .check_in(1, 2, NOW, 20, 6)
                .unwrap()
                .unwrap();
            assert_eq!(
                cama_domain::pet_keepsakes::get_keepsake(&gift.keepsake_id)
                    .unwrap()
                    .calling
                    .is_some(),
                expected_calling
            );
        }
    }
}
