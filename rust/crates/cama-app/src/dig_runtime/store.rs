use super::*;

/// Existing-schema SQLite adapter.  It never creates or migrates tables.
#[derive(Clone, Debug)]
pub struct SqliteDigRuntimeStore {
    pub(super) path: PathBuf,
}

/// One actor-scoped tunnel/wallet mutation admitted by the runtime SQLite
/// adapter. Keeping the audit identity beside the state deltas prevents
/// positional call sites from swapping cost, reward, or timestamp fields.
#[derive(Clone, Copy, Debug)]
pub struct AtomicTunnelBalanceUpdate<'a> {
    pub discord_id: i64,
    pub guild_id: i64,
    pub balance_delta: i64,
    pub balance_cost: i64,
    pub depth_after: Option<i64>,
    pub detail: &'a str,
    pub action_type: &'a str,
    pub now: i64,
}

impl SqliteDigRuntimeStore {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub(super) fn connection(&self) -> Result<Connection, rusqlite::Error> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", false)?;
        Ok(connection)
    }

    /// Apply one actor-scoped Dig balance/tunnel mutation through the
    /// existing SQLite adapter.  The ledger context is installed only around
    /// the wallet writes, so a following ordinary balance update cannot
    /// inherit an event's actor or relation.
    pub fn atomic_tunnel_balance_update(
        &self,
        request: AtomicTunnelBalanceUpdate<'_>,
    ) -> Result<i64, DigRuntimeStoreError> {
        let AtomicTunnelBalanceUpdate {
            discord_id,
            guild_id,
            balance_delta,
            balance_cost,
            depth_after,
            detail,
            action_type,
            now,
        } = request;
        let detail_value = serde_json::from_str::<Value>(detail)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("balance update detail"))?;
        if !detail_value.is_object() {
            return Err(DigRuntimeStoreError::InvalidJson("balance update detail"));
        }
        let cost = balance_cost.max(0);
        let event_id = detail_value
            .get("event_id")
            .or_else(|| detail_value.get("event"))
            .and_then(Value::as_str);
        let related_type = event_id.map_or("dig_action", |_| "event");
        let related_id = event_id.unwrap_or(action_type);
        let reason = if balance_delta - cost >= 0 {
            "dig event credit"
        } else {
            "dig event debit"
        };

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0) FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(balance) = balance else {
            return Err(DigRuntimeStoreError::MissingPlayer);
        };
        let depth_before = transaction
            .query_row(
                "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(depth_before) = depth_before else {
            return Err(DigRuntimeStoreError::MissingTunnel);
        };
        if balance < cost {
            return Err(DigRuntimeStoreError::InsufficientFunds);
        }
        let balance_after = balance
            .checked_sub(cost)
            .and_then(|value| value.checked_add(balance_delta))
            .ok_or(DigRuntimeStoreError::Conflict)?;

        if cost != 0 {
            set_runtime_ledger_context(
                &transaction,
                discord_id,
                "event",
                related_id,
                "dig paid cost",
                detail,
            )?;
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                  WHERE discord_id=?2 AND guild_id=?3 AND jopacoin_balance=?4",
                params![balance - cost, discord_id, guild_id, balance],
            )?;
            clear_runtime_ledger_context(&transaction)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }
        if balance_delta != 0 {
            set_runtime_ledger_context(
                &transaction,
                discord_id,
                related_type,
                related_id,
                reason,
                detail,
            )?;
            let before = balance - cost;
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                  WHERE discord_id=?2 AND guild_id=?3 AND jopacoin_balance=?4",
                params![balance_after, discord_id, guild_id, before],
            )?;
            clear_runtime_ledger_context(&transaction)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }
        let depth_after = depth_after.unwrap_or(depth_before);
        let changed = transaction.execute(
            "UPDATE tunnels SET depth=?1 WHERE discord_id=?2 AND guild_id=?3
              AND depth=?4",
            params![depth_after, discord_id, guild_id, depth_before],
        )?;
        if changed != 1 {
            return Err(DigRuntimeStoreError::Conflict);
        }
        transaction.execute(
            "INSERT INTO dig_actions
                (guild_id,actor_id,target_id,action_type,depth_before,depth_after,
                 jc_delta,detail,created_at)
             VALUES (?1,?2,NULL,?3,?4,?5,?6,?7,?8)",
            params![
                guild_id,
                discord_id,
                action_type,
                depth_before,
                depth_after,
                balance_after - balance,
                detail,
                now,
            ],
        )?;
        let action_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(action_id)
    }
}

fn set_runtime_ledger_context(
    transaction: &Transaction<'_>,
    actor_id: i64,
    related_type: &str,
    related_id: &str,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    set_runtime_ledger_context_source(
        transaction,
        "dig",
        actor_id,
        related_type,
        related_id,
        reason,
        metadata,
    )
}

fn set_runtime_ledger_context_source(
    transaction: &Transaction<'_>,
    source: &str,
    actor_id: i64,
    related_type: &str,
    related_id: &str,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
            (id,source,actor_id,related_type,related_id,reason,metadata)
         VALUES (1,?1,?2,?3,?4,?5,?6)",
        params![source, actor_id, related_type, related_id, reason, metadata],
    )?;
    Ok(())
}

fn clear_runtime_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn runtime_ledger_reason(action_type: &str, delta: i64) -> &'static str {
    match (action_type, delta.signum()) {
        ("dig" | "paid_dig", 1) => "dig credit",
        ("dig" | "paid_dig", -1) => "dig debit",
        (_, 1) => "dig action credit",
        (_, -1) => "dig action debit",
        _ => "dig balance change",
    }
}

const TUNNEL_SELECT: &str = "SELECT depth,max_depth,total_digs,total_jc_earned,last_dig_at,
        luminosity,COALESCE(tunnel_name,?3),prestige_level,
        COALESCE(prestige_perks,'[]'),COALESCE(boss_progress,'{}'),
        COALESCE(boss_attempts,'{}'),route_state,injury_state,
        hard_hat_charges,COALESCE(reinforced_until,0),void_bait_digs,
        sonar_skip_pending,temp_buffs,temp_curses,stat_strength,
        stat_smarts,stat_stamina,stat_points,paid_digs_today,
        paid_dig_date,pickaxe_tier,current_run_jc,current_run_artifacts,
        current_run_events,streak_days,auto_buy_torch,auto_buy_hard_hat,
        best_run_score,total_prestige_score,streak_last_date,
        trap_active,trap_free_today,trap_date,insured_until,
        revenge_target,revenge_type,revenge_until,cheer_data,
        grappling_hook_charges,lantern_stub_date,thick_skin_date,
        mutations,engine_mode,miner_origin,miner_about,stat_boss_awards,
        stinger_curse,last_lum_update_at,pinnacle_boss_id,pinnacle_phase,
        pinnacle_hp_remaining,pinnacle_last_engaged_at,retreat_cooldown_until,
        last_cheer_at,cavein_free_streak,relic_trim_notice
 FROM tunnels WHERE discord_id=?1 AND guild_id=?2";

fn load_tunnel_row(
    row: &rusqlite::Row<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<DigRuntimeTunnel, rusqlite::Error> {
    Ok(DigRuntimeTunnel {
        discord_id,
        guild_id,
        depth: row.get(0)?,
        max_depth: row.get(1)?,
        total_digs: row.get(2)?,
        total_jc_earned: row.get(3)?,
        last_dig_at: row.get(4)?,
        luminosity: row.get(5)?,
        tunnel_name: row.get(6)?,
        prestige_level: row.get(7)?,
        prestige_perks: row.get(8)?,
        boss_progress: row.get(9)?,
        boss_attempts: row.get(10)?,
        route_state: row.get(11)?,
        injury_state: row.get(12)?,
        hard_hat_charges: row.get(13)?,
        reinforced_until: row.get(14)?,
        void_bait_digs: row.get(15)?,
        sonar_skip_pending: row.get::<_, i64>(16)? != 0,
        temp_buffs: row.get(17)?,
        temp_curses: row.get(18)?,
        stat_strength: row.get(19)?,
        stat_smarts: row.get(20)?,
        stat_stamina: row.get(21)?,
        stat_points: row.get(22)?,
        paid_digs_today: row.get(23)?,
        paid_dig_date: row.get(24)?,
        pickaxe_tier: row.get(25)?,
        current_run_jc: row.get(26)?,
        current_run_artifacts: row.get(27)?,
        current_run_events: row.get(28)?,
        streak_days: row.get(29)?,
        auto_buy_torch: row.get::<_, i64>(30)? != 0,
        auto_buy_hard_hat: row.get::<_, i64>(31)? != 0,
        best_run_score: row.get(32)?,
        total_prestige_score: row.get(33)?,
        streak_last_date: row.get(34)?,
        trap_active: row.get::<_, i64>(35)? != 0,
        trap_free_today: row.get::<_, i64>(36)? != 0,
        trap_date: row.get(37)?,
        insured_until: row.get(38)?,
        revenge_target: row.get(39)?,
        revenge_type: row.get(40)?,
        revenge_until: row.get(41)?,
        cheer_data: row.get(42)?,
        grappling_hook_charges: row.get(43)?,
        lantern_stub_date: row.get(44)?,
        thick_skin_date: row.get(45)?,
        mutations: row.get(46)?,
        engine_mode: row.get(47)?,
        miner_origin: row.get(48)?,
        miner_about: row.get(49)?,
        stat_boss_awards: row.get(50)?,
        stinger_curse: row.get(51)?,
        last_lum_update_at: row.get(52)?,
        pinnacle_boss_id: row.get(53)?,
        pinnacle_phase: row.get(54)?,
        pinnacle_hp_remaining: row.get(55)?,
        pinnacle_last_engaged_at: row.get(56)?,
        retreat_cooldown_until: row.get(57)?,
        last_cheer_at: row.get(58)?,
        cavein_free_streak: row.get(59)?,
        relic_trim_notice: row.get::<_, i64>(60)? != 0,
    })
}

fn event_actor_snapshot(
    snapshot: &DigRuntimeSnapshot,
    depth: i64,
    luminosity: i64,
) -> DigEventActorSnapshot {
    let tunnel = snapshot.tunnel.as_ref().expect("event actor needs tunnel");
    DigEventActorSnapshot {
        key: DigEventActorKey {
            discord_id: tunnel.discord_id,
            guild_id: Some(tunnel.guild_id),
        },
        depth,
        luminosity,
        prestige_level: tunnel.prestige_level,
        prestige_perks_json: tunnel.prestige_perks.clone(),
        boss_progress_json: tunnel.boss_progress.clone(),
        streak_days: tunnel.streak_days,
        temp_buff_json: tunnel.temp_buffs.clone(),
        temp_curse_json: tunnel.temp_curses.clone(),
        balance: snapshot.balance,
        inventory_count: snapshot.inventory.len(),
        owned_gear: snapshot
            .gear
            .iter()
            .filter_map(|piece| piece.item_id.clone())
            .collect(),
        equipped_gear: snapshot
            .gear
            .iter()
            .filter(|piece| piece.equipped && piece.durability > 0)
            .filter_map(|piece| piece.item_id.clone())
            .collect(),
        owned_artifacts: snapshot
            .artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.clone())
            .collect(),
        equipped_relics: snapshot
            .artifacts
            .iter()
            .filter(|artifact| artifact.is_relic && artifact.equipped)
            .map(|artifact| artifact.artifact_id.clone())
            .collect(),
    }
}

impl DigRuntimeStore for SqliteDigRuntimeStore {
    fn snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<DigRuntimeSnapshot, DigRuntimeStoreError> {
        let connection = self.connection()?;
        let player = connection
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0) FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(balance) = player else {
            return Ok(DigRuntimeSnapshot {
                registered: false,
                balance: 0,
                tunnel: None,
                inventory: Vec::new(),
                artifacts: Vec::new(),
                gear: Vec::new(),
                weather: Vec::new(),
            });
        };

        let tunnel = connection
            .query_row(
                TUNNEL_SELECT,
                params![discord_id, guild_id, format!("Miner {discord_id}")],
                |row| load_tunnel_row(row, discord_id, guild_id),
            )
            .optional()?;
        let inventory = load_inventory(&connection, discord_id, guild_id)?;
        let artifacts = load_artifacts(&connection, discord_id, guild_id)?;
        let gear = load_gear(&connection, discord_id, guild_id)?;
        Ok(DigRuntimeSnapshot {
            registered: true,
            balance,
            tunnel,
            inventory,
            artifacts,
            gear,
            weather: Vec::new(),
        })
    }

    fn event_actor_snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigEventActorSnapshot>, DigRuntimeStoreError> {
        DigEventRuntimeRepository::new(&self.path)
            .actor_snapshot_for_event(DigEventActorKey {
                discord_id,
                guild_id: Some(guild_id),
            })
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))
    }

    fn event_quest_snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<cama_db::dig_event_runtime::DigEventQuestSnapshot, DigRuntimeStoreError> {
        DigEventRuntimeRepository::new(&self.path)
            .quest_snapshot(
                DigEventActorKey {
                    discord_id,
                    guild_id: Some(guild_id),
                },
                now,
            )
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))
    }

    fn helltide_tax(&self, guild_id: i64, now: i64) -> Result<i64, DigRuntimeStoreError> {
        let modifier = DigGuildModifierRepository::new(&self.path)
            .get_active(Some(guild_id), now)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?
            .into_iter()
            .find(|modifier| modifier.modifier_id == "helltide_active");
        Ok(modifier
            .map(|modifier| {
                modifier
                    .payload
                    .get("tax_per_dig")
                    .and_then(Value::as_i64)
                    .unwrap_or(5)
                    .max(0)
            })
            .unwrap_or(0))
    }

    fn adjust_daily_reward(
        &self,
        guild_id: i64,
        amount: i64,
        now: i64,
        economy_config: &EconomyEventConfig,
    ) -> Result<(i64, f64), DigRuntimeStoreError> {
        if amount <= 0 {
            return Ok((amount, 1.0));
        }
        let service = SqliteEconomyEventService::new(&self.path, economy_config.clone());
        let adjusted = service
            .adjust_reward_at(guild_id, amount, now)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        let multiplier = (adjusted as f64 / amount as f64).max(0.0);
        Ok((adjusted, multiplier))
    }

    fn daily_reward_multiplier(
        &self,
        guild_id: i64,
        now: i64,
        economy_config: &EconomyEventConfig,
    ) -> Result<f64, DigRuntimeStoreError> {
        let effects = SqliteEconomyEventService::new(&self.path, economy_config.clone())
            .effects_at(guild_id, now)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        Ok(effects.reward_multiplier.max(0.0))
    }

    fn mana_effects(
        &self,
        discord_id: i64,
        guild_id: i64,
        today: &str,
    ) -> Result<ManaEffects, DigRuntimeStoreError> {
        // Python treats a failed mana lookup as one neutral request-local
        // snapshot; it must not turn an otherwise valid Dig into an error or
        // retry the repository later in the same action.
        let row = match ManaRepository::new(&self.path).get_mana(discord_id, Some(guild_id)) {
            Ok(row) => row,
            Err(_) => return Ok(ManaEffects::default()),
        };
        let Some(row) = row.filter(|row| row.assigned_date == today && !row.consumed_today) else {
            return Ok(ManaEffects::default());
        };
        let color = color_for_land(&row.current_land);
        Ok(ManaEffects::for_color(color, Some(&row.current_land)))
    }

    fn bankruptcy_penalty_games(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<i64, DigRuntimeStoreError> {
        BankruptcyRepository::new(&self.path)
            .get_penalty_games(discord_id, Some(guild_id))
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))
    }

    fn credit_plains_tithe(
        &self,
        discord_id: i64,
        guild_id: i64,
        total_jc: i64,
        tithe: i64,
        event_key: &str,
    ) -> Result<Option<i64>, DigRuntimeStoreError> {
        if total_jc <= 0 || tithe <= 0 {
            return Ok(None);
        }
        let context = LedgerContext {
            source: Some("dig".to_owned()),
            actor_id: Some(discord_id),
            related_type: Some("plains_tithe".to_owned()),
            related_id: Some(event_key.to_owned()),
            reason: Some("dig plains tithe reserve credit".to_owned()),
            metadata: Some(serde_json::json!({"total_jc": total_jc, "tithe": tithe}).to_string()),
        };
        LoanRepository::new(&self.path)
            .add_to_nonprofit_fund_once(Some(guild_id), tithe, &context)
            .map(|receipt| Some(receipt.amount))
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))
    }

    fn overgrowth_active(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<bool, DigRuntimeStoreError> {
        let active = ManashopRepository::new(&self.path)
            .active_for(discord_id, Some(guild_id), "overgrowth", now)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        Ok(active
            .iter()
            .any(|buff| buff.data.charges_remaining.unwrap_or(0) > 0))
    }

    fn auto_buy_items(
        &self,
        request: AutoBuyRequest<'_>,
    ) -> Result<Vec<cama_db::dig_inventory_repository::AutoBuyItemOutcome>, DigRuntimeStoreError>
    {
        DigInventoryRepository::new(&self.path)
            .ensure_auto_buy_items_atomic(request)
            .map_err(|error| DigRuntimeStoreError::Inventory(error.to_string()))
    }

    fn claim_slow_drip(
        &self,
        snapshot: &DigRuntimeSnapshot,
        now: i64,
        economy_config: &EconomyEventConfig,
    ) -> Result<Option<DigSlowDripClaim>, DigRuntimeStoreError> {
        let Some(tunnel) = snapshot.tunnel.as_ref() else {
            return Ok(None);
        };
        let equipped = snapshot.artifacts.iter().any(|artifact| {
            artifact.is_relic && artifact.equipped && artifact.artifact_id == "slow_drip"
        });
        if !equipped {
            return Ok(None);
        }
        let claim_date = game_date_for_timestamp(now as f64)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        let state = ManashopRepository::new(&self.path)
            .slow_drip_today(tunnel.discord_id, Some(tunnel.guild_id), &claim_date)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        let claimed_before = state.claimed_today.clamp(0, 100);
        if claimed_before >= 100 {
            ManashopRepository::new(&self.path)
                .stamp_slow_drip_seen(tunnel.discord_id, Some(tunnel.guild_id), &claim_date, now)
                .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
            return Ok(None);
        }
        let Some(anchor_before) = (state.last_claim_at > 0)
            .then_some(state.last_claim_at)
            .or(tunnel.last_dig_at.filter(|anchor| *anchor > 0))
        else {
            ManashopRepository::new(&self.path)
                .stamp_slow_drip_seen(tunnel.discord_id, Some(tunnel.guild_id), &claim_date, now)
                .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
            return Ok(None);
        };
        if anchor_before >= now {
            return Ok(None);
        }
        let elapsed_minutes = now
            .saturating_sub(anchor_before)
            .checked_div(60)
            .unwrap_or(0);
        let gross = settle_slow_drip_claim(elapsed_minutes, claimed_before, 1.0);
        if gross.gross_jc <= 0 {
            return Ok(None);
        }
        let economy_adjusted = SqliteEconomyEventService::new(&self.path, economy_config.clone())
            .adjust_reward_at(tunnel.guild_id, gross.gross_jc, now)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        let credit_jc = scale_positive_dig_jc(economy_adjusted);
        let claim = DigSlowDripClaim {
            claim_date,
            gross_jc: gross.gross_jc,
            credit_jc,
            claimed_before,
            claimed_after: claimed_before.saturating_add(gross.gross_jc),
            anchor_before,
            expected_last_claim_at: state.last_claim_at,
            claimed_at: now,
        };

        // Python's fail-soft boundary is intentional: consume the gross cap
        // first, then perform the independent wallet credit.  Use the
        // expected claim row as a CAS so two concurrent Digs cannot both
        // consume the same idle interval or cross the daily cap.
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE slow_drip_claims
                SET claimed_today=claimed_today+?1,last_claim_at=?2
              WHERE discord_id=?3 AND guild_id=?4 AND claim_date=?5
                AND claimed_today=?6 AND last_claim_at=?7",
            params![
                claim.gross_jc,
                claim.claimed_at,
                tunnel.discord_id,
                tunnel.guild_id,
                claim.claim_date,
                claim.claimed_before,
                claim.expected_last_claim_at,
            ],
        )?;
        if updated != 1 {
            let inserted = transaction.execute(
                "INSERT INTO slow_drip_claims
                     (discord_id,guild_id,claim_date,claimed_today,last_claim_at)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(discord_id,guild_id,claim_date) DO NOTHING",
                params![
                    tunnel.discord_id,
                    tunnel.guild_id,
                    claim.claim_date,
                    claim.gross_jc,
                    claim.claimed_at,
                ],
            )?;
            if inserted != 1 {
                // Another claimant won the row CAS.  This is a normal
                // duplicate click, not a Dig failure.
                return Ok(None);
            }
        }
        transaction.commit()?;
        if claim.credit_jc > 0 {
            let metadata = serde_json::json!({
                "claim_date": claim.claim_date,
                "gross_jc": claim.gross_jc,
                "credit_jc": claim.credit_jc,
                "claimed_before": claim.claimed_before,
                "claimed_after": claim.claimed_after,
                "anchor_before": claim.anchor_before,
                "claimed_at": claim.claimed_at,
            })
            .to_string();
            let credit_result = (|| -> Result<(), DigRuntimeStoreError> {
                let mut connection = self.connection()?;
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let balance = transaction
                    .query_row(
                        "SELECT COALESCE(jopacoin_balance,0) FROM players
                         WHERE discord_id=?1 AND guild_id=?2",
                        params![tunnel.discord_id, tunnel.guild_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                let Some(balance) = balance else {
                    return Err(DigRuntimeStoreError::MissingPlayer);
                };
                let balance_after = balance
                    .checked_add(claim.credit_jc)
                    .ok_or(DigRuntimeStoreError::Conflict)?;
                set_runtime_ledger_context(
                    &transaction,
                    tunnel.discord_id,
                    "slow_drip",
                    &claim.claim_date,
                    "slow drip credit",
                    &metadata,
                )?;
                let changed = transaction.execute(
                    "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
                    params![balance_after, tunnel.discord_id, tunnel.guild_id, balance],
                )?;
                clear_runtime_ledger_context(&transaction)?;
                if changed != 1 {
                    return Err(DigRuntimeStoreError::Conflict);
                }
                transaction.commit()?;
                Ok(())
            })();
            if let Err(error) = credit_result {
                let _ = error;
                return Ok(Some(DigSlowDripClaim {
                    credit_jc: 0,
                    ..claim
                }));
            }
        }
        Ok(Some(claim))
    }

    fn canonical_event_id(
        &self,
        snapshot: &DigRuntimeSnapshot,
        now: i64,
        in_boss: bool,
        entropy_seed: u64,
    ) -> Result<Option<String>, DigRuntimeStoreError> {
        let Some(tunnel) = snapshot.tunnel.as_ref() else {
            return Ok(None);
        };
        let actor = event_actor_snapshot(snapshot, tunnel.depth, tunnel.luminosity);
        let quest = DigEventRuntimeRepository::new(&self.path)
            .quest_snapshot(
                DigEventActorKey {
                    discord_id: tunnel.discord_id,
                    guild_id: Some(tunnel.guild_id),
                },
                now,
            )
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        let service = crate::dig_event_runtime::DigEventRuntimeService::sqlite(&self.path);
        let mut entropy = SeededLootEntropy::new(entropy_seed);
        Ok(service
            .roll_event_for_snapshot(&actor, &quest, true, in_boss, &mut entropy)
            .map(|event| event.event_id))
    }

    fn canonical_event_id_for_snapshot(
        &self,
        request: DigRuntimeCanonicalEventRequest<'_>,
    ) -> Result<Option<String>, DigRuntimeStoreError> {
        let Some(_tunnel) = request.snapshot.tunnel.as_ref() else {
            return Ok(None);
        };
        let actor = event_actor_snapshot(request.snapshot, request.depth, request.luminosity);
        let mut entropy = crate::dig_loot::ScriptedLootEntropy::new(
            [f64::from_bits(request.selection_roll_bits)],
            [],
        );
        let service = crate::dig_event_runtime::DigEventRuntimeService::sqlite(&self.path);
        Ok(service
            .roll_event_for_snapshot_with_modifiers(
                crate::dig_event_runtime::CanonicalEventRollInput {
                    snapshot: &actor,
                    quest_snapshot: request.quest,
                    include_quest_events: true,
                    in_boss: request.in_boss,
                    void_bait_active: request.void_bait_active,
                    rare_event_multiplier: request.rare_event_multiplier,
                    legendary_event_multiplier: request.legendary_event_multiplier,
                },
                &mut entropy,
            )
            .map(|event| event.event_id))
    }

    fn preview_pet_dig_work(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
        decay_per_day: i64,
        entropy_secret: u64,
    ) -> Result<Option<PetDigWork>, DigRuntimeStoreError> {
        if decay_per_day <= 0 {
            return Ok(None);
        }
        let entropy_seed = seed_for(
            DigRuntimeRequest {
                discord_id,
                guild_id,
                now,
                paid: false,
                forced_event: false,
            },
            entropy_secret,
        );
        SqlitePetCommandService::new(
            &self.path,
            SeededPetRandom::new(entropy_seed),
            SystemPetClock,
            decay_per_day,
        )
        .service_mut()
        .preview_dig_work(discord_id, Some(guild_id), now)
        .map_err(|error| DigRuntimeStoreError::Pet(error.to_string()))
    }

    fn commit_with_delivery(
        &self,
        mut request: DigRuntimeCommit,
        draft: DigRuntimeDeliveryDraft,
    ) -> Result<DigRuntimeCommitReceipt, DigRuntimeStoreError> {
        request.delivery_draft = Some(draft);
        self.commit(request)
    }

    fn commit(
        &self,
        request: DigRuntimeCommit,
    ) -> Result<DigRuntimeCommitReceipt, DigRuntimeStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let player_balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0) FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![
                    request
                        .next
                        .tunnel
                        .as_ref()
                        .map_or(0, |tunnel| tunnel.discord_id),
                    request
                        .next
                        .tunnel
                        .as_ref()
                        .map_or(0, |tunnel| tunnel.guild_id)
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(player_balance) = player_balance else {
            return Err(DigRuntimeStoreError::MissingPlayer);
        };
        if player_balance != request.expected.balance {
            return Err(DigRuntimeStoreError::Conflict);
        }
        let Some(next_tunnel) = request.next.tunnel.as_ref() else {
            return Err(DigRuntimeStoreError::MissingTunnel);
        };
        let discord_id = next_tunnel.discord_id;
        let guild_id = next_tunnel.guild_id;

        for item_id in &request.consumed_item_ids {
            let exists = transaction
                .query_row(
                    "SELECT 1 FROM dig_inventory
                     WHERE id=?1 AND discord_id=?2 AND guild_id=?3",
                    params![item_id, discord_id, guild_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if exists.is_none() {
                return Err(DigRuntimeStoreError::MissingQueuedItem(*item_id));
            }
        }

        let existing_tunnel = transaction
            .query_row(
                TUNNEL_SELECT,
                params![discord_id, guild_id, format!("Miner {discord_id}")],
                |row| load_tunnel_row(row, discord_id, guild_id),
            )
            .optional()?;
        let existing = existing_tunnel
            .as_ref()
            .map(|tunnel| (tunnel.depth, tunnel.total_digs, tunnel.last_dig_at));
        let live_inventory = load_inventory_transaction(&transaction, discord_id, guild_id)?;
        let live_artifacts = load_artifacts_transaction(&transaction, discord_id, guild_id)?;
        let live_gear = load_gear_transaction(&transaction, discord_id, guild_id)?;
        if !version_matches(existing, request.expected)
            || existing_tunnel
                .as_ref()
                .is_some_and(|tunnel| fingerprint(tunnel) != request.expected.tunnel_fingerprint)
            || fingerprint(&live_inventory) != request.expected.inventory_fingerprint
            || fingerprint(&live_artifacts) != request.expected.artifact_fingerprint
            || fingerprint(&live_gear) != request.expected.gear_fingerprint
        {
            return Err(DigRuntimeStoreError::Conflict);
        }

        if request.consume_overgrowth {
            let consumed = ManashopRepository::consume_overgrowth_charge_in_transaction(
                &transaction,
                discord_id,
                Some(guild_id),
                request.now,
            )
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
            if !consumed {
                return Err(DigRuntimeStoreError::OvergrowthConflict);
            }
        }

        if let Some(claim) = request.pet_work_claim {
            let changed = transaction.execute(
                "UPDATE pets
                    SET dig_work_units=?1, dig_work_at=?2
                  WHERE pet_id=?3 AND discord_id=?4 AND guild_id=?5
                    AND died_at IS NULL
                    AND dig_work_units=?6 AND dig_work_at=?7",
                params![
                    claim.new_units,
                    claim.new_at,
                    claim.pet_id,
                    discord_id,
                    guild_id,
                    claim.expected_units,
                    claim.expected_at,
                ],
            )?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::PetWorkConflict);
            }
        }

        if existing_tunnel.is_none() {
            transaction.execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,total_jc_earned,
                  luminosity,prestige_level,prestige_perks,boss_progress,boss_attempts,
                  route_state,injury_state,hard_hat_charges,reinforced_until,void_bait_digs,
                  sonar_skip_pending,temp_buffs,temp_curses,stat_strength,stat_smarts,
                  stat_stamina,stat_points,paid_digs_today,paid_dig_date,pickaxe_tier,
                  current_run_jc,current_run_artifacts,current_run_events,streak_days,
                  auto_buy_torch,auto_buy_hard_hat,last_dig_at,best_run_score,
                  total_prestige_score,streak_last_date,trap_active,trap_free_today,
                  trap_date,insured_until,revenge_target,revenge_type,revenge_until,
                  cheer_data,grappling_hook_charges,lantern_stub_date,thick_skin_date,
                  mutations,engine_mode,miner_origin,miner_about,stat_boss_awards,
                  stinger_curse,last_lum_update_at,pinnacle_boss_id,pinnacle_phase,
                  pinnacle_hp_remaining,pinnacle_last_engaged_at,retreat_cooldown_until,
                  last_cheer_at,cavein_free_streak,relic_trim_notice)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,
                         ?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,
                         ?35,?36,?37,?38,?39,?40,?41,?42,?43,?44,?45,?46,?47,?48,?49,?50,
                         ?51,?52,?53,?54,?55,?56,?57,?58,?59,?60,?61,?62,?63)",
                params![
                    discord_id,
                    guild_id,
                    next_tunnel.tunnel_name,
                    next_tunnel.depth,
                    next_tunnel.max_depth,
                    next_tunnel.total_digs,
                    next_tunnel.total_jc_earned,
                    next_tunnel.luminosity,
                    next_tunnel.prestige_level,
                    next_tunnel.prestige_perks,
                    next_tunnel.boss_progress,
                    next_tunnel.boss_attempts,
                    next_tunnel.route_state,
                    next_tunnel.injury_state,
                    next_tunnel.hard_hat_charges,
                    next_tunnel.reinforced_until,
                    next_tunnel.void_bait_digs,
                    i64::from(next_tunnel.sonar_skip_pending),
                    next_tunnel.temp_buffs,
                    next_tunnel.temp_curses,
                    next_tunnel.stat_strength,
                    next_tunnel.stat_smarts,
                    next_tunnel.stat_stamina,
                    next_tunnel.stat_points,
                    next_tunnel.paid_digs_today,
                    next_tunnel.paid_dig_date,
                    next_tunnel.pickaxe_tier,
                    next_tunnel.current_run_jc,
                    next_tunnel.current_run_artifacts,
                    next_tunnel.current_run_events,
                    next_tunnel.streak_days,
                    i64::from(next_tunnel.auto_buy_torch),
                    i64::from(next_tunnel.auto_buy_hard_hat),
                    next_tunnel.last_dig_at,
                    next_tunnel.best_run_score,
                    next_tunnel.total_prestige_score,
                    next_tunnel.streak_last_date,
                    i64::from(next_tunnel.trap_active),
                    i64::from(next_tunnel.trap_free_today),
                    next_tunnel.trap_date,
                    next_tunnel.insured_until,
                    next_tunnel.revenge_target,
                    next_tunnel.revenge_type,
                    next_tunnel.revenge_until,
                    next_tunnel.cheer_data,
                    next_tunnel.grappling_hook_charges,
                    next_tunnel.lantern_stub_date,
                    next_tunnel.thick_skin_date,
                    next_tunnel.mutations,
                    next_tunnel.engine_mode,
                    next_tunnel.miner_origin,
                    next_tunnel.miner_about,
                    next_tunnel.stat_boss_awards,
                    next_tunnel.stinger_curse,
                    next_tunnel.last_lum_update_at,
                    next_tunnel.pinnacle_boss_id,
                    next_tunnel.pinnacle_phase,
                    next_tunnel.pinnacle_hp_remaining,
                    next_tunnel.pinnacle_last_engaged_at,
                    next_tunnel.retreat_cooldown_until,
                    next_tunnel.last_cheer_at,
                    next_tunnel.cavein_free_streak,
                    i64::from(next_tunnel.relic_trim_notice),
                ],
            )?;
            // Python's tunnel constructor creates and equips a starter weapon
            // in the same admission transaction.  Keeping that invariant here
            // matters because the first dig snapshots gear before applying
            // pickaxe modifiers; a tunnel without this row silently falls
            // back to a procedural tier and loses the migrated gear identity.
            transaction.execute(
                "INSERT INTO dig_gear(
                     discord_id, guild_id, slot, tier, durability,
                     equipped, acquired_at, source, item_id
                 )
                 SELECT ?1, ?2, 'weapon', ?3, 20, 1, ?4, 'starter', NULL
                 WHERE NOT EXISTS (
                     SELECT 1 FROM dig_gear
                     WHERE discord_id=?1 AND guild_id=?2 AND slot='weapon'
                 )",
                params![discord_id, guild_id, next_tunnel.pickaxe_tier, request.now,],
            )?;
        } else {
            let changed = transaction.execute(
                "UPDATE tunnels SET depth=?1,max_depth=?2,total_digs=?3,total_jc_earned=?4,
                    last_dig_at=?5,luminosity=?6,prestige_level=?7,prestige_perks=?8,
                    boss_progress=?9,boss_attempts=?10,route_state=?11,injury_state=?12,
                    hard_hat_charges=?13,reinforced_until=?14,void_bait_digs=?15,
                    sonar_skip_pending=?16,temp_buffs=?17,temp_curses=?18,stat_strength=?19,
                    stat_smarts=?20,stat_stamina=?21,stat_points=?22,paid_digs_today=?23,
                    paid_dig_date=?24,pickaxe_tier=?25,current_run_jc=?26,
                    current_run_artifacts=?27,current_run_events=?28,streak_days=?29,
                    auto_buy_torch=?30,auto_buy_hard_hat=?31,tunnel_name=?32,
                    best_run_score=?38,total_prestige_score=?39,streak_last_date=?40,
                    trap_active=?41,trap_free_today=?42,trap_date=?43,insured_until=?44,
                    revenge_target=?45,revenge_type=?46,revenge_until=?47,cheer_data=?48,
                    grappling_hook_charges=?49,lantern_stub_date=?50,thick_skin_date=?51,
                    mutations=?52,engine_mode=?53,miner_origin=?54,miner_about=?55,
                    stat_boss_awards=?56,stinger_curse=?57,last_lum_update_at=?58,
                    pinnacle_boss_id=?59,pinnacle_phase=?60,pinnacle_hp_remaining=?61,
                    pinnacle_last_engaged_at=?62,retreat_cooldown_until=?63,
                    last_cheer_at=?64,cavein_free_streak=?65,relic_trim_notice=?66
                 WHERE discord_id=?33 AND guild_id=?34
                   AND depth=?35 AND total_digs=?36 AND last_dig_at IS ?37",
                params![
                    next_tunnel.depth,
                    next_tunnel.max_depth,
                    next_tunnel.total_digs,
                    next_tunnel.total_jc_earned,
                    next_tunnel.last_dig_at,
                    next_tunnel.luminosity,
                    next_tunnel.prestige_level,
                    next_tunnel.prestige_perks,
                    next_tunnel.boss_progress,
                    next_tunnel.boss_attempts,
                    next_tunnel.route_state,
                    next_tunnel.injury_state,
                    next_tunnel.hard_hat_charges,
                    next_tunnel.reinforced_until,
                    next_tunnel.void_bait_digs,
                    i64::from(next_tunnel.sonar_skip_pending),
                    next_tunnel.temp_buffs,
                    next_tunnel.temp_curses,
                    next_tunnel.stat_strength,
                    next_tunnel.stat_smarts,
                    next_tunnel.stat_stamina,
                    next_tunnel.stat_points,
                    next_tunnel.paid_digs_today,
                    next_tunnel.paid_dig_date,
                    next_tunnel.pickaxe_tier,
                    next_tunnel.current_run_jc,
                    next_tunnel.current_run_artifacts,
                    next_tunnel.current_run_events,
                    next_tunnel.streak_days,
                    i64::from(next_tunnel.auto_buy_torch),
                    i64::from(next_tunnel.auto_buy_hard_hat),
                    next_tunnel.tunnel_name,
                    discord_id,
                    guild_id,
                    request.expected.depth,
                    request.expected.total_digs,
                    request.expected.last_dig_at,
                    next_tunnel.best_run_score,
                    next_tunnel.total_prestige_score,
                    next_tunnel.streak_last_date,
                    i64::from(next_tunnel.trap_active),
                    i64::from(next_tunnel.trap_free_today),
                    next_tunnel.trap_date,
                    next_tunnel.insured_until,
                    next_tunnel.revenge_target,
                    next_tunnel.revenge_type,
                    next_tunnel.revenge_until,
                    next_tunnel.cheer_data,
                    next_tunnel.grappling_hook_charges,
                    next_tunnel.lantern_stub_date,
                    next_tunnel.thick_skin_date,
                    next_tunnel.mutations,
                    next_tunnel.engine_mode,
                    next_tunnel.miner_origin,
                    next_tunnel.miner_about,
                    next_tunnel.stat_boss_awards,
                    next_tunnel.stinger_curse,
                    next_tunnel.last_lum_update_at,
                    next_tunnel.pinnacle_boss_id,
                    next_tunnel.pinnacle_phase,
                    next_tunnel.pinnacle_hp_remaining,
                    next_tunnel.pinnacle_last_engaged_at,
                    next_tunnel.retreat_cooldown_until,
                    next_tunnel.last_cheer_at,
                    next_tunnel.cavein_free_streak,
                    i64::from(next_tunnel.relic_trim_notice),
                ],
            )?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }

        let balance_cost = request.balance_cost.max(0);
        let mut balance_after_cost = request.expected.balance;
        if balance_cost > 0 {
            if balance_after_cost < balance_cost {
                return Err(DigRuntimeStoreError::InsufficientFunds);
            }
            balance_after_cost = balance_after_cost
                .checked_sub(balance_cost)
                .ok_or(DigRuntimeStoreError::InsufficientFunds)?;
            set_runtime_ledger_context(
                &transaction,
                discord_id,
                &request.action_type,
                &request.action_type,
                "paid dig cost",
                &request.detail,
            )?;
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
                params![
                    balance_after_cost,
                    discord_id,
                    guild_id,
                    request.expected.balance
                ],
            )?;
            clear_runtime_ledger_context(&transaction)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }
        // `request.next.balance` is the net post-policy balance.  Record the
        // gross reward first (net plus the explicit taxes), then withhold each
        // tax in its own ledger row.  All updates remain inside this
        // transaction so an audit/delivery failure cannot leave any side
        // of the split persisted.
        let net_reward_delta = request.next.balance.saturating_sub(balance_after_cost);
        let vanity_tax = request.vanity_tax.max(0);
        let low_priority_tax = request.low_priority_tax.max(0);
        let gross_reward_delta = net_reward_delta
            .saturating_add(vanity_tax)
            .saturating_add(low_priority_tax);
        let gross_balance = balance_after_cost.saturating_add(gross_reward_delta);
        if gross_reward_delta != 0 {
            set_runtime_ledger_context(
                &transaction,
                discord_id,
                &request.action_type,
                &request.action_type,
                runtime_ledger_reason(&request.action_type, gross_reward_delta),
                &request.detail,
            )?;
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
                params![gross_balance, discord_id, guild_id, balance_after_cost,],
            )?;
            clear_runtime_ledger_context(&transaction)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }
        let mut withheld_balance = gross_balance;
        if vanity_tax != 0 {
            set_runtime_ledger_context_source(
                &transaction,
                "vanity_tax",
                discord_id,
                &request.action_type,
                &request.action_type,
                "vanity tax on JC profit",
                &request.detail,
            )?;
            let next_balance = withheld_balance.saturating_sub(vanity_tax);
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
                params![next_balance, discord_id, guild_id, withheld_balance],
            )?;
            clear_runtime_ledger_context(&transaction)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
            withheld_balance = next_balance;
        }
        if low_priority_tax != 0 {
            set_runtime_ledger_context_source(
                &transaction,
                "low_priority_tax",
                discord_id,
                &request.action_type,
                &request.action_type,
                "low priority tax on JC profit",
                &request.detail,
            )?;
            let next_balance = withheld_balance.saturating_sub(low_priority_tax);
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
                params![next_balance, discord_id, guild_id, withheld_balance],
            )?;
            clear_runtime_ledger_context(&transaction)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }
        sync_inventory(
            &transaction,
            &request.next.inventory,
            discord_id,
            guild_id,
            request.now,
        )?;
        sync_artifacts(
            &transaction,
            &request.next.artifacts,
            discord_id,
            guild_id,
            request.now,
        )?;
        sync_gear(
            &transaction,
            &request.next.gear,
            discord_id,
            guild_id,
            request.now,
        )?;
        for item_id in &request.consumed_item_ids {
            // A dig can consume a queued item or spill an ordinary satchel
            // item.  The snapshot/CAS check above proves ownership, so the
            // same id list safely handles both removal paths.
            transaction.execute(
                "DELETE FROM dig_inventory WHERE id=?1 AND discord_id=?2 AND guild_id=?3",
                params![item_id, discord_id, guild_id],
            )?;
        }
        transaction.execute(
            "INSERT INTO dig_actions
             (guild_id,actor_id,target_id,action_type,depth_before,depth_after,jc_delta,detail,created_at)
             VALUES (?1,?2,NULL,?3,?4,?5,?6,?7,?8)",
            params![
                guild_id,
                discord_id,
                request.action_type,
                request.depth_before,
                request.depth_after,
                request.jc_delta,
                request.detail,
                request.now,
            ],
        )?;
        let action_id = transaction.last_insert_rowid();
        if let Some(mut draft) = request.delivery_draft {
            draft.outcome.action_id = Some(action_id);
            if let Some(delivery) = build_delivery_snapshot(
                &draft.outcome,
                draft.discord_id,
                draft.guild_id,
                draft.context,
                draft.committed_at,
            ) {
                let mut detail_value = serde_json::from_str::<Value>(&request.detail)
                    .map_err(|_| DigRuntimeStoreError::InvalidJson("dig action detail"))?;
                let object = detail_value
                    .as_object_mut()
                    .ok_or(DigRuntimeStoreError::InvalidJson("dig action detail"))?;
                object.insert(
                    "delivery".to_owned(),
                    serde_json::to_value(delivery)
                        .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?,
                );
                transaction.execute(
                    "UPDATE dig_actions SET detail=?1 WHERE id=?2 AND actor_id=?3 AND guild_id=?4",
                    params![detail_value.to_string(), action_id, discord_id, guild_id],
                )?;
            }
        }
        transaction.commit()?;
        Ok(DigRuntimeCommitReceipt {
            balance_after: request.next.balance,
            action_id,
            inserted_item_ids: Vec::new(),
            inserted_artifact_ids: Vec::new(),
            inserted_gear_ids: Vec::new(),
        })
    }

    fn ensure_weather(
        &self,
        guild_id: i64,
        game_date: &str,
        now: i64,
    ) -> Result<Vec<DigRuntimeWeather>, DigRuntimeStoreError> {
        DigWeatherRepository::new(&self.path)
            .ensure_for_day(guild_id, game_date, now)
            .map(|entries| entries.into_iter().map(Into::into).collect())
            .map_err(|error| DigRuntimeStoreError::Weather(error.to_string()))
    }

    fn attach_delivery(
        &self,
        delivery: &DigRuntimeDeliverySnapshot,
    ) -> Result<(), DigRuntimeStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let detail = transaction
            .query_row(
                "SELECT detail FROM dig_actions WHERE id=?1 AND actor_id=?2 AND guild_id=?3",
                params![delivery.action_id, delivery.discord_id, delivery.guild_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or(DigRuntimeStoreError::StateConflict)?;
        let mut value = detail
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let object = value
            .as_object_mut()
            .ok_or(DigRuntimeStoreError::InvalidJson("dig action detail"))?;
        object.insert(
            "delivery".to_owned(),
            serde_json::to_value(delivery)
                .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?,
        );
        let changed = transaction.execute(
            "UPDATE dig_actions SET detail=?1
             WHERE id=?2 AND actor_id=?3 AND guild_id=?4",
            params![
                value.to_string(),
                delivery.action_id,
                delivery.discord_id,
                delivery.guild_id
            ],
        )?;
        if changed != 1 {
            return Err(DigRuntimeStoreError::StateConflict);
        }
        transaction.commit()?;
        Ok(())
    }

    fn pending_deliveries(
        &self,
        query: DigRuntimePendingDeliveryQuery,
    ) -> Result<Vec<DigRuntimeDeliverySnapshot>, DigRuntimeStoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT detail FROM dig_actions
             WHERE action_type='dig'
               AND (?1 IS NULL OR guild_id=?1)
               AND (?2 IS NULL OR actor_id=?2)
             ORDER BY id ASC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                query.guild_id,
                query.discord_id,
                i64::try_from(query.limit).unwrap_or(i64::MAX)
            ],
            |row| row.get::<_, Option<String>>(0),
        )?;
        let mut deliveries = Vec::new();
        for row in rows {
            let Some(detail) = row? else { continue };
            let Some(raw) = serde_json::from_str::<Value>(&detail)
                .ok()
                .and_then(|value| value.get("delivery").cloned())
            else {
                continue;
            };
            let delivery = serde_json::from_value::<DigRuntimeDeliverySnapshot>(raw)
                .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?;
            if !delivery.blood_pact.is_terminal()
                || delivery.main_delivered_at.is_none()
                || (delivery.render.kind.requires_event_part()
                    && delivery.event_delivered_at.is_none())
            {
                deliveries.push(delivery);
            }
        }
        Ok(deliveries)
    }

    fn mark_delivery_delivered(
        &self,
        request: DigRuntimeMarkDelivered,
    ) -> Result<bool, DigRuntimeStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let detail = transaction
            .query_row(
                "SELECT detail FROM dig_actions WHERE id=?1",
                params![request.action_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        let Some(detail) = detail.flatten() else {
            transaction.commit()?;
            return Ok(false);
        };
        let Some(mut delivery) = serde_json::from_str::<Value>(&detail)
            .ok()
            .and_then(|value| value.get("delivery").cloned())
            .and_then(|value| serde_json::from_value::<DigRuntimeDeliverySnapshot>(value).ok())
        else {
            transaction.commit()?;
            return Ok(false);
        };
        if delivery.source_key != request.source_key {
            transaction.commit()?;
            return Ok(false);
        }
        match request.part {
            DigRuntimeDeliveryPart::Main => {
                if delivery.main_delivered_at.is_none() {
                    delivery.main_delivered_at = Some(request.delivered_at);
                }
            }
            DigRuntimeDeliveryPart::Event => {
                if delivery.event_delivered_at.is_none() {
                    delivery.event_delivered_at = Some(request.delivered_at);
                }
            }
        }
        let mut value = serde_json::from_str::<Value>(&detail)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("dig action detail"))?;
        value["delivery"] = serde_json::to_value(&delivery)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?;
        transaction.execute(
            "UPDATE dig_actions SET detail=?1 WHERE id=?2",
            params![value.to_string(), request.action_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    fn rebind_pending_delivery_channel(
        &self,
        request: DigRuntimeRebindDeliveryChannel,
    ) -> Result<DigRuntimeDeliverySnapshot, DigRuntimeStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let detail = transaction
            .query_row(
                "SELECT detail FROM dig_actions WHERE id=?1",
                params![request.action_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .ok_or(DigRuntimeStoreError::StateConflict)?;
        let mut value = serde_json::from_str::<Value>(&detail)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("dig action detail"))?;
        let raw = value
            .get("delivery")
            .cloned()
            .ok_or(DigRuntimeStoreError::StateConflict)?;
        let mut delivery = serde_json::from_value::<DigRuntimeDeliverySnapshot>(raw)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?;
        let part_pending = match request.part {
            DigRuntimeDeliveryPart::Main => delivery.main_delivered_at.is_none(),
            DigRuntimeDeliveryPart::Event => {
                delivery.render.kind.requires_event_part() && delivery.event_delivered_at.is_none()
            }
        };
        if delivery.source_key != request.source_key
            || delivery.context.channel_id != request.expected_channel_id
            || !part_pending
        {
            return Err(DigRuntimeStoreError::StateConflict);
        }
        delivery.context.channel_id = request.fallback_channel_id;
        value["delivery"] = serde_json::to_value(&delivery)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?;
        let changed = transaction.execute(
            "UPDATE dig_actions SET detail=?1 WHERE id=?2",
            params![value.to_string(), request.action_id],
        )?;
        if changed != 1 {
            return Err(DigRuntimeStoreError::StateConflict);
        }
        transaction.commit()?;
        Ok(delivery)
    }

    fn finalize_delivery(
        &self,
        request: DigRuntimeFinalizeDelivery,
    ) -> Result<DigRuntimeDeliverySnapshot, DigRuntimeStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let detail = transaction
            .query_row(
                "SELECT detail FROM dig_actions WHERE id=?1",
                params![request.action_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .ok_or(DigRuntimeStoreError::StateConflict)?;
        let mut value = serde_json::from_str::<Value>(&detail)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("dig action detail"))?;
        let raw = value
            .get("delivery")
            .cloned()
            .ok_or(DigRuntimeStoreError::StateConflict)?;
        let mut delivery = serde_json::from_value::<DigRuntimeDeliverySnapshot>(raw)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?;
        if delivery.source_key != request.source_key {
            return Err(DigRuntimeStoreError::StateConflict);
        }
        delivery.flavor = request.flavor;
        if let Some(boss) = request.boss {
            delivery.render.kind = DigRuntimeRenderKind::Boss;
            delivery.render.boss = Some(boss);
        }
        delivery.render.flavor_narrative = delivery.flavor.narrative().map(str::to_owned);
        value["delivery"] = serde_json::to_value(&delivery)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?;
        let changed = transaction.execute(
            "UPDATE dig_actions SET detail=?1 WHERE id=?2",
            params![value.to_string(), request.action_id],
        )?;
        if changed != 1 {
            return Err(DigRuntimeStoreError::StateConflict);
        }
        transaction.commit()?;
        Ok(delivery)
    }

    fn settle_blood_pact_delivery(
        &self,
        request: DigRuntimeSettleBloodPact,
        minigame_jc_delta_scale: f64,
    ) -> Result<DigRuntimeDeliverySnapshot, DigRuntimeStoreError> {
        // Read the pending immutable projection before opening the effect
        // transaction.  The Blood Pact repository opens its own
        // BEGIN IMMEDIATE transaction; holding a SQLite write transaction
        // here would turn the exact-once boundary into a lock inversion.
        let pending = {
            let connection = self.connection()?;
            let detail = connection
                .query_row(
                    "SELECT detail FROM dig_actions WHERE id=?1",
                    params![request.action_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .ok_or(DigRuntimeStoreError::StateConflict)?;
            let value = serde_json::from_str::<Value>(&detail)
                .map_err(|_| DigRuntimeStoreError::InvalidJson("dig action detail"))?;
            let raw = value
                .get("delivery")
                .cloned()
                .ok_or(DigRuntimeStoreError::StateConflict)?;
            let delivery = serde_json::from_value::<DigRuntimeDeliverySnapshot>(raw)
                .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?;
            if delivery.action_id != request.action_id || delivery.source_key != request.source_key
            {
                return Err(DigRuntimeStoreError::StateConflict);
            }
            delivery
        };

        if pending.blood_pact.is_terminal() {
            return Ok(pending);
        }

        let state = if pending.outcome.cave_in || pending.outcome.jc_earned <= 0 {
            DigRuntimeBloodPactSnapshot::Skipped
        } else {
            let event_key = format!(
                "dig-blood-pact:{}:{}",
                pending.action_id, pending.discord_id
            );
            let mana_date = game_date_for_timestamp(request.occurred_at as f64)
                .map_err(|error| DigRuntimeStoreError::BloodPact(error.to_string()))?;
            let mut settlement_request = DigBloodPactSettlementRequest::with_default_scale(
                pending.discord_id,
                pending.guild_id,
                pending.outcome.jc_earned,
                event_key,
                request.occurred_at,
                mana_date,
            );
            settlement_request.minigame_jc_delta_scale = minigame_jc_delta_scale;
            let settlement = DigBloodPactRepository::new(&self.path)
                .settle(&settlement_request)
                .map_err(|error| DigRuntimeStoreError::BloodPact(error.to_string()))?;
            if settlement.event.is_some() {
                DigRuntimeBloodPactSnapshot::Applied {
                    skimmed: settlement.applied_amount,
                }
            } else {
                DigRuntimeBloodPactSnapshot::Skipped
            }
        };

        // The repository effect and this projection update are intentionally
        // separate transactions.  If the process stops between them, the
        // stable hostile-loss event key makes the next call a duplicate-safe
        // reconciliation and then records the terminal state.  Reloading the
        // row also prevents a stale flavor/render update from being clobbered.
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let detail = transaction
            .query_row(
                "SELECT detail FROM dig_actions WHERE id=?1",
                params![request.action_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .ok_or(DigRuntimeStoreError::StateConflict)?;
        let mut value = serde_json::from_str::<Value>(&detail)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("dig action detail"))?;
        let current_raw = value
            .get("delivery")
            .cloned()
            .ok_or(DigRuntimeStoreError::StateConflict)?;
        let current = serde_json::from_value::<DigRuntimeDeliverySnapshot>(current_raw)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?;
        if current.action_id != request.action_id || current.source_key != request.source_key {
            return Err(DigRuntimeStoreError::StateConflict);
        }
        if current.blood_pact.is_terminal() {
            transaction.commit()?;
            return Ok(current);
        }
        if current.outcome.jc_earned != pending.outcome.jc_earned
            || current.outcome.cave_in != pending.outcome.cave_in
        {
            return Err(DigRuntimeStoreError::StateConflict);
        }
        let delivery_value = value
            .get_mut("delivery")
            .and_then(Value::as_object_mut)
            .ok_or(DigRuntimeStoreError::InvalidJson("delivery"))?;
        delivery_value.insert(
            "blood_pact".to_owned(),
            serde_json::to_value(&state)
                .map_err(|_| DigRuntimeStoreError::InvalidJson("blood_pact"))?,
        );
        let changed = transaction.execute(
            "UPDATE dig_actions SET detail=?1 WHERE id=?2",
            params![value.to_string(), request.action_id],
        )?;
        if changed != 1 {
            return Err(DigRuntimeStoreError::StateConflict);
        }
        let updated_delivery = value
            .get("delivery")
            .cloned()
            .ok_or(DigRuntimeStoreError::InvalidJson("delivery"))?;
        transaction.commit()?;
        serde_json::from_value(updated_delivery)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))
    }
}

fn sync_inventory(
    transaction: &Transaction<'_>,
    inventory: &[DigRuntimeInventoryItem],
    discord_id: i64,
    guild_id: i64,
    now: i64,
) -> Result<(), DigRuntimeStoreError> {
    for item in inventory {
        let changed = transaction.execute(
            "UPDATE dig_inventory SET queued=?1
             WHERE id=?2 AND discord_id=?3 AND guild_id=?4",
            params![i64::from(item.queued), item.id, discord_id, guild_id],
        )?;
        if changed == 0 {
            transaction.execute(
                "INSERT INTO dig_inventory
                 (discord_id,guild_id,item_type,queued,created_at)
                 VALUES (?1,?2,?3,?4,?5)",
                params![
                    discord_id,
                    guild_id,
                    item.item_type,
                    i64::from(item.queued),
                    now,
                ],
            )?;
        }
    }
    Ok(())
}

fn sync_artifacts(
    transaction: &Transaction<'_>,
    artifacts: &[DigRuntimeArtifact],
    discord_id: i64,
    guild_id: i64,
    now: i64,
) -> Result<(), DigRuntimeStoreError> {
    for artifact in artifacts {
        let changed = transaction.execute(
            "UPDATE dig_artifacts SET equipped=?1,artifact_id=?2,is_relic=?3
             WHERE id=?4 AND discord_id=?5 AND guild_id=?6",
            params![
                i64::from(artifact.equipped),
                artifact.artifact_id,
                i64::from(artifact.is_relic),
                artifact.id,
                discord_id,
                guild_id,
            ],
        )?;
        if changed == 0 {
            transaction.execute(
                "INSERT INTO dig_artifacts
                 (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    discord_id,
                    guild_id,
                    artifact.artifact_id,
                    now,
                    i64::from(artifact.is_relic),
                    i64::from(artifact.equipped),
                ],
            )?;
        }
    }
    Ok(())
}

fn sync_gear(
    transaction: &Transaction<'_>,
    gear: &[DigRuntimeGear],
    discord_id: i64,
    guild_id: i64,
    now: i64,
) -> Result<(), DigRuntimeStoreError> {
    for piece in gear {
        let changed = transaction.execute(
            "UPDATE dig_gear SET slot=?1,tier=?2,durability=?3,equipped=?4,
                    acquired_at=?5,source=?6,item_id=?7
             WHERE id=?8 AND discord_id=?9 AND guild_id=?10",
            params![
                piece.slot,
                piece.tier,
                piece.durability.max(0),
                i64::from(piece.equipped),
                piece.acquired_at,
                piece.source,
                piece.item_id,
                piece.id,
                discord_id,
                guild_id,
            ],
        )?;
        if changed == 0 {
            transaction.execute(
                "INSERT INTO dig_gear(
                     discord_id,guild_id,slot,tier,durability,equipped,
                     acquired_at,source,item_id
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    discord_id,
                    guild_id,
                    piece.slot,
                    piece.tier,
                    piece.durability.max(0),
                    i64::from(piece.equipped),
                    if piece.acquired_at == 0 {
                        now
                    } else {
                        piece.acquired_at
                    },
                    piece.source,
                    piece.item_id,
                ],
            )?;
        }
    }
    Ok(())
}

fn version_matches(existing: Option<(i64, i64, Option<i64>)>, expected: DigRuntimeVersion) -> bool {
    match (existing, expected.depth, expected.total_digs) {
        (None, None, None) => true,
        (Some((depth, total_digs, last_dig_at)), Some(expected_depth), Some(expected_total)) => {
            depth == expected_depth
                && total_digs == expected_total
                && last_dig_at == expected.last_dig_at
        }
        _ => false,
    }
}

fn load_inventory(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeInventoryItem>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id,item_type,queued FROM dig_inventory
         WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeInventoryItem {
                id: row.get(0)?,
                item_type: row.get(1)?,
                queued: row.get::<_, i64>(2)? != 0,
            })
        })?
        .collect()
}

fn load_inventory_transaction(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeInventoryItem>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT id,item_type,queued FROM dig_inventory
         WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeInventoryItem {
                id: row.get(0)?,
                item_type: row.get(1)?,
                queued: row.get::<_, i64>(2)? != 0,
            })
        })?
        .collect()
}

fn load_artifacts(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeArtifact>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id,artifact_id,is_relic,equipped FROM dig_artifacts
         WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeArtifact {
                id: row.get(0)?,
                artifact_id: row.get(1)?,
                is_relic: row.get::<_, i64>(2)? != 0,
                equipped: row.get::<_, i64>(3)? != 0,
            })
        })?
        .collect()
}

fn load_artifacts_transaction(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeArtifact>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT id,artifact_id,is_relic,equipped FROM dig_artifacts
         WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeArtifact {
                id: row.get(0)?,
                artifact_id: row.get(1)?,
                is_relic: row.get::<_, i64>(2)? != 0,
                equipped: row.get::<_, i64>(3)? != 0,
            })
        })?
        .collect()
}

fn load_gear(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeGear>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id,slot,tier,durability,equipped,acquired_at,source,item_id
         FROM dig_gear WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeGear {
                id: row.get(0)?,
                slot: row.get(1)?,
                tier: row.get(2)?,
                durability: row.get(3)?,
                equipped: row.get::<_, i64>(4)? != 0,
                acquired_at: row.get(5)?,
                source: row.get(6)?,
                item_id: row.get(7)?,
            })
        })?
        .collect()
}

fn load_gear_transaction(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeGear>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT id,slot,tier,durability,equipped,acquired_at,source,item_id
         FROM dig_gear WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeGear {
                id: row.get(0)?,
                slot: row.get(1)?,
                tier: row.get(2)?,
                durability: row.get(3)?,
                equipped: row.get::<_, i64>(4)? != 0,
                acquired_at: row.get(5)?,
                source: row.get(6)?,
                item_id: row.get(7)?,
            })
        })?
        .collect()
}
