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
        let balance = dig_runtime_store::player_balance(&transaction, discord_id, guild_id)?;
        let Some(balance) = balance else {
            return Err(DigRuntimeStoreError::MissingPlayer);
        };
        let depth_before = dig_runtime_store::tunnel_depth(&transaction, discord_id, guild_id)?;
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
            let changed = dig_runtime_store::update_player_balance_cas(
                &transaction,
                balance - cost,
                discord_id,
                guild_id,
                balance,
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
            let changed = dig_runtime_store::update_player_balance_cas(
                &transaction,
                balance_after,
                discord_id,
                guild_id,
                before,
            )?;
            clear_runtime_ledger_context(&transaction)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }
        let depth_after = depth_after.unwrap_or(depth_before);
        let changed = dig_runtime_store::update_tunnel_depth_cas(
            &transaction,
            depth_after,
            discord_id,
            guild_id,
            depth_before,
        )?;
        if changed != 1 {
            return Err(DigRuntimeStoreError::Conflict);
        }
        let action_id = dig_runtime_store::insert_dig_action(
            &transaction,
            guild_id,
            discord_id,
            None,
            action_type,
            depth_before,
            depth_after,
            balance_after - balance,
            detail,
            now,
        )?;
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
    dig_runtime_store::set_ledger_context(
        transaction,
        source,
        actor_id,
        related_type,
        related_id,
        reason,
        metadata,
    )
}

fn clear_runtime_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    dig_runtime_store::clear_ledger_context(transaction)
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
        let player = dig_runtime_store::player_balance(&connection, discord_id, guild_id)?;
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

        let tunnel = dig_runtime_store::tunnel(&connection, discord_id, guild_id)?;
        let inventory = dig_runtime_store::inventory(&connection, discord_id, guild_id)?;
        let artifacts = dig_runtime_store::artifacts(&connection, discord_id, guild_id)?;
        let gear = dig_runtime_store::gear(&connection, discord_id, guild_id)?;
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
        let updated = dig_runtime_store::update_slow_drip_claim_cas(
            &transaction,
            claim.gross_jc,
            claim.claimed_at,
            tunnel.discord_id,
            tunnel.guild_id,
            &claim.claim_date,
            claim.claimed_before,
            claim.expected_last_claim_at,
        )?;
        if updated != 1 {
            let inserted = dig_runtime_store::insert_slow_drip_claim(
                &transaction,
                tunnel.discord_id,
                tunnel.guild_id,
                &claim.claim_date,
                claim.gross_jc,
                claim.claimed_at,
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
                let balance = dig_runtime_store::player_balance(
                    &transaction,
                    tunnel.discord_id,
                    tunnel.guild_id,
                )?;
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
                let changed = dig_runtime_store::update_player_balance_coalesce_cas(
                    &transaction,
                    balance_after,
                    tunnel.discord_id,
                    tunnel.guild_id,
                    balance,
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
        let player_balance = dig_runtime_store::player_balance(
            &transaction,
            request
                .next
                .tunnel
                .as_ref()
                .map_or(0, |tunnel| tunnel.discord_id),
            request
                .next
                .tunnel
                .as_ref()
                .map_or(0, |tunnel| tunnel.guild_id),
        )?;
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
            let exists = dig_runtime_store::inventory_item_exists(
                &transaction,
                *item_id,
                discord_id,
                guild_id,
            )?;
            if !exists {
                return Err(DigRuntimeStoreError::MissingQueuedItem(*item_id));
            }
        }

        let existing_tunnel = dig_runtime_store::tunnel(&transaction, discord_id, guild_id)?;
        let existing = existing_tunnel
            .as_ref()
            .map(|tunnel| (tunnel.depth, tunnel.total_digs, tunnel.last_dig_at));
        let live_inventory = dig_runtime_store::inventory(&transaction, discord_id, guild_id)?;
        let live_artifacts = dig_runtime_store::artifacts(&transaction, discord_id, guild_id)?;
        let live_gear = dig_runtime_store::gear(&transaction, discord_id, guild_id)?;
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
            let changed =
                dig_runtime_store::claim_pet_dig_work(&transaction, &claim, discord_id, guild_id)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::PetWorkConflict);
            }
        }

        if existing_tunnel.is_none() {
            dig_runtime_store::insert_tunnel(&transaction, next_tunnel)?;
            // Python's tunnel constructor creates and equips a starter weapon
            // in the same admission transaction.  Keeping that invariant here
            // matters because the first dig snapshots gear before applying
            // pickaxe modifiers; a tunnel without this row silently falls
            // back to a procedural tier and loses the migrated gear identity.
            dig_runtime_store::insert_starter_weapon(
                &transaction,
                discord_id,
                guild_id,
                next_tunnel.pickaxe_tier,
                request.now,
            )?;
        } else {
            let changed = dig_runtime_store::update_tunnel_cas(
                &transaction,
                next_tunnel,
                request.expected.depth,
                request.expected.total_digs,
                request.expected.last_dig_at,
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
            let changed = dig_runtime_store::update_player_balance_coalesce_cas(
                &transaction,
                balance_after_cost,
                discord_id,
                guild_id,
                request.expected.balance,
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
            let changed = dig_runtime_store::update_player_balance_coalesce_cas(
                &transaction,
                gross_balance,
                discord_id,
                guild_id,
                balance_after_cost,
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
            let changed = dig_runtime_store::update_player_balance_coalesce_cas(
                &transaction,
                next_balance,
                discord_id,
                guild_id,
                withheld_balance,
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
            let changed = dig_runtime_store::update_player_balance_coalesce_cas(
                &transaction,
                next_balance,
                discord_id,
                guild_id,
                withheld_balance,
            )?;
            clear_runtime_ledger_context(&transaction)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }
        dig_runtime_store::sync_inventory(
            &transaction,
            &request.next.inventory,
            discord_id,
            guild_id,
            request.now,
        )?;
        dig_runtime_store::sync_artifacts(
            &transaction,
            &request.next.artifacts,
            discord_id,
            guild_id,
            request.now,
        )?;
        dig_runtime_store::sync_gear(
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
            dig_runtime_store::delete_inventory_item(&transaction, *item_id, discord_id, guild_id)?;
        }
        let action_id = dig_runtime_store::insert_dig_action(
            &transaction,
            guild_id,
            discord_id,
            None,
            &request.action_type,
            request.depth_before,
            request.depth_after,
            request.jc_delta,
            &request.detail,
            request.now,
        )?;
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
                dig_runtime_store::update_dig_action_detail_for_actor(
                    &transaction,
                    &detail_value.to_string(),
                    action_id,
                    discord_id,
                    guild_id,
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
        let detail = dig_runtime_store::dig_action_detail_for_actor(
            &transaction,
            delivery.action_id,
            delivery.discord_id,
            delivery.guild_id,
        )?
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
        let changed = dig_runtime_store::update_dig_action_detail_for_actor(
            &transaction,
            &value.to_string(),
            delivery.action_id,
            delivery.discord_id,
            delivery.guild_id,
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
        let rows = dig_runtime_store::dig_action_details_for_delivery(
            &connection,
            query.guild_id,
            query.discord_id,
            i64::try_from(query.limit).unwrap_or(i64::MAX),
        )?;
        let mut deliveries = Vec::new();
        for row in rows {
            let Some(detail) = row else { continue };
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
        let detail = dig_runtime_store::dig_action_detail(&transaction, request.action_id)?;
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
        dig_runtime_store::update_dig_action_detail(
            &transaction,
            &value.to_string(),
            request.action_id,
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
        let detail = dig_runtime_store::dig_action_detail(&transaction, request.action_id)?
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
        let changed = dig_runtime_store::update_dig_action_detail(
            &transaction,
            &value.to_string(),
            request.action_id,
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
        let detail = dig_runtime_store::dig_action_detail(&transaction, request.action_id)?
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
        let changed = dig_runtime_store::update_dig_action_detail(
            &transaction,
            &value.to_string(),
            request.action_id,
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
            let detail = dig_runtime_store::dig_action_detail(&connection, request.action_id)?
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
        let detail = dig_runtime_store::dig_action_detail(&transaction, request.action_id)?
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
        let changed = dig_runtime_store::update_dig_action_detail(
            &transaction,
            &value.to_string(),
            request.action_id,
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
