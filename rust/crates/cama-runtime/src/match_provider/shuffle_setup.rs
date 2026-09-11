use super::*;
use crate::discord_transport::DiscordMessageReceipt;
use cama_domain::dota_hosting::FirstPick;

impl MatchHandler {
    pub(super) fn prepare_shuffle(
        &self,
        request: PrepareShuffleRequest,
    ) -> Result<PreparedShuffle, String> {
        let dota_hosting = GuildConfigRepository::new(&self.database_path, false)
            .dota_hosting_options(request.guild_id)
            .map_err(|error| error.to_string())?
            .merged(&request.dota_hosting);
        dota_hosting.validate()?;
        if !matches!(
            request.rating_system.as_str(),
            "glicko" | "openskill" | "jopacoin"
        ) {
            return Err("rating_system must be 'glicko', 'openskill', or 'jopacoin'".to_owned());
        }
        if !matches!(request.shuffle_mode.as_str(), "balanced" | "region") {
            return Err("shuffle_mode must be 'balanced' or 'region'".to_owned());
        }
        let ShuffleInputs {
            mut players,
            last_match_dates,
            exclusion_counts,
        } = self
            .players
            .get_shuffle_inputs(&request.player_ids, Some(request.guild_id))
            .map_err(|error| error.to_string())?;
        if players.len() != request.player_ids.len() {
            return Err(format!(
                "Could not load all players: expected {}, got {}",
                request.player_ids.len(),
                players.len()
            ));
        }
        if players.len() < 10 {
            return Err("Need at least 10 players to shuffle.".to_owned());
        }

        let now = Utc::now();
        for player in &mut players {
            let Some(player_id) = player.discord_id else {
                continue;
            };
            let Some(last_match) = last_match_dates.get(&player_id).and_then(sql_datetime) else {
                continue;
            };
            let days_since = (now - last_match).num_days().max(0);
            if let Some(rd) = player.glicko_rd {
                player.glicko_rd = Some(
                    self.rating
                        .apply_rd_decay(rd, i32::try_from(days_since).unwrap_or(i32::MAX)),
                );
            }
            if let Some(sigma) = player.os_sigma {
                player.os_sigma = Some(self.openskill.apply_sigma_decay(sigma, days_since));
            }
        }

        // Per-role win/loss drives the role-performance multiplier. Loaded
        // once for the whole pool: the factor varies by assigned role, but the
        // underlying record does not, so the search can reuse it across every
        // candidate arrangement.
        let mut records_by_player: BTreeMap<i64, BTreeMap<String, RoleRecord>> = BTreeMap::new();
        for ((discord_id, role), (wins, losses)) in MatchRepository::new(&self.database_path)
            .role_records(&request.player_ids, Some(request.guild_id))
            .map_err(|error| error.to_string())?
        {
            records_by_player
                .entry(discord_id)
                .or_default()
                .insert(role, RoleRecord::new(wins, losses));
        }
        for player in &mut players {
            if let Some(player_id) = player.discord_id
                && let Some(records) = records_by_player.remove(&player_id)
            {
                player.role_records = records;
            }
        }

        let mut rating_system = request.rating_system.clone();
        if rating_system == "openskill" && players.iter().any(|player| player.os_mu.is_none()) {
            rating_system = "glicko".to_owned();
        }
        let use_openskill = rating_system == "openskill";
        let use_jopacoin = rating_system == "jopacoin";
        let avoids = self
            .avoids
            .get_active_avoids_for_players(Some(request.guild_id), &request.player_ids)
            .map_err(|error| error.to_string())?;
        let deals = self
            .deals
            .get_active_deals_for_players(Some(request.guild_id), &request.player_ids)
            .map_err(|error| error.to_string())?;
        let low_priority_ids = self
            .low_priority
            .get_active_ids(&request.player_ids, Some(request.guild_id))
            .map_err(|error| error.to_string())?;
        // Low priority makes a player's games harder by balancing them as
        // though they were stronger, until they win their way out. This scales
        // only the value the shuffler compares; stored ratings, profiles, and
        // the post-match rating update all read the untouched player. Jopacoin
        // mode is excluded deliberately: its "rating" is a signed balance, so
        // scaling would drive a debtor further negative and hand them easier
        // games instead of harder ones.
        if !use_jopacoin {
            for player in &mut players {
                if player
                    .discord_id
                    .is_some_and(|discord_id| low_priority_ids.contains(&discord_id))
                {
                    player.matchmaking_multiplier = Some(self.config.low_priority_mmr_multiplier);
                }
            }
        }
        let domain_avoids = avoids
            .iter()
            .map(|avoid| SoftAvoid {
                avoider_discord_id: avoid.avoider_discord_id,
                avoided_discord_id: avoid.avoided_discord_id,
            })
            .collect::<Vec<_>>();
        let domain_deals = deals
            .iter()
            .map(|deal| PackageDeal {
                buyer_discord_id: deal.buyer_discord_id,
                partner_discord_id: deal.partner_discord_id,
            })
            .collect::<Vec<_>>();
        let constraints = ShuffleConstraints {
            avoids: Some(&domain_avoids),
            deals: Some(&domain_deals),
            low_priority_ids: Some(&low_priority_ids),
        };

        let mut shuffler = BalancedShuffler::default();
        shuffler.use_glicko = true;
        shuffler.use_openskill = use_openskill;
        shuffler.use_jopacoin = use_jopacoin;
        shuffler.off_role_multiplier = self.config.off_role_multiplier;
        shuffler.off_role_flat_value_penalty = self.config.off_role_flat_value_penalty;
        shuffler.off_role_flat_penalty = self.config.off_role_flat_penalty;
        shuffler.exclusion_penalty_weight = self.config.exclusion_penalty_weight;
        shuffler.rd_priority_weight = self.config.rd_priority_weight;
        shuffler.recent_match_penalty_weight = self.config.recent_match_penalty_weight;
        shuffler.soft_avoid_penalty = self.config.soft_avoid_penalty;
        shuffler.package_deal_penalty = self.config.package_deal_penalty;
        shuffler.package_deal_split_penalty = self.config.package_deal_split_penalty;
        shuffler.rating_spread_divisor = self.config.rating_spread_divisor;
        shuffler.region_split = request.shuffle_mode == "region";
        shuffler.region_split_penalty = self.config.region_split_penalty;

        let recent_match_ids = last_match_participant_ids(&self.database_path, request.guild_id)?;
        let recent_match_names = players
            .iter()
            .filter(|player| {
                player
                    .discord_id
                    .is_some_and(|id| recent_match_ids.contains(&id))
            })
            .map(|player| player.name.clone())
            .collect::<HashSet<_>>();
        let exclusion_counts = players
            .iter()
            .filter_map(|player| {
                player.discord_id.map(|player_id| {
                    (
                        player.name.clone(),
                        usize::try_from(
                            exclusion_counts
                                .get(&player_id)
                                .copied()
                                .unwrap_or_default()
                                .max(0),
                        )
                        .unwrap_or(usize::MAX),
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        let result = shuffler
            .shuffle_from_pool(
                &players,
                PoolOptions {
                    exclusion_counts: Some(&exclusion_counts),
                    recent_match_names: Some(&recent_match_names),
                    constraints,
                    lobby_wait_minutes: Some(&request.lobby_wait_minutes),
                    sampling_seed: fastrand::u64(..),
                },
            )
            .map_err(|error| error.to_string())?;
        let (radiant_team, dire_team) = if fastrand::bool() {
            (result.team1, result.team2)
        } else {
            (result.team2, result.team1)
        };
        let excluded_players = result.excluded;
        let radiant_ids = team_player_ids(&radiant_team)?;
        let dire_ids = team_player_ids(&dire_team)?;
        let excluded_ids = player_ids(&excluded_players);
        let radiant_roles = radiant_team
            .role_assignments
            .clone()
            .ok_or_else(|| "Radiant role assignments were not produced".to_owned())?;
        let dire_roles = dire_team
            .role_assignments
            .clone()
            .ok_or_else(|| "Dire role assignments were not produced".to_owned())?;
        let radiant_value = radiant_team
            .get_team_value_with_off_role_value_penalty(
                true,
                shuffler.off_role_multiplier,
                use_openskill,
                use_jopacoin,
                shuffler.off_role_flat_value_penalty,
            )
            .map_err(|error| error.to_string())?;
        let dire_value = dire_team
            .get_team_value_with_off_role_value_penalty(
                true,
                shuffler.off_role_multiplier,
                use_openskill,
                use_jopacoin,
                shuffler.off_role_flat_value_penalty,
            )
            .map_err(|error| error.to_string())?;
        let value_diff = (radiant_value - dire_value).abs();

        let balancing = TeamBalancingService::new_with_off_role_value_penalty(
            true,
            shuffler.off_role_multiplier,
            shuffler.off_role_flat_value_penalty,
            shuffler.off_role_flat_penalty,
            shuffler.role_matchup_delta_weight,
        );
        let off_role_penalty = (radiant_team
            .get_off_role_count()
            .map_err(|error| error.to_string())?
            + dire_team
                .get_off_role_count()
                .map_err(|error| error.to_string())?) as f64
            * shuffler.off_role_flat_penalty;
        let weighted_parity_delta = (balancing
            .calculate_role_matchup_delta(&radiant_team, &dire_team, use_openskill, use_jopacoin)
            .map_err(|error| error.to_string())?
            + balancing
                .calculate_role_parity_delta(&radiant_team, &dire_team, use_openskill, use_jopacoin)
                .map_err(|error| error.to_string())?)
            * shuffler.role_matchup_delta_weight;
        let radiant_set = radiant_ids.iter().copied().collect::<HashSet<_>>();
        let dire_set = dire_ids.iter().copied().collect::<HashSet<_>>();
        let selected_set = radiant_set
            .union(&dire_set)
            .copied()
            .collect::<HashSet<_>>();
        let excluded_set = excluded_ids.iter().copied().collect::<HashSet<_>>();
        let excluded_penalty = excluded_players
            .iter()
            .map(|player| {
                exclusion_counts
                    .get(&player.name)
                    .copied()
                    .unwrap_or_default()
            })
            .sum::<usize>() as f64
            * shuffler.exclusion_penalty_weight;
        let recent_match_penalty = radiant_team
            .players
            .iter()
            .chain(&dire_team.players)
            .filter(|player| recent_match_names.contains(&player.name))
            .count() as f64
            * shuffler.recent_match_penalty_weight;
        let soft_avoid_penalty = shuffler.calculate_soft_avoid_penalty(
            &radiant_set,
            &dire_set,
            Some(&domain_avoids),
            Some(&low_priority_ids),
        );
        let package_deal_penalty = shuffler.calculate_package_deal_penalty(
            &radiant_set,
            &dire_set,
            Some(&domain_deals),
            Some(&low_priority_ids),
        );
        let deal_split_penalty = shuffler.calculate_package_deal_split_penalty(
            &selected_set,
            &excluded_set,
            Some(&domain_deals),
            Some(&low_priority_ids),
        );
        let low_priority_penalty = BalancedShuffler::calculate_low_priority_penalty(
            &selected_set,
            Some(&low_priority_ids),
        );
        let low_priority_team_adjustment = BalancedShuffler::calculate_low_priority_team_adjustment(
            &radiant_set,
            &dire_set,
            Some(&low_priority_ids),
        );
        let region_split_penalty = if request.shuffle_mode == "region" {
            region_split_mismatches(
                &region_inputs(&radiant_team.players),
                &region_inputs(&dire_team.players),
            ) as f64
                * shuffler.region_split_penalty
        } else {
            0.0
        };
        let selected_players = radiant_team
            .players
            .iter()
            .chain(&dire_team.players)
            .collect::<Vec<_>>();
        let selected_values = selected_players
            .iter()
            .map(|player| player.get_value(true, use_openskill, use_jopacoin))
            .collect::<Vec<_>>();
        let goodness_score = value_diff * ADJUSTED_VALUE_DIFF_WEIGHT
            + off_role_penalty
            + weighted_parity_delta
            + excluded_penalty
            + recent_match_penalty
            + soft_avoid_penalty
            + package_deal_penalty
            + deal_split_penalty
            + low_priority_penalty
            + low_priority_team_adjustment
            + region_split_penalty
            + shuffler.calculate_rating_spread_penalty(&selected_values)
            - BalancedShuffler::calculate_lobby_rating_bonus(&selected_values)
            - BalancedShuffler::calculate_lobby_wait_bonus(
                &selected_players,
                Some(&request.lobby_wait_minutes),
            )
            - shuffler.calculate_rd_priority(&selected_players);

        let glicko_radiant_win_prob =
            glicko_probability(&self.rating, &radiant_team.players, &dire_team.players);
        let openskill_radiant_win_prob =
            openskill_probability(&self.openskill, &radiant_team.players, &dire_team.players)?;
        let included_ids = selected_set;
        let effective_avoid_ids = avoids
            .iter()
            .filter(|avoid| {
                included_ids.contains(&avoid.avoider_discord_id)
                    && included_ids.contains(&avoid.avoided_discord_id)
                    && ((radiant_set.contains(&avoid.avoider_discord_id)
                        && dire_set.contains(&avoid.avoided_discord_id))
                        || (dire_set.contains(&avoid.avoider_discord_id)
                            && radiant_set.contains(&avoid.avoided_discord_id)))
            })
            .map(|avoid| avoid.id)
            .collect();
        let effective_deal_ids = deals
            .iter()
            .filter(|deal| {
                (radiant_set.contains(&deal.buyer_discord_id)
                    && radiant_set.contains(&deal.partner_discord_id))
                    || (dire_set.contains(&deal.buyer_discord_id)
                        && dire_set.contains(&deal.partner_discord_id))
            })
            .map(|deal| deal.id)
            .collect();

        let mut state = PendingMatchState {
            radiant_team_ids: radiant_ids.clone(),
            dire_team_ids: dire_ids.clone(),
            excluded_player_ids: excluded_ids,
            excluded_conditional_player_ids: request.excluded_conditional_ids.clone(),
            radiant_roles,
            dire_roles,
            radiant_value,
            dire_value,
            value_diff,
            first_pick_team: Some(
                match dota_hosting.first_pick {
                    Some(FirstPick::Radiant) => "Radiant",
                    Some(FirstPick::Dire) => "Dire",
                    Some(FirstPick::Random) | None => first_pick_team(fastrand::bool()),
                }
                .to_owned(),
            ),
            shuffle_timestamp: Some(request.shuffle_timestamp),
            bet_lock_until: Some(
                request
                    .shuffle_timestamp
                    .checked_add(self.config.bet_lock_seconds)
                    .ok_or_else(|| "betting lock timestamp overflow".to_owned())?,
            ),
            betting_mode: "pool".to_owned(),
            is_bomb_pot: request.is_bomb_pot,
            is_openskill_shuffle: use_openskill,
            balancing_rating_system: rating_system,
            lobby_kind: Some(lobby_kind_value(request.lobby_kind).to_owned()),
            effective_avoid_ids,
            effective_deal_ids,
            exclusion_updates_deferred: true,
            full_exclusion_increment_ids: excluded_set.into_iter().collect(),
            half_exclusion_increment_ids: Vec::new(),
            ..PendingMatchState::default()
        };
        if let Some(message_id) = request.source_lobby_message_id {
            state
                .extra
                .insert("spectator_lobby_message_id".into(), json!(message_id));
        }
        state
            .extra
            .insert("shuffle_mode".to_owned(), json!(request.shuffle_mode));
        state
            .extra
            .insert("dota_hosting".to_owned(), json!(dota_hosting));
        state
            .extra
            .insert("goodness_score".to_owned(), json!(goodness_score));
        state.extra.insert(
            "glicko_radiant_win_prob".to_owned(),
            json!(glicko_radiant_win_prob),
        );
        state.extra.insert(
            "openskill_radiant_win_prob".to_owned(),
            json!(openskill_radiant_win_prob),
        );
        let source = self.lobbies.snapshot(request.guild_id, request.lobby_kind);
        let managed_setup = source.is_some() || request.source_lobby_message_id.is_some();
        if managed_setup {
            state
                .extra
                .insert("shuffle_setup_complete".into(), json!(false));
            state.extra.insert(
                "_cama_shuffle_setup".into(),
                json!({
                    "phase":"preparing", "lease_until":unix_seconds().saturating_add(300),
                    "source_channel":source.as_ref().and_then(|s|s.lobby_channel_id),
                    "source_message":request.source_lobby_message_id,
                    "source_thread":source.as_ref().and_then(|s|s.thread_id),
                    "origin_channel":source.as_ref().and_then(|s|s.origin_channel_id),
                }),
            );
        }
        let mut pending = self
            .pending
            .create_pending_match(request.guild_id, &state)
            .map_err(|error| error.to_string())?;

        let pending_id = pending.pending_match_id;
        let setup_result = (|| -> Result<PreparedShuffle, String> {
            let mut first_game_reserved = 0;
            if self.config.first_game_pool_daily_amount > 0 {
                let game_date = cama_domain::game_date::get_game_date();
                self.seeds
                    .fund_first_game_pools(
                        Some(request.guild_id),
                        &game_date,
                        self.config.first_game_pool_daily_amount,
                    )
                    .map_err(|error| error.to_string())?;
                first_game_reserved = self
                    .seeds
                    .claim_first_game_pool(
                        Some(request.guild_id),
                        match request.lobby_kind {
                            LobbyKind::Open => FirstGameLobby::Open,
                            LobbyKind::LowSkill => FirstGameLobby::LowSkill,
                        },
                        &game_date,
                        pending.pending_match_id,
                    )
                    .map_err(|error| error.to_string())?;
            }
            self.seeds
                .reserve_seed_atomic(
                    Some(request.guild_id),
                    pending.pending_match_id,
                    self.config.dota_bet_seed_amount,
                    first_game_reserved,
                    SeedBettingMode::Pool,
                )
                .map_err(|error| error.to_string())?;
            pending = self
                .pending
                .pending_match(request.guild_id, pending.pending_match_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "pending match disappeared after seed reservation".to_owned())?;

            // Python snapshots configured-investment eligibility before the blind
            // batch debits wallets, then sizes each position from the post-blind
            // balance. Keep that two-snapshot contract across the DB hook rather
            // than letting the blind transaction change the 50-JC gate.
            let investment_starting_balances = {
                let target_ids = radiant_ids
                    .iter()
                    .chain(dire_ids.iter())
                    .copied()
                    .collect::<Vec<_>>();
                let investments = AutobetInvestmentRepository::new(&self.database_path);
                let positions = investments
                    .for_targets(Some(request.guild_id), &target_ids)
                    .map_err(|e| format!("configured-investment position snapshot: {e}"))?;
                let investor_ids = positions.iter().map(|p| p.investor_id).collect::<Vec<_>>();
                self.players
                    .get_balances_bulk(&investor_ids, Some(request.guild_id))
                    .map_err(|e| format!("configured-investment balance snapshot: {e}"))?
                    .into_iter()
                    .collect::<BTreeMap<_, _>>()
            };

            let blind_bets = if self.config.auto_blind_enabled {
                let candidates = radiant_ids
                    .iter()
                    .copied()
                    .map(|discord_id| BlindBetCandidate {
                        discord_id,
                        team: BettingTeam::Radiant,
                        ante_override: None,
                    })
                    .chain(
                        dire_ids
                            .iter()
                            .copied()
                            .map(|discord_id| BlindBetCandidate {
                                discord_id,
                                team: BettingTeam::Dire,
                                ante_override: None,
                            }),
                    )
                    .collect::<Vec<_>>();
                match self.bets.create_auto_blind_bets_atomic(
                    Some(request.guild_id),
                    Some(pending.pending_match_id),
                    request.shuffle_timestamp,
                    &candidates,
                    BlindBetPolicy {
                        is_bomb_pot: request.is_bomb_pot,
                        normal_threshold: self.config.auto_blind_threshold,
                        normal_percentage: percentage_points(self.config.auto_blind_percentage),
                        bomb_percentage: percentage_points(self.config.bomb_pot_blind_percentage),
                        bomb_ante: self.config.bomb_pot_ante,
                        max_debt: self.config.max_debt,
                    },
                ) {
                    Ok(outcome) => Some(outcome),
                    Err(error) => {
                        warn!(
                            %error,
                            pending_match_id = pending.pending_match_id,
                            "automatic blind bets failed"
                        );
                        return Err(error.to_string());
                    }
                }
            } else {
                None
            };

            // Python's `create_match_automatic_bets` runs the three automatic
            // liquidity policies in this order: blind bets, configured
            // long/short investments, then spectator balancing. Failed setup is
            // compensated before publication; an interrupted setup stays gated
            // until recovery refunds it. No partial batch is blindly replayed.
            let mut automatic_summary = blind_bets
                .as_ref()
                .map(|outcome| {
                    blind_outcome_json(outcome, self.config.clone(), request.is_bomb_pot)
                })
                .unwrap_or_else(|| {
                    json!({
                        "created": 0,
                        "total_radiant": 0,
                        "total_dire": 0,
                        "percentage": if request.is_bomb_pot {
                            self.config.bomb_pot_blind_percentage
                        } else {
                            self.config.auto_blind_percentage
                        },
                        "is_bomb_pot": request.is_bomb_pot,
                        "bets": [],
                        "skipped": [],
                    })
                });

            match self.bets.create_configured_investment_bets_for_shuffle(
                ConfiguredInvestmentRequest {
                    guild_id: Some(request.guild_id),
                    pending_match_id: Some(pending.pending_match_id),
                    bet_time: request.shuffle_timestamp,
                    radiant_ids: radiant_ids.clone(),
                    dire_ids: dire_ids.clone(),
                    max_debt: self.config.max_debt,
                    starting_balances: investment_starting_balances,
                },
            ) {
                Ok(outcome) => {
                    let target_ids = outcome
                        .created
                        .iter()
                        .filter_map(|bet| bet.investment_target_id)
                        .collect::<BTreeSet<_>>();
                    let investment_metadata = AutobetInvestmentRepository::new(&self.database_path)
                        .for_targets(
                            Some(request.guild_id),
                            &target_ids.iter().copied().collect::<Vec<_>>(),
                        )
                        .map(|positions| {
                            positions
                                .into_iter()
                                .map(|position| {
                                    (
                                        (
                                            position.investor_id,
                                            position.target_id,
                                            position.direction,
                                        ),
                                        position.percentage,
                                    )
                                })
                                .collect::<BTreeMap<_, _>>()
                        })
                        .unwrap_or_default();
                    automatic_summary["investment_bets"] =
                        automatic_outcome_json(&outcome, &BTreeMap::new(), &investment_metadata);
                }
                Err(error) => return Err(format!("configured-investment setup: {error}")),
            }

            if self.config.auto_spectator_bet_enabled {
                let candidates = self
                    .players
                    .get_all(Some(request.guild_id))
                    .map_err(|e| format!("automatic spectator candidate lookup: {e}"))?
                    .into_iter()
                    .filter_map(|player| {
                        player.discord_id.map(|discord_id| WealthSnapshot {
                            discord_id,
                            balance: player.jopacoin_balance,
                        })
                    })
                    .collect::<Vec<_>>();
                let participant_ids = radiant_ids
                    .iter()
                    .chain(dire_ids.iter())
                    .copied()
                    .collect::<BTreeSet<_>>();
                let plan = plan_automatic_spectator_bets(
                    &candidates,
                    &participant_ids,
                    AutomaticSpectatorConfig {
                        enabled: true,
                        total_count: self.config.auto_spectator_bet_count,
                        top_count: self.config.auto_spectator_bet_top_count,
                        base_basis_points: percentage_basis_points(
                            self.config.auto_spectator_bet_percentage,
                        ),
                        top_basis_points: percentage_basis_points(
                            self.config.auto_spectator_bet_top_percentage,
                        ),
                    },
                );
                let requests = plan
                    .bets
                    .iter()
                    .map(|bet| AutomaticBasisPointBetRequest {
                        discord_id: bet.discord_id,
                        team: bet.team,
                        basis_points: bet.basis_points,
                    })
                    .collect::<Vec<_>>();
                match self.bets.place_automatic_basis_point_bets_atomic(
                    Some(request.guild_id),
                    Some(pending.pending_match_id),
                    request.shuffle_timestamp,
                    &requests,
                    self.config.max_debt,
                ) {
                    Ok(outcome) => {
                        automatic_summary["spectator_bets"] = automatic_outcome_json(
                            &outcome,
                            &plan
                                .bets
                                .iter()
                                .map(|bet| (bet.discord_id, (bet.networth, bet.basis_points)))
                                .collect::<BTreeMap<_, _>>(),
                            &BTreeMap::new(),
                        );
                    }
                    Err(error) => return Err(format!("spectator wager setup: {error}")),
                }
            }

            let has_automatic_bets = automatic_summary
                .get("created")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default()
                > 0
                || automatic_summary
                    .get("investment_bets")
                    .and_then(|value| value.get("created"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default()
                    > 0
                || automatic_summary
                    .get("spectator_bets")
                    .and_then(|value| value.get("created"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default()
                    > 0;
            if has_automatic_bets {
                pending = self
                    .pending
                    .mutate_pending_match(
                        request.guild_id,
                        pending.pending_match_id,
                        move |state| state.blind_bets_result = Some(automatic_summary),
                    )
                    .map_err(|error| error.to_string())?
                    .map(|(record, ())| record)
                    .ok_or_else(|| "pending match disappeared after automatic bets".to_owned())?;
            }

            if managed_setup {
                pending = self
                    .pending
                    .mutate_pending_match(request.guild_id, pending_id, |state| {
                        state
                            .extra
                            .get_mut("_cama_shuffle_setup")
                            .expect("saved setup")["phase"] = json!("prepared");
                    })
                    .map_err(|e| e.to_string())?
                    .ok_or("shuffle disappeared during setup")?
                    .0;
            }
            Ok(PreparedShuffle { pending })
        })();
        match setup_result {
            Ok(prepared) => Ok(prepared),
            Err(error) => {
                match self
                    .bets
                    .abort_pending_match_atomic(Some(request.guild_id), pending_id, &[])
                {
                    Ok(_) => Err(format!("Shuffle setup failed and was refunded: {error}")),
                    Err(cleanup) => Err(format!(
                        "Shuffle setup paused; automatic cleanup will retry for Match #{pending_id}: {error}; {cleanup}"
                    )),
                }
            }
        }
    }

    pub(super) async fn finalize_shuffle(
        &self,
        context: MatchCommandContext,
        responder: Arc<dyn InteractionResponder>,
        snapshot: MatchLobbySnapshot,
        mut prepared: PreparedShuffle,
    ) -> Result<(), InteractionHandlerError> {
        let Some(_guard) = self
            .try_acquire_finalization_guard(context.guild_id, prepared.pending.pending_match_id)
        else {
            return Err("Shuffle finalization already in progress; publication will retry.".into());
        };
        if prepared
            .pending
            .state
            .extra
            .contains_key("_cama_shuffle_setup")
        {
            let journal = json!({"phase":"prepared", "lease_until":unix_seconds().saturating_add(300),
                "source_channel":snapshot.lobby_channel_id,"source_message":snapshot.lobby_message_id,
                "source_thread":snapshot.thread_id,"origin_channel":snapshot.origin_channel_id,
                "command_channel":context.channel_id});
            let repo = self.pending.clone();
            let guild = context.guild_id;
            let id = prepared.pending.pending_match_id;
            prepared.pending = tokio::task::spawn_blocking(move || {
                repo.mutate_pending_match(guild, id, move |s| {
                    s.extra.insert("_cama_shuffle_setup".into(), journal);
                })
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?
            .ok_or("shuffle was aborted before publication")?
            .0;
        }
        let guild_id =
            u64::try_from(context.guild_id).map_err(|_| "guild ID is outside Discord's range")?;
        let guild_id_i64 = context.guild_id;
        let full_lobby_ids = prepared.pending.state.full_lobby_player_ids();
        let real_ids = full_lobby_ids
            .iter()
            .copied()
            .filter(|player_id| *player_id > 0)
            .filter_map(|player_id| u64::try_from(player_id).ok())
            .collect::<Vec<_>>();
        let streaming_ids = if let Some(saved) = prepared
            .pending
            .state
            .extra
            .get("shuffle_streaming_bonus_ids")
            .and_then(serde_json::Value::as_array)
        {
            saved.iter().filter_map(serde_json::Value::as_u64).collect()
        } else if self.config.streaming_bonus > 0 {
            self.discord
                .streaming_member_ids(guild_id, &real_ids)
                .await
                .map_err(|e| format!("streaming eligibility lookup: {e}"))?
        } else {
            BTreeSet::new()
        };
        {
            let saved = json!(streaming_ids);
            let repo = self.pending.clone();
            let guild = context.guild_id;
            let id = prepared.pending.pending_match_id;
            prepared.pending = tokio::task::spawn_blocking(move || {
                repo.mutate_pending_match(guild, id, move |s| {
                    s.extra
                        .entry("shuffle_streaming_bonus_ids".to_owned())
                        .or_insert(saved);
                })
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?
            .ok_or("shuffle aborted")?
            .0;
        }
        let streaming_ids: BTreeSet<u64> = serde_json::from_value(
            prepared.pending.state.extra["shuffle_streaming_bonus_ids"].clone(),
        )
        .map_err(|e| format!("saved shuffle streaming eligibility: {e}"))?;
        if !streaming_ids.is_empty() {
            let player_ids = streaming_ids
                .iter()
                .filter_map(|player_id| i64::try_from(*player_id).ok())
                .collect::<Vec<_>>();
            // These are SQLite money writes, so they belong on a blocking
            // thread like the equivalent award at match record time.
            let rewards = Arc::clone(&self.rewards);
            let gross = self.config.streaming_bonus;
            let pending_match_id = prepared.pending.pending_match_id;
            let awarded = tokio::task::spawn_blocking(move || {
                rewards.award_generated_batch(GeneratedRewardBatch {
                    guild_id: guild_id_i64,
                    player_ids: &player_ids,
                    gross,
                    apply_bankruptcy_penalty: true,
                    apply_vanity_tax: true,
                    low_priority_taxable_ids: None,
                    source: "shuffle_streaming_bonus",
                    related_type: "pending_match",
                    related_id: pending_match_id,
                    reason: "shuffle streaming bonus",
                    event_nonce_tag: 5,
                })
            })
            .await
            .map_err(|error| format!("shuffle streaming bonus task failed: {error}"))?;
            awarded.map_err(|e| format!("shuffle streaming award: {e}"))?;
            let persisted_streamers = streaming_ids
                .iter()
                .filter_map(|player_id| i64::try_from(*player_id).ok())
                .map(serde_json::Value::from)
                .collect::<Vec<_>>();
            let pending_repository = self.pending.clone();
            let pending_match_id = prepared.pending.pending_match_id;
            prepared.pending = tokio::task::spawn_blocking(move || {
                pending_repository.mutate_pending_match(
                    guild_id_i64,
                    pending_match_id,
                    move |state| {
                        state.extra.insert(
                            "shuffle_streaming_bonus_ids".to_owned(),
                            serde_json::Value::Array(persisted_streamers),
                        );
                    },
                )
            })
            .await
            .map_err(|error| format!("streaming metadata task failed: {error}"))?
            .map_err(|error| error.to_string())?
            .map(|(pending, ())| pending)
            .ok_or("pending match disappeared before publication")?;
        }

        let embed = self.render_shuffle_embed(&prepared.pending).await?;
        let public_response = InteractionResponse::message("").embed(embed.clone());
        let lobby_send = async {
            let channel_id = snapshot.lobby_channel_id?;
            if let Some(message_id) = prepared.pending.state.shuffle_message_id {
                return Some(DiscordMessageReceipt {
                    channel_id,
                    message_id: message_id as u64,
                    jump_url: prepared
                        .pending
                        .state
                        .shuffle_message_jump_url
                        .clone()
                        .unwrap_or_default(),
                });
            }
            match self
                .discord
                .send_message_with_delivery_key(
                    channel_id,
                    &shuffle_nonce(context.guild_id, prepared.pending.pending_match_id, "lobby"),
                    DiscordMessage::silent(public_response.clone()),
                )
                .await
            {
                Ok(receipt) => Some(receipt),
                Err(error) => {
                    warn!(%error, channel_id, "shuffle lobby-channel publication failed");
                    None
                }
            }
        };
        let command_send = async {
            let channel_id = context.channel_id?;
            if Some(channel_id) == snapshot.lobby_channel_id {
                return None;
            }
            if let Some(message_id) = prepared.pending.state.cmd_shuffle_message_id {
                return Some(DiscordMessageReceipt {
                    channel_id,
                    message_id: message_id as u64,
                    jump_url: String::new(),
                });
            }
            match self
                .discord
                .send_message_with_delivery_key(
                    channel_id,
                    &shuffle_nonce(
                        context.guild_id,
                        prepared.pending.pending_match_id,
                        "command",
                    ),
                    DiscordMessage::silent(public_response.clone()),
                )
                .await
            {
                Ok(receipt) => Some(receipt),
                Err(error) => {
                    warn!(%error, channel_id, "shuffle command-channel publication failed");
                    None
                }
            }
        };
        let (lobby_receipt, command_receipt) = tokio::join!(lobby_send, command_send);
        let all_sent = (snapshot.lobby_channel_id.is_none() || lobby_receipt.is_some())
            && (context.channel_id.is_none()
                || context.channel_id == snapshot.lobby_channel_id
                || command_receipt.is_some());

        let origin_channel_id = snapshot.origin_channel_id;
        let pending_repository = self.pending.clone();
        let pending_match_id = prepared.pending.pending_match_id;
        let lobby_receipt_for_state = lobby_receipt.clone();
        let command_receipt_for_state = command_receipt.clone();
        prepared.pending = tokio::task::spawn_blocking(move || {
            pending_repository.mutate_pending_match(guild_id_i64, pending_match_id, move |state| {
                if let Some(receipt) = lobby_receipt_for_state {
                    state.shuffle_channel_id = i64::try_from(receipt.channel_id).ok();
                    state.shuffle_message_id = i64::try_from(receipt.message_id).ok();
                    state.shuffle_message_jump_url = Some(receipt.jump_url);
                }
                if let Some(receipt) = command_receipt_for_state {
                    state.cmd_shuffle_channel_id = i64::try_from(receipt.channel_id).ok();
                    state.cmd_shuffle_message_id = i64::try_from(receipt.message_id).ok();
                }
                state.origin_channel_id = origin_channel_id.and_then(|id| i64::try_from(id).ok());
            })
        })
        .await
        .map_err(|error| format!("publication metadata task failed: {error}"))?
        .map_err(|error| error.to_string())?
        .map(|(pending, ())| pending)
        .ok_or("pending match disappeared during publication")?;

        if !all_sent {
            return Err("Shuffle saved; Discord publication will retry automatically.".into());
        }
        let thread_publish = self.publish_shuffle_thread(&snapshot, &prepared.pending, embed);
        let unpin = self.lobbies.unpin_source_message(&snapshot);
        let (thread_result, unpin_result) = tokio::join!(thread_publish, unpin);
        thread_result.map_err(|e| format!("shuffle thread publication will retry: {e}"))?;
        if let Err(error) = unpin_result {
            warn!(%error, "shuffle source unpin failed");
        }
        // Never reset a newer lobby generation during recovery.
        if self
            .lobbies
            .snapshot(context.guild_id, snapshot.lobby_kind)
            .is_some_and(|current| current.lobby_message_id == snapshot.lobby_message_id)
        {
            self.lobbies
                .reset_after_shuffle(context.guild_id, snapshot.lobby_kind)
                .await?;
        }
        let repo = self.pending.clone();
        let guild = context.guild_id;
        let id = prepared.pending.pending_match_id;
        let ready = tokio::task::spawn_blocking(move || {
            repo.mutate_pending_match(guild, id, |state| {
                state
                    .extra
                    .insert("shuffle_setup_complete".into(), json!(true));
                if let Some(journal) = state.extra.get_mut("_cama_shuffle_setup") {
                    journal["phase"] = json!("ready");
                }
            })
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?
        .ok_or("shuffle aborted during publication")?
        .0;
        self.schedule_betting_reminders(&ready, true);
        self.notify_match_started(&ready);
        // Interaction expiry is not a failed shuffle. All receipts and ready
        // state are durable before attempting this disposable confirmation.
        if let Err(error) = responder
            .followup(
                InteractionResponse::message(format!(
                    "✅ {} teams shuffled!",
                    snapshot.lobby_kind.label()
                ))
                .ephemeral(),
            )
            .await
        {
            debug!(%error,"shuffle confirmation expired after durable publication");
        }
        Ok(())
    }
    pub(super) async fn publish_shuffle_thread(
        &self,
        snapshot: &MatchLobbySnapshot,
        pending: &PendingMatchRecord,
        embed: InteractionEmbed,
    ) -> Result<(), String> {
        let Some(thread_id) = snapshot.thread_id else {
            return Ok(());
        };
        let name = format!(
            "🔒 {} Shuffled - Awaiting Results",
            snapshot.lobby_kind.label()
        );
        let rename = self.discord.edit_thread(thread_id, &name, false, false);
        let publish = async {
            if pending.state.thread_shuffle_message_id.is_none() {
                let receipt = self
                    .discord
                    .send_message_with_delivery_key(
                        thread_id,
                        &shuffle_nonce(pending.guild_id, pending.pending_match_id, "thread"),
                        DiscordMessage::silent(InteractionResponse::message("").embed(embed)),
                    )
                    .await?;
                let repository = self.pending.clone();
                let guild_id = pending.guild_id;
                let pending_match_id = pending.pending_match_id;
                tokio::task::spawn_blocking(move || {
                    repository.mutate_pending_match(guild_id, pending_match_id, move |state| {
                        state.thread_shuffle_thread_id = i64::try_from(thread_id).ok();
                        state.thread_shuffle_message_id = i64::try_from(receipt.message_id).ok();
                    })
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?
                .ok_or("pending match disappeared during thread publication")?;
            }
            let real_player_ids = pending
                .state
                .participant_ids()
                .into_iter()
                .filter(|id| *id > 0)
                .filter_map(|id| u64::try_from(id).ok())
                .collect::<BTreeSet<_>>();
            if !real_player_ids.is_empty()
                && pending.state.extra.get("shuffle_thread_players_published") != Some(&json!(true))
            {
                let mentions = real_player_ids
                    .iter()
                    .map(|id| format!("<@{id}>"))
                    .collect::<Vec<_>>()
                    .join(" ");
                self.discord
                    .send_message_with_delivery_key(
                        thread_id,
                        &shuffle_nonce(pending.guild_id, pending.pending_match_id, "players"),
                        DiscordMessage::mentioning(
                            InteractionResponse::message(format!(
                                "{mentions}\nPlayers, take your starting positions"
                            )),
                            real_player_ids,
                        ),
                    )
                    .await?;
                let repository = self.pending.clone();
                let guild_id = pending.guild_id;
                let pending_match_id = pending.pending_match_id;
                tokio::task::spawn_blocking(move || {
                    repository.mutate_pending_match(guild_id, pending_match_id, |state| {
                        state
                            .extra
                            .insert("shuffle_thread_players_published".into(), json!(true));
                    })
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?
                .ok_or("pending match disappeared during player publication")?;
            }
            Ok::<_, String>(())
        };
        let (rename_result, publish_result) = tokio::join!(rename, publish);
        if let Err(error) = rename_result {
            debug!(%error, thread_id, "shuffle thread rename failed");
        }
        publish_result?;
        self.discord
            .edit_thread(thread_id, &name, false, true)
            .await?;
        Ok(())
    }
}

fn shuffle_nonce(guild: i64, pending: i64, kind: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("shuffle:{guild}:{pending}:{kind}"));
    format!(
        "q{}",
        digest[..12]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

impl MatchHandler {
    pub(super) async fn recover_shuffle_setup(
        &self,
        pending: &PendingMatchRecord,
    ) -> Result<bool, String> {
        if pending.state.extra.get("shuffle_setup_complete") != Some(&json!(false)) {
            return Ok(false);
        }
        let kind = parse_persisted_lobby_kind(pending.state.lobby_kind.as_deref());
        let operation = self.lobbies.operation_lock(pending.guild_id, kind);
        let Ok(_operation) = operation.try_lock() else {
            return Ok(true);
        };
        let journal = pending
            .state
            .extra
            .get("_cama_shuffle_setup")
            .ok_or("incomplete shuffle has no recovery journal")?;
        match journal.get("phase").and_then(serde_json::Value::as_str) {
            Some("preparing") => {
                if journal
                    .get("lease_until")
                    .and_then(serde_json::Value::as_i64)
                    .is_some_and(|until| unix_seconds() < until)
                {
                    return Ok(true);
                }
                let bets = self.bets.clone();
                let guild = pending.guild_id;
                let id = pending.pending_match_id;
                tokio::task::spawn_blocking(move || {
                    bets.abort_expired_shuffle_setup_atomic(Some(guild), id, unix_seconds())
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
                Ok(true)
            }
            Some("prepared") => {
                let source = |key| journal.get(key).and_then(serde_json::Value::as_u64);
                let snapshot = MatchLobbySnapshot {
                    guild_id: pending.guild_id,
                    lobby_kind: kind,
                    created_by: None,
                    player_ids: pending.state.full_lobby_player_ids().into_iter().collect(),
                    player_join_times: BTreeMap::new(),
                    confirmed_player_ids: None,
                    ready_threshold: 10,
                    lobby_channel_id: source("source_channel"),
                    lobby_message_id: source("source_message"),
                    origin_channel_id: source("origin_channel"),
                    thread_id: source("source_thread"),
                };
                let channel = source("command_channel").or(snapshot.lobby_channel_id);
                if channel.is_none() {
                    return Err(format!(
                        "Match #{} needs a publication channel or an abort",
                        pending.pending_match_id
                    ));
                }
                self.finalize_shuffle(
                    MatchCommandContext {
                        user_id: 0,
                        guild_id: pending.guild_id,
                        channel_id: channel,
                        member_permissions: None,
                        options: Vec::new(),
                    },
                    Arc::new(RecoveryResponder),
                    snapshot,
                    PreparedShuffle {
                        pending: pending.clone(),
                    },
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(true)
            }
            _ => Err("invalid unfinished shuffle stage".into()),
        }
    }
}

struct RecoveryResponder;
#[async_trait]
impl InteractionResponder for RecoveryResponder {
    async fn respond(
        &self,
        _: InteractionResponse,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        Ok(())
    }
    async fn defer(&self, _: bool) -> Result<(), crate::registration::InteractionResponseError> {
        Ok(())
    }
    async fn followup(
        &self,
        _: InteractionResponse,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        Ok(())
    }
    fn response_is_done(&self) -> bool {
        true
    }
}

struct ShuffleRecoveryWorker {
    handler: Arc<MatchHandler>,
}
#[async_trait]
impl crate::BackgroundWorker for ShuffleRecoveryWorker {
    async fn run(&self, mut context: crate::WorkerContext) -> Result<(), String> {
        loop {
            let repo = self.handler.pending.clone();
            let rows = tokio::task::spawn_blocking(move || repo.unfinished_shuffle_setups())
                .await
                .map_err(|e| e.to_string())?;
            match rows {
                Ok(rows) => {
                    for pending in rows {
                        if let Err(error) = self.handler.recover_pending_match(pending).await {
                            warn!(%error,"shuffle recovery will retry");
                        }
                    }
                }
                Err(error) => warn!(%error,"shuffle recovery read will retry"),
            }
            if !context.sleep(Duration::from_secs(30)).await {
                return Ok(());
            }
        }
    }
}
impl MatchRegistrationProvider {
    pub fn setup_recovery_worker(&self) -> crate::BackgroundWorkerSpec {
        crate::BackgroundWorkerSpec::new(
            "match-setup-recovery",
            Arc::new(ShuffleRecoveryWorker {
                handler: self.handler.clone(),
            }),
        )
    }
}
