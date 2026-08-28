-- Generated from the Python SchemaManager on 2026-08-11.
-- Runtime use is entirely Rust/SQLite; keep this canonical DDL in source control.

-- table: app_kv
CREATE TABLE app_kv (
                guild_id INTEGER NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (guild_id, key)
            );

-- table: autobet_investments
CREATE TABLE autobet_investments (
                guild_id    INTEGER NOT NULL DEFAULT 0,
                investor_id INTEGER NOT NULL,
                target_id   INTEGER NOT NULL,
                direction   TEXT NOT NULL CHECK(direction IN ('long', 'short')),
                percentage  INTEGER NOT NULL CHECK(percentage BETWEEN 1 AND 10),
                created_at  TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at  TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (guild_id, investor_id, target_id)
            );

-- table: bankruptcy_state
CREATE TABLE "bankruptcy_state" (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                last_bankruptcy_at INTEGER,
                penalty_games_remaining INTEGER DEFAULT 0,
                bankruptcy_count INTEGER DEFAULT 0,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: bet_settlement_taxes
CREATE TABLE bet_settlement_taxes (
                match_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                discord_id INTEGER NOT NULL,
                vanity_tax INTEGER NOT NULL DEFAULT 0
                    CHECK (vanity_tax >= 0),
                low_priority_tax INTEGER NOT NULL DEFAULT 0
                    CHECK (low_priority_tax >= 0),
                PRIMARY KEY (match_id, guild_id, discord_id)
            );

-- table: bets
CREATE TABLE bets (
                bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                match_id INTEGER,
                discord_id INTEGER NOT NULL,
                team_bet_on TEXT NOT NULL,
                amount INTEGER NOT NULL,
                bet_time INTEGER NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, leverage INTEGER DEFAULT 1, payout INTEGER, is_blind INTEGER DEFAULT 0, odds_at_placement REAL, pending_match_id INTEGER, investment_target_id INTEGER, investment_direction TEXT,
                FOREIGN KEY (match_id) REFERENCES matches(match_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            );

-- table: dig_achievements
CREATE TABLE dig_achievements (
                discord_id       INTEGER NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                achievement_id   TEXT NOT NULL,
                unlocked_at      INTEGER NOT NULL,
                PRIMARY KEY (discord_id, guild_id, achievement_id)
            );

-- table: dig_actions
CREATE TABLE dig_actions (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                actor_id         INTEGER NOT NULL,
                target_id        INTEGER,
                action_type      TEXT NOT NULL,
                depth_before     INTEGER NOT NULL,
                depth_after      INTEGER NOT NULL,
                jc_delta         INTEGER NOT NULL DEFAULT 0,
                detail           TEXT,
                created_at       INTEGER NOT NULL
            );

-- table: dig_active_duels
CREATE TABLE dig_active_duels (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL,
                boss_id TEXT NOT NULL,
                tier INTEGER NOT NULL,
                mechanic_id TEXT NOT NULL,
                risk_tier TEXT NOT NULL,
                wager INTEGER NOT NULL,
                player_hp INTEGER NOT NULL,
                boss_hp INTEGER NOT NULL,
                round_num INTEGER NOT NULL,
                round_log TEXT NOT NULL DEFAULT '[]',
                pending_prompt TEXT,
                rng_state TEXT NOT NULL,
                status_effects TEXT NOT NULL DEFAULT '{}',
                echo_applied INTEGER NOT NULL DEFAULT 0,
                echo_killer_id INTEGER,
                player_hit REAL NOT NULL,
                player_dmg INTEGER NOT NULL,
                boss_hit REAL NOT NULL,
                boss_dmg INTEGER NOT NULL,
                crit_chance REAL NOT NULL DEFAULT 0,
                crit_bonus INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                last_interaction_at INTEGER NOT NULL,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: dig_artifact_registry
CREATE TABLE dig_artifact_registry (
                artifact_id      TEXT NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                first_finder_id  INTEGER,
                first_found_at   INTEGER,
                total_found      INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (artifact_id, guild_id)
            );

-- table: dig_artifacts
CREATE TABLE dig_artifacts (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id       INTEGER NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                artifact_id      TEXT NOT NULL,
                found_at         INTEGER NOT NULL,
                is_relic         INTEGER NOT NULL DEFAULT 0,
                equipped         INTEGER NOT NULL DEFAULT 0
            );

-- table: dig_boss_echoes
CREATE TABLE dig_boss_echoes (
                guild_id INTEGER NOT NULL,
                boss_id TEXT NOT NULL,
                depth INTEGER NOT NULL,
                killer_discord_id INTEGER NOT NULL,
                weakened_until INTEGER NOT NULL,
                PRIMARY KEY (guild_id, boss_id)
            );

-- table: dig_dm_memory
CREATE TABLE dig_dm_memory (
                discord_id   INTEGER NOT NULL,
                guild_id     INTEGER NOT NULL DEFAULT 0,
                summary_text TEXT NOT NULL DEFAULT '',
                updated_at   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: dig_gear
CREATE TABLE dig_gear (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id    INTEGER NOT NULL,
                guild_id      INTEGER NOT NULL DEFAULT 0,
                slot          TEXT    NOT NULL,
                tier          INTEGER NOT NULL,
                durability    INTEGER NOT NULL,
                equipped      INTEGER NOT NULL DEFAULT 0,
                acquired_at   INTEGER NOT NULL,
                source        TEXT    NOT NULL DEFAULT 'shop'
            , item_id TEXT);

-- table: dig_guild_modifiers
CREATE TABLE dig_guild_modifiers (
                guild_id     INTEGER NOT NULL,
                modifier_id  TEXT    NOT NULL,
                expires_at   INTEGER NOT NULL,
                payload_json TEXT    NOT NULL DEFAULT '{}',
                created_at   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (guild_id, modifier_id)
            );

-- table: dig_inventory
CREATE TABLE dig_inventory (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id       INTEGER NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                item_type        TEXT NOT NULL,
                queued           INTEGER NOT NULL DEFAULT 0,
                created_at       INTEGER NOT NULL
            );

-- table: dig_personality
CREATE TABLE dig_personality (
                discord_id       INTEGER NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                summary          TEXT DEFAULT '',
                choice_histogram TEXT DEFAULT '{}',
                notable_moments  TEXT DEFAULT '[]',
                play_style       TEXT DEFAULT 'unknown',
                social_summary   TEXT DEFAULT '',
                updated_at       INTEGER DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: dig_quests
CREATE TABLE dig_quests (
                discord_id        INTEGER NOT NULL,
                guild_id          INTEGER NOT NULL,
                active_quest_id   TEXT,
                active_quest_step INTEGER,
                completed_quests  TEXT NOT NULL DEFAULT '[]',
                last_updated_at   INTEGER,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: dig_weather
CREATE TABLE dig_weather (
                guild_id    INTEGER NOT NULL,
                game_date   TEXT NOT NULL,
                layer_name  TEXT NOT NULL,
                weather_id  TEXT NOT NULL,
                PRIMARY KEY (guild_id, game_date, layer_name)
            );

-- table: disburse_history
CREATE TABLE disburse_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                disbursed_at INTEGER NOT NULL,
                total_amount INTEGER NOT NULL,
                method TEXT NOT NULL,
                recipient_count INTEGER NOT NULL,
                recipients TEXT NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );

-- table: disburse_proposals
CREATE TABLE disburse_proposals (
                guild_id INTEGER PRIMARY KEY,
                proposal_id INTEGER NOT NULL,
                message_id INTEGER,
                channel_id INTEGER,
                fund_amount INTEGER NOT NULL,
                quorum_required INTEGER NOT NULL,
                status TEXT DEFAULT 'active',
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );

-- table: disburse_vote_history
CREATE TABLE disburse_vote_history (
                guild_id INTEGER NOT NULL DEFAULT 0,
                proposal_id INTEGER NOT NULL,
                discord_id INTEGER NOT NULL,
                vote_method TEXT NOT NULL,
                voted_at INTEGER NOT NULL,
                proposal_outcome TEXT,
                finalized_at INTEGER,
                PRIMARY KEY (guild_id, proposal_id, discord_id)
            );

-- table: disburse_votes
CREATE TABLE disburse_votes (
                guild_id INTEGER NOT NULL,
                proposal_id INTEGER NOT NULL,
                discord_id INTEGER NOT NULL,
                vote_method TEXT NOT NULL,
                voted_at INTEGER NOT NULL,
                PRIMARY KEY (guild_id, proposal_id, discord_id)
            );

-- table: double_or_nothing_spins
CREATE TABLE double_or_nothing_spins (
                spin_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                discord_id INTEGER NOT NULL,
                cost INTEGER NOT NULL,
                balance_before INTEGER NOT NULL,
                balance_after INTEGER NOT NULL,
                won INTEGER NOT NULL,
                spin_time INTEGER NOT NULL,
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            );

-- table: duel_challenges
CREATE TABLE duel_challenges (
                challenge_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                channel_id INTEGER NOT NULL,
                message_id INTEGER,
                challenger_id INTEGER NOT NULL,
                recipient_id INTEGER NOT NULL,
                wager INTEGER NOT NULL CHECK (wager BETWEEN 500 AND 1000),
                issuance_fee INTEGER NOT NULL DEFAULT 50 CHECK (issuance_fee = 50),
                status TEXT NOT NULL CHECK (status IN (
                    'pending', 'accepted', 'declined', 'expired',
                    'resolved', 'voided', 'delivery_failed'
                )),
                trial_type TEXT CHECK (
                    trial_type IS NULL OR trial_type IN ('trial_by_combat', 'trial_of_five')
                ),
                challenger_glicko REAL NOT NULL,
                challenger_rd REAL NOT NULL,
                recipient_glicko REAL NOT NULL,
                recipient_rd REAL NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                next_reminder_at INTEGER,
                responded_at INTEGER,
                resolved_at INTEGER,
                winner_id INTEGER,
                resolution_actor_id INTEGER,
                CHECK (challenger_id != recipient_id)
            );

-- table: draft_financial_effects
CREATE TABLE draft_financial_effects (
                effect_key TEXT PRIMARY KEY NOT NULL,
                completion_key TEXT NOT NULL,
                guild_id INTEGER NOT NULL,
                session_id INTEGER NOT NULL CHECK(session_id > 0),
                pending_match_id INTEGER NOT NULL,
                effect_kind TEXT NOT NULL
                    CHECK(effect_kind IN ('seed', 'blind', 'investment', 'spectator')),
                ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
                plan_sha256 TEXT NOT NULL
                    CHECK(
                        length(plan_sha256) = 64
                        AND plan_sha256 NOT GLOB '*[^0-9a-f]*'
                    ),
                intended_json TEXT NOT NULL
                    CHECK(json_valid(intended_json) AND json_type(intended_json) = 'object'),
                status TEXT NOT NULL CHECK(status IN ('applied', 'skipped')),
                receipt_json TEXT NOT NULL
                    CHECK(json_valid(receipt_json) AND json_type(receipt_json) = 'object'),
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                UNIQUE(completion_key, effect_kind, ordinal),
                FOREIGN KEY(completion_key)
                    REFERENCES draft_finalization_jobs(completion_key),
                FOREIGN KEY(pending_match_id)
                    REFERENCES pending_matches(pending_match_id)
            );

-- table: draft_finalization_jobs
CREATE TABLE draft_finalization_jobs (
                completion_key TEXT PRIMARY KEY NOT NULL,
                guild_id INTEGER NOT NULL,
                session_id INTEGER NOT NULL CHECK(session_id > 0),
                pending_match_id INTEGER NOT NULL UNIQUE,
                revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
                stage TEXT NOT NULL CHECK(length(stage) > 0),
                plan_json TEXT NOT NULL
                    CHECK(json_valid(plan_json) AND json_type(plan_json) = 'object'),
                progress_json TEXT NOT NULL DEFAULT '{}'
                    CHECK(json_valid(progress_json) AND json_type(progress_json) = 'object'),
                lease_owner TEXT,
                lease_until INTEGER,
                last_error TEXT,
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                CHECK(
                    (lease_owner IS NULL AND lease_until IS NULL)
                    OR (lease_owner IS NOT NULL AND lease_until IS NOT NULL)
                ),
                UNIQUE(guild_id, session_id)
            );

-- table: economy_daily_events
CREATE TABLE economy_daily_events (
                event_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                event_date TEXT NOT NULL,
                name TEXT NOT NULL,
                hero TEXT NOT NULL,
                direction TEXT NOT NULL
                    CHECK(direction IN ('deflationary', 'neutral', 'boon')),
                severity INTEGER NOT NULL CHECK(severity BETWEEN 1 AND 5),
                target_effect_jc INTEGER NOT NULL,
                forecast_flow_jc INTEGER NOT NULL,
                expected_effect_jc INTEGER NOT NULL,
                direct_effect_jc INTEGER NOT NULL DEFAULT 0,
                actual_stock_change_jc INTEGER,
                monetary_stock_before INTEGER NOT NULL,
                effects TEXT NOT NULL,
                announcement TEXT NOT NULL,
                starts_at INTEGER NOT NULL,
                ends_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                reminder_announced_at INTEGER, announced_at INTEGER,
                UNIQUE(guild_id, event_date)
            );

-- table: economy_daily_snapshots
CREATE TABLE economy_daily_snapshots (
                guild_id INTEGER NOT NULL,
                snapshot_date TEXT NOT NULL,
                captured_at INTEGER NOT NULL,
                player_wallets INTEGER NOT NULL,
                positive_wallets INTEGER NOT NULL,
                visible_debt INTEGER NOT NULL,
                player_count INTEGER NOT NULL,
                average_wallet REAL NOT NULL,
                reserve_available INTEGER NOT NULL,
                reserve_locked INTEGER NOT NULL,
                reserve_next_match_pot INTEGER NOT NULL,
                prediction_open_cash INTEGER NOT NULL,
                wager_escrow INTEGER NOT NULL,
                monetary_stock INTEGER NOT NULL,
                annualized_30d REAL,
                annualized_90d REAL,
                PRIMARY KEY (guild_id, snapshot_date)
            );

-- table: economy_ledger_context
CREATE TABLE economy_ledger_context (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                source TEXT,
                actor_id INTEGER,
                related_type TEXT,
                related_id TEXT,
                reason TEXT,
                metadata TEXT
            );

-- table: economy_ledger_entries
CREATE TABLE economy_ledger_entries (
                ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                account_type TEXT NOT NULL CHECK(account_type IN ('player', 'nonprofit')),
                account_id INTEGER,
                delta INTEGER NOT NULL,
                balance_before INTEGER NOT NULL,
                balance_after INTEGER NOT NULL,
                source TEXT NOT NULL DEFAULT 'balance_update',
                actor_id INTEGER,
                related_type TEXT,
                related_id TEXT,
                reason TEXT,
                metadata TEXT,
                created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
            );

-- table: economy_policy_state
CREATE TABLE economy_policy_state (
                guild_id INTEGER PRIMARY KEY,
                mode TEXT NOT NULL DEFAULT 'normal'
                    CHECK(mode IN ('recovery', 'normal', 'disabled')),
                target_annual_rate REAL NOT NULL DEFAULT 0.02,
                inflation_ceiling REAL NOT NULL DEFAULT 0.02,
                recovery_started_at INTEGER,
                stable_since INTEGER,
                updated_at INTEGER NOT NULL
            );

-- table: first_game_pool_claims
CREATE TABLE first_game_pool_claims (
                guild_id INTEGER NOT NULL DEFAULT 0,
                lobby_kind TEXT NOT NULL CHECK(lobby_kind IN ('open', 'lowskill')),
                game_date TEXT NOT NULL,
                pending_match_id INTEGER NOT NULL,
                amount INTEGER NOT NULL CHECK(amount >= 0),
                settled INTEGER NOT NULL DEFAULT 0 CHECK(settled IN (0, 1)),
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (guild_id, lobby_kind, game_date),
                UNIQUE (guild_id, pending_match_id)
            );

-- table: first_game_pool_funding_days
CREATE TABLE first_game_pool_funding_days (
                guild_id INTEGER NOT NULL DEFAULT 0,
                game_date TEXT NOT NULL,
                daily_amount INTEGER NOT NULL,
                funded INTEGER NOT NULL CHECK(funded IN (0, 1)),
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (guild_id, game_date)
            );

-- table: guild_config
CREATE TABLE guild_config (
                guild_id INTEGER PRIMARY KEY,
                league_id INTEGER,
                auto_enrich_matches INTEGER DEFAULT 1,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            , ai_features_enabled INTEGER DEFAULT 0);

-- table: hostile_loss_events
CREATE TABLE hostile_loss_events (
                event_id                    INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id                   INTEGER NOT NULL DEFAULT 0,
                victim_id                  INTEGER NOT NULL,
                actor_id                   INTEGER,
                event_key                  TEXT NOT NULL,
                kind                       TEXT NOT NULL,
                destination                TEXT NOT NULL
                                               CHECK(destination IN ('burn', 'player', 'reserve')),
                recipient_id               INTEGER,
                requested                  INTEGER NOT NULL CHECK(requested >= 0),
                attempted                  INTEGER NOT NULL CHECK(attempted >= 0),
                absorbed                   INTEGER NOT NULL CHECK(absorbed >= 0),
                applied                    INTEGER NOT NULL CHECK(applied >= 0),
                victim_balance_before      INTEGER NOT NULL,
                victim_balance_after       INTEGER NOT NULL,
                destination_balance_before INTEGER,
                destination_balance_after  INTEGER,
                shieldable                 INTEGER NOT NULL DEFAULT 1,
                retro_covered              INTEGER NOT NULL DEFAULT 0,
                protection_details         TEXT NOT NULL DEFAULT '[]',
                metadata                   TEXT,
                occurred_at                INTEGER NOT NULL,
                created_at                 INTEGER NOT NULL,
                UNIQUE(guild_id, victim_id, event_key)
            );

-- table: llm_request_attempts
CREATE TABLE llm_request_attempts (
                attempt_id       INTEGER PRIMARY KEY AUTOINCREMENT,
                feature          TEXT NOT NULL CHECK(length(trim(feature)) > 0),
                operation        TEXT NOT NULL CHECK(length(trim(operation)) > 0),
                provider         TEXT NOT NULL CHECK(length(trim(provider)) > 0),
                model            TEXT NOT NULL CHECK(length(trim(model)) > 0),
                success          INTEGER NOT NULL CHECK(success IN (0, 1)),
                latency_ms       INTEGER NOT NULL CHECK(latency_ms >= 0),
                prompt_tokens    INTEGER CHECK(prompt_tokens >= 0),
                completion_tokens INTEGER CHECK(completion_tokens >= 0),
                total_tokens     INTEGER CHECK(total_tokens >= 0),
                error_type       TEXT,
                created_at       INTEGER NOT NULL
                    DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
            );

-- table: loan_state
CREATE TABLE "loan_state" (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                last_loan_at INTEGER,
                total_loans_taken INTEGER DEFAULT 0,
                total_fees_paid INTEGER DEFAULT 0,
                negative_loans_taken INTEGER DEFAULT 0,
                outstanding_principal INTEGER DEFAULT 0,
                outstanding_fee INTEGER DEFAULT 0,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: lobby_state
CREATE TABLE lobby_state (
                lobby_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                players TEXT,
                conditional_players TEXT DEFAULT '[]',
                status TEXT,
                created_by INTEGER,
                created_at TEXT,
                message_id INTEGER,
                channel_id INTEGER,
                thread_id INTEGER,
                embed_message_id INTEGER,
                origin_channel_id INTEGER,
                player_join_times TEXT DEFAULT '{}',
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (lobby_id, guild_id)
            );

-- table: lobby_suspensions
CREATE TABLE lobby_suspensions (
                guild_id                INTEGER NOT NULL DEFAULT 0,
                discord_id              INTEGER NOT NULL,
                scope                   TEXT NOT NULL
                    CHECK(scope IN ('all', 'open', 'lowskill')),
                completion              TEXT NOT NULL
                    CHECK(completion IN ('time', 'matches', 'either', 'both')),
                expires_at              INTEGER,
                matches_required        INTEGER CHECK(matches_required > 0),
                matches_remaining       INTEGER CHECK(matches_remaining >= 0),
                pending_match_watermark INTEGER NOT NULL DEFAULT 0
                    CHECK(pending_match_watermark >= 0),
                reason                  TEXT NOT NULL CHECK(length(trim(reason)) > 0),
                source                  TEXT NOT NULL
                    CHECK(source IN ('admin', 'vote', 'creator', 'system')),
                actor_id                INTEGER NOT NULL,
                revision                INTEGER NOT NULL DEFAULT 1
                    CHECK(revision > 0),
                active                  INTEGER NOT NULL DEFAULT 1
                    CHECK(active IN (0, 1)),
                created_at              INTEGER NOT NULL,
                updated_at              INTEGER NOT NULL,
                PRIMARY KEY (guild_id, discord_id),
                CHECK(
                    (completion = 'time'
                        AND expires_at IS NOT NULL
                        AND matches_required IS NULL
                        AND matches_remaining IS NULL)
                    OR
                    (completion = 'matches'
                        AND expires_at IS NULL
                        AND matches_required IS NOT NULL
                        AND matches_remaining IS NOT NULL)
                    OR
                    (completion IN ('either', 'both')
                        AND expires_at IS NOT NULL
                        AND matches_required IS NOT NULL
                        AND matches_remaining IS NOT NULL)
                ),
                CHECK(
                    matches_remaining IS NULL
                    OR matches_remaining <= matches_required
                )
            );

-- table: lobby_target_subscriptions
CREATE TABLE lobby_target_subscriptions (
                guild_id     INTEGER NOT NULL DEFAULT 0,
                target_id    INTEGER NOT NULL,
                subscriber_id INTEGER NOT NULL,
                created_at   INTEGER NOT NULL,
                PRIMARY KEY (guild_id, target_id, subscriber_id)
            );

-- table: low_priority_state
CREATE TABLE "low_priority_state" (
                discord_id            INTEGER NOT NULL,
                guild_id              INTEGER NOT NULL DEFAULT 0,
                wins_required         INTEGER NOT NULL DEFAULT 3
                    CHECK(wins_required BETWEEN 1 AND 20),
                wins_remaining        INTEGER NOT NULL DEFAULT 3
                    CHECK(wins_remaining BETWEEN 0 AND 20),
                start_pending_match_id INTEGER CHECK(start_pending_match_id >= 0),
                active                INTEGER NOT NULL DEFAULT 1
                    CHECK(active IN (0, 1)),
                reason                TEXT,
                reason_player_visible INTEGER NOT NULL DEFAULT 0
                    CHECK(reason_player_visible IN (0, 1)),
                set_by                INTEGER NOT NULL,
                removed_by            INTEGER,
                removed_reason        TEXT,
                created_at            TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at            TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id),
                CHECK(wins_remaining <= wins_required)
            );

-- table: mafia_actions
CREATE TABLE "mafia_actions" (
                action_id   INTEGER PRIMARY KEY AUTOINCREMENT,
                game_id     INTEGER NOT NULL,
                guild_id    INTEGER NOT NULL DEFAULT 0,
                actor_id    INTEGER NOT NULL,
                target_id   INTEGER,
                action_type TEXT NOT NULL,
                phase       TEXT NOT NULL,
                day_number  INTEGER NOT NULL DEFAULT 1,
                created_at  INTEGER NOT NULL,
                result      TEXT,
                UNIQUE(game_id, actor_id, action_type, phase, day_number)
            );

-- table: mafia_bounties
CREATE TABLE mafia_bounties (
                game_id        INTEGER NOT NULL,
                guild_id       INTEGER NOT NULL DEFAULT 0,
                day_number     INTEGER NOT NULL,
                target_id      INTEGER NOT NULL,
                contributor_id INTEGER NOT NULL,
                amount         INTEGER NOT NULL DEFAULT 1,
                created_at     INTEGER NOT NULL,
                PRIMARY KEY (game_id, day_number, target_id, contributor_id),
                FOREIGN KEY (game_id) REFERENCES mafia_games(game_id)
            );

-- table: mafia_games
CREATE TABLE "mafia_games" (
                game_id              INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id             INTEGER NOT NULL DEFAULT 0,
                game_date            TEXT    NOT NULL,
                phase                TEXT    NOT NULL,
                started_at           INTEGER NOT NULL,
                night_ended_at       INTEGER,
                day_ended_at         INTEGER,
                winner               TEXT,
                entry_fee            INTEGER NOT NULL DEFAULT 0,
                payout_per_winner    INTEGER NOT NULL DEFAULT 0,
                mvp_id               INTEGER,
                roster_size          INTEGER NOT NULL,
                twist_event          TEXT,
                mafia_thread_id      INTEGER,
                discussion_thread_id INTEGER,
                setup_message_id     INTEGER,
                day_number           INTEGER NOT NULL DEFAULT 1,
                phase_started_at     INTEGER,
                standings_message_id INTEGER,
                graveyard_thread_id  INTEGER,
                mafia_intro_message_id INTEGER,
                graveyard_intro_message_id INTEGER,
                resolution_message_id INTEGER,
                resolution_summary   TEXT,
                status               TEXT NOT NULL DEFAULT 'ACTIVE'
            );

-- table: mafia_meta
CREATE TABLE mafia_meta (
                guild_id                INTEGER PRIMARY KEY DEFAULT 0,
                hall_of_fame_message_id INTEGER,
                updated_at              TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );

-- table: mafia_optout
CREATE TABLE mafia_optout (
                discord_id INTEGER NOT NULL,
                guild_id   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: mafia_phase_reminders
CREATE TABLE mafia_phase_reminders (
                guild_id   INTEGER NOT NULL DEFAULT 0,
                game_id     INTEGER NOT NULL,
                day_number  INTEGER NOT NULL,
                phase       TEXT    NOT NULL,
                claimed_at  INTEGER NOT NULL,
                PRIMARY KEY (guild_id, game_id, day_number, phase),
                FOREIGN KEY (game_id) REFERENCES mafia_games(game_id) ON DELETE CASCADE
            );

-- table: mafia_players
CREATE TABLE mafia_players (
                game_id          INTEGER NOT NULL,
                discord_id       INTEGER NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                role             TEXT    NOT NULL,
                is_godfather     INTEGER NOT NULL DEFAULT 0,
                hero_name        TEXT,
                is_alive         INTEGER NOT NULL DEFAULT 1,
                eliminated_phase TEXT,
                eliminated_at    INTEGER,
                acted            INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (game_id, discord_id),
                FOREIGN KEY (game_id) REFERENCES mafia_games(game_id)
            );

-- table: mafia_signups
CREATE TABLE mafia_signups (
                guild_id   INTEGER NOT NULL DEFAULT 0,
                week_start TEXT    NOT NULL,
                discord_id INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (guild_id, week_start, discord_id)
            );

-- table: mana_protection_events
CREATE TABLE mana_protection_events (
                protection_event_id INTEGER PRIMARY KEY AUTOINCREMENT,
                hostile_loss_event_id INTEGER NOT NULL,
                guild_id              INTEGER NOT NULL DEFAULT 0,
                victim_id             INTEGER NOT NULL,
                protection_type       TEXT NOT NULL,
                pool_key              TEXT NOT NULL,
                buff_id               INTEGER,
                amount                INTEGER NOT NULL CHECK(amount >= 0),
                rate                  REAL NOT NULL,
                capacity_before       INTEGER,
                capacity_after        INTEGER,
                retroactive           INTEGER NOT NULL DEFAULT 0,
                created_at            INTEGER NOT NULL,
                details               TEXT,
                UNIQUE(hostile_loss_event_id, pool_key, retroactive),
                FOREIGN KEY(hostile_loss_event_id)
                    REFERENCES hostile_loss_events(event_id)
            );

-- table: manashop_buffs
CREATE TABLE manashop_buffs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER NOT NULL,
                guild_id   INTEGER NOT NULL DEFAULT 0,
                buff_type  TEXT NOT NULL,
                target_id  INTEGER,
                granted_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                triggered  INTEGER NOT NULL DEFAULT 0,
                data       TEXT
            );

-- table: manashop_daily_uses
CREATE TABLE manashop_daily_uses (
                discord_id INTEGER NOT NULL,
                guild_id   INTEGER NOT NULL DEFAULT 0,
                item_id    TEXT NOT NULL,
                used_date  TEXT NOT NULL, purchase_id TEXT,
                PRIMARY KEY (discord_id, guild_id, item_id, used_date)
            );

-- table: manashop_purchases
CREATE TABLE manashop_purchases (
                purchase_id TEXT PRIMARY KEY,
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                item_id TEXT NOT NULL,
                used_date TEXT NOT NULL,
                cost INTEGER NOT NULL,
                tap_mana INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'applying', 'completed', 'refunded')),
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

-- table: match_bans
CREATE TABLE match_bans (
                match_id   INTEGER NOT NULL,
                ban_index  INTEGER NOT NULL,
                team       INTEGER NOT NULL CHECK(team IN (0, 1)),
                hero_id    INTEGER NOT NULL CHECK(hero_id > 0),
                PRIMARY KEY (match_id, ban_index),
                FOREIGN KEY (match_id) REFERENCES matches(match_id)
                    ON DELETE CASCADE
            );

-- table: match_correction_claims
CREATE TABLE match_correction_claims (
                match_id INTEGER PRIMARY KEY,
                guild_id INTEGER NOT NULL,
                old_winning_team INTEGER NOT NULL,
                new_winning_team INTEGER NOT NULL,
                state TEXT NOT NULL DEFAULT 'pending'
                    CHECK (state IN ('pending', 'core_applied')),
                owner_token TEXT,
                claimed_at INTEGER NOT NULL DEFAULT 0,
                correction_id INTEGER,
                FOREIGN KEY (match_id) REFERENCES matches(match_id),
                FOREIGN KEY (correction_id) REFERENCES match_corrections(correction_id)
            );

-- table: match_corrections
CREATE TABLE match_corrections (
                correction_id INTEGER PRIMARY KEY AUTOINCREMENT,
                match_id INTEGER NOT NULL,
                old_winning_team INTEGER NOT NULL,
                new_winning_team INTEGER NOT NULL,
                corrected_by INTEGER NOT NULL,
                corrected_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (match_id) REFERENCES matches(match_id)
            );

-- table: match_participants
CREATE TABLE match_participants (
                match_id INTEGER,
                discord_id INTEGER,
                team_number INTEGER,
                won BOOLEAN,
                side TEXT, hero_id INTEGER, kills INTEGER, deaths INTEGER, assists INTEGER, last_hits INTEGER, denies INTEGER, gpm INTEGER, xpm INTEGER, hero_damage INTEGER, tower_damage INTEGER, net_worth INTEGER, hero_healing INTEGER, lane_role INTEGER, lane_efficiency INTEGER, towers_killed INTEGER, roshans_killed INTEGER, teamfight_participation REAL, obs_placed INTEGER, sen_placed INTEGER, camps_stacked INTEGER, rune_pickups INTEGER, firstblood_claimed INTEGER, stuns REAL, fantasy_points REAL, guild_id INTEGER NOT NULL DEFAULT 0, bonus_jc INTEGER, win_bonus_jc INTEGER, assigned_role TEXT, gold_at_10 INTEGER, derived_role TEXT, last_hits_at_10 INTEGER,
                FOREIGN KEY (match_id) REFERENCES matches(match_id),
                PRIMARY KEY (match_id, discord_id)
            );

-- table: match_discovery_attempts
CREATE TABLE match_discovery_attempts (
                attempt_id              INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id                INTEGER NOT NULL DEFAULT 0,
                internal_match_id       INTEGER NOT NULL,
                dry_run                 INTEGER NOT NULL CHECK(dry_run IN (0, 1)),
                status                  TEXT NOT NULL,
                selected_valve_match_id INTEGER,
                players_with_steam_id   INTEGER NOT NULL DEFAULT 0,
                candidate_count         INTEGER NOT NULL DEFAULT 0,
                diagnostics_json        TEXT NOT NULL,
                attempted_at            INTEGER NOT NULL
            );

-- table: match_predictions
CREATE TABLE match_predictions (
                match_id INTEGER PRIMARY KEY,
                radiant_rating REAL,
                dire_rating REAL,
                radiant_rd REAL,
                dire_rd REAL,
                expected_radiant_win_prob REAL,
                timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP, openskill_radiant_win_prob REAL, openskill_raw_radiant_win_prob REAL, openskill_algorithm_version INTEGER, openskill_algorithm_fingerprint TEXT,
                FOREIGN KEY (match_id) REFERENCES matches(match_id)
            );

-- table: matches
CREATE TABLE matches (
                match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                team1_players TEXT NOT NULL,
                team2_players TEXT NOT NULL,
                winning_team INTEGER,
                match_date TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                dotabuff_match_id TEXT,
                notes TEXT
            , valve_match_id INTEGER, duration_seconds INTEGER, radiant_score INTEGER, dire_score INTEGER, game_mode INTEGER, enrichment_data TEXT, enrichment_source TEXT, enrichment_confidence REAL, lobby_type TEXT DEFAULT 'shuffle', balancing_rating_system TEXT DEFAULT 'glicko', guild_id INTEGER NOT NULL DEFAULT 0, betting_mode TEXT DEFAULT 'pool', pending_match_id INTEGER, bonuses_paid INTEGER NOT NULL DEFAULT 0, win_reward_jc INTEGER, jc_changes TEXT, lobby_kind TEXT CHECK(lobby_kind IS NULL OR lobby_kind IN ('open', 'lowskill')), parsed_refresh_attempts INTEGER NOT NULL DEFAULT 0);

-- table: moderation_events
CREATE TABLE moderation_events (
                event_id                INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id                INTEGER NOT NULL DEFAULT 0,
                discord_id              INTEGER NOT NULL,
                event_type              TEXT NOT NULL CHECK(event_type IN (
                    'suspend', 'replace', 'lift', 'complete', 'kick',
                    'lowprio_assign', 'lowprio_replace', 'lowprio_clear',
                    'lowprio_complete'
                )),
                source                  TEXT NOT NULL
                    CHECK(source IN ('admin', 'vote', 'creator', 'system')),
                actor_id                INTEGER,
                reason                  TEXT,
                scope                   TEXT
                    CHECK(scope IS NULL OR scope IN ('all', 'open', 'lowskill')),
                completion              TEXT CHECK(
                    completion IS NULL
                    OR completion IN ('time', 'matches', 'either', 'both')
                ),
                expires_at              INTEGER,
                matches_required        INTEGER CHECK(matches_required > 0),
                matches_remaining       INTEGER CHECK(matches_remaining >= 0),
                pending_match_watermark INTEGER CHECK(pending_match_watermark >= 0),
                wins_required           INTEGER CHECK(wins_required >= 0),
                wins_remaining          INTEGER CHECK(wins_remaining >= 0),
                match_id                INTEGER,
                created_at              INTEGER NOT NULL
            );

-- table: neon_events
CREATE TABLE neon_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                event_type TEXT NOT NULL,
                layer INTEGER NOT NULL DEFAULT 1,
                one_time INTEGER NOT NULL DEFAULT 0,
                fired_at INTEGER NOT NULL,
                metadata TEXT
            );

-- table: nonprofit_fund
CREATE TABLE nonprofit_fund (
                guild_id INTEGER PRIMARY KEY DEFAULT 0,
                total_collected INTEGER DEFAULT 0,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            , next_match_pot INTEGER NOT NULL DEFAULT 0, first_game_open_pool INTEGER NOT NULL DEFAULT 0, first_game_lowskill_pool INTEGER NOT NULL DEFAULT 0);

-- table: openskill_rating_events
CREATE TABLE openskill_rating_events (
                event_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                discord_id INTEGER NOT NULL,
                event_type TEXT NOT NULL
                    CHECK(event_type IN ('set_mu', 'set_sigma', 'add_sigma')),
                value REAL NOT NULL,
                event_at TIMESTAMP NOT NULL,
                reason TEXT,
                actor_id INTEGER,
                os_algorithm_version INTEGER NOT NULL,
                os_algorithm_fingerprint TEXT NOT NULL,
                FOREIGN KEY (discord_id, guild_id)
                    REFERENCES players(discord_id, guild_id)
            );

-- table: openskill_rating_revisions
CREATE TABLE openskill_rating_revisions (
                guild_id INTEGER PRIMARY KEY,
                revision INTEGER NOT NULL DEFAULT 0,
                updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

-- table: openskill_replay_jobs
CREATE TABLE openskill_replay_jobs (
                guild_id INTEGER PRIMARY KEY,
                reason TEXT NOT NULL,
                requested_at TIMESTAMP NOT NULL,
                last_error TEXT
            );

-- table: package_deal_purchases
CREATE TABLE package_deal_purchases (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                buyer_discord_id INTEGER NOT NULL,
                partner_discord_id INTEGER NOT NULL,
                jc_spent INTEGER NOT NULL,
                games_committed INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );

-- table: package_deals
CREATE TABLE package_deals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                buyer_discord_id INTEGER NOT NULL,
                partner_discord_id INTEGER NOT NULL,
                games_remaining INTEGER NOT NULL DEFAULT 10,
                cost_paid INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                UNIQUE (guild_id, buyer_discord_id, partner_discord_id)
            );

-- table: pending_matches
CREATE TABLE pending_matches (
                pending_match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                payload TEXT NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                completion_key TEXT
            );

-- table: pet_accessories
CREATE TABLE pet_accessories (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL,
                accessory_id TEXT NOT NULL,
                obtained_at INTEGER NOT NULL,
                PRIMARY KEY (discord_id, guild_id, accessory_id)
            );

-- table: pet_brawls
CREATE TABLE pet_brawls (
                brawl_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                channel_id INTEGER NOT NULL,
                challenger_id INTEGER NOT NULL,
                recipient_id INTEGER NOT NULL,
                challenger_pet_id INTEGER NOT NULL,
                recipient_pet_id INTEGER,
                status TEXT NOT NULL DEFAULT 'pending' CHECK (
                    status IN ('pending', 'active', 'done',
                               'declined', 'expired', 'void')
                ),
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                resolved_at INTEGER,
                winner_id INTEGER,
                winner_pet_id INTEGER,
                loser_pet_id INTEGER,
                rounds INTEGER,
                winner_hunger_delta INTEGER NOT NULL DEFAULT 0,
                loser_hunger_delta INTEGER NOT NULL DEFAULT 0, wager INTEGER NOT NULL DEFAULT 0 CHECK (wager BETWEEN 0 AND 100), fee INTEGER NOT NULL DEFAULT 0 CHECK (fee >= 0), challenger_xp_delta INTEGER NOT NULL DEFAULT 0 CHECK (challenger_xp_delta BETWEEN 0 AND 2), recipient_xp_delta INTEGER NOT NULL DEFAULT 0 CHECK (recipient_xp_delta BETWEEN 0 AND 2), challenger_stat_gain TEXT CHECK (challenger_stat_gain IS NULL OR challenger_stat_gain IN ('str', 'int', 'dex')), recipient_stat_gain TEXT CHECK (recipient_stat_gain IS NULL OR recipient_stat_gain IN ('str', 'int', 'dex')), personality_event_key TEXT,
                CHECK (challenger_id != recipient_id),
                CHECK (winner_id IS NULL
                       OR winner_id IN (challenger_id, recipient_id))
            );

-- table: pet_evolution_daily
CREATE TABLE pet_evolution_daily (
                pet_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL,
                upbringing_day INTEGER NOT NULL
                    CHECK (upbringing_day BETWEEN 0 AND 6),
                instinct TEXT NOT NULL,
                score INTEGER NOT NULL CHECK (score BETWEEN 0 AND 3),
                first_activity TEXT NOT NULL,
                first_source_key TEXT NOT NULL,
                second_activity TEXT,
                second_source_key TEXT,
                PRIMARY KEY (pet_id, upbringing_day, instinct)
            );

-- table: pet_refund_windows
CREATE TABLE pet_refund_windows (
                guild_id INTEGER NOT NULL,
                week_key TEXT NOT NULL,
                paid_at INTEGER NOT NULL,
                total_paid INTEGER NOT NULL CHECK (total_paid >= 0),
                announcement_payload TEXT,
                announced_at INTEGER,
                PRIMARY KEY (guild_id, week_key)
            );

-- table: pet_supplies
CREATE TABLE pet_supplies (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL,
                item_id TEXT NOT NULL,
                qty INTEGER NOT NULL DEFAULT 0 CHECK (qty >= 0),
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (discord_id, guild_id, item_id)
            );

-- table: pet_stable_state
CREATE TABLE pet_stable_state (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL,
                last_voluntary_switch_at INTEGER,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: pets
CREATE TABLE pets (
                pet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL,
                name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 32),
                species TEXT NOT NULL,
                egg_tier TEXT NOT NULL DEFAULT 'standard'
                    CHECK (egg_tier IN ('standard', 'gilded')),
                adopted_at INTEGER NOT NULL,
                hatched_at INTEGER NOT NULL,
                adopt_fee INTEGER NOT NULL CHECK (adopt_fee >= 0),
                last_fed_at INTEGER NOT NULL,
                hunger_at_last_fed INTEGER NOT NULL
                    CHECK (hunger_at_last_fed BETWEEN 0 AND 100),
                times_fed INTEGER NOT NULL DEFAULT 0 CHECK (times_fed >= 0),
                feeds_today INTEGER NOT NULL DEFAULT 0 CHECK (feeds_today >= 0),
                feed_date TEXT,
                week_consumed_jc INTEGER NOT NULL DEFAULT 0
                    CHECK (week_consumed_jc >= 0),
                week_key TEXT,
                prev_week_consumed_jc INTEGER NOT NULL DEFAULT 0
                    CHECK (prev_week_consumed_jc >= 0),
                prev_week_key TEXT,
                pampered_until INTEGER,
                accessory TEXT,
                dig_work_units INTEGER NOT NULL DEFAULT 0
                    CHECK (dig_work_units >= 0),
                dig_work_at INTEGER NOT NULL DEFAULT 0
                    CHECK (dig_work_at >= 0),
                aegis_used INTEGER NOT NULL DEFAULT 0 CHECK (aegis_used IN (0, 1)),
                hatch_announced_at INTEGER,
                died_at INTEGER,
                death_cause TEXT,
                death_announced_at INTEGER, training_xp INTEGER NOT NULL DEFAULT 0 CHECK (training_xp BETWEEN 0 AND 20), training_str INTEGER NOT NULL DEFAULT 0 CHECK (training_str BETWEEN 0 AND 2), training_int INTEGER NOT NULL DEFAULT 0 CHECK (training_int BETWEEN 0 AND 2), training_dex INTEGER NOT NULL DEFAULT 0 CHECK (training_dex BETWEEN 0 AND 2), solo_training_sessions INTEGER NOT NULL DEFAULT 3 CHECK (solo_training_sessions BETWEEN 0 AND 3), solo_training_recharged_at INTEGER, evolution_started_at INTEGER, evolution_due_at INTEGER, evolved_at INTEGER, evolution_calling TEXT, evolution_primary TEXT, evolution_secondary TEXT, evolution_announced_at INTEGER,
                is_active INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
                stabled_at INTEGER,
                CHECK (hatched_at >= adopted_at),
                CHECK (died_at IS NULL OR died_at >= adopted_at),
                CHECK (death_announced_at IS NULL OR died_at IS NOT NULL),
                CHECK (is_active = 0 OR died_at IS NULL),
                CHECK ((is_active = 1 AND stabled_at IS NULL) OR is_active = 0)
            );

-- table: player_curfew_windows
CREATE TABLE player_curfew_windows (
                discord_id   INTEGER NOT NULL,
                guild_id     INTEGER NOT NULL DEFAULT 0,
                name         TEXT NOT NULL,
                start_hour   INTEGER NOT NULL,
                start_minute INTEGER NOT NULL DEFAULT 0,
                end_hour     INTEGER NOT NULL,
                end_minute   INTEGER NOT NULL DEFAULT 0,
                timezone     TEXT,
                days         INTEGER,
                created_at   TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id, name)
            );

-- table: player_mana
CREATE TABLE player_mana (
                discord_id   INTEGER NOT NULL,
                guild_id     INTEGER NOT NULL DEFAULT 0,
                current_land TEXT,
                assigned_date TEXT,
                created_at   TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at   TIMESTAMP DEFAULT CURRENT_TIMESTAMP, bankrupt_insurance_used INTEGER DEFAULT 0, bankrupt_reroll_used INTEGER DEFAULT 0, consumed_today INTEGER DEFAULT 0, white_shield_remaining INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: player_pairings
CREATE TABLE "player_pairings" (
                guild_id INTEGER NOT NULL DEFAULT 0,
                player1_id INTEGER NOT NULL,
                player2_id INTEGER NOT NULL,
                games_together INTEGER DEFAULT 0,
                wins_together INTEGER DEFAULT 0,
                games_against INTEGER DEFAULT 0,
                player1_wins_against INTEGER DEFAULT 0,
                last_match_id INTEGER,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (guild_id, player1_id, player2_id),
                CHECK (player1_id < player2_id)
            );

-- table: player_pool_bets
CREATE TABLE player_pool_bets (
                bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                match_id INTEGER,
                discord_id INTEGER NOT NULL,
                team TEXT NOT NULL,
                amount INTEGER NOT NULL,
                bet_time INTEGER NOT NULL,
                payout INTEGER,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (match_id) REFERENCES matches(match_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            );

-- table: player_stakes
CREATE TABLE player_stakes (
                stake_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                match_id INTEGER,
                discord_id INTEGER NOT NULL,
                team TEXT NOT NULL,
                is_excluded INTEGER DEFAULT 0,
                payout INTEGER,
                stake_time INTEGER NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (match_id) REFERENCES matches(match_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            );

-- table: player_steam_ids
CREATE TABLE player_steam_ids (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER NOT NULL,
                steam_id INTEGER NOT NULL,
                is_primary INTEGER DEFAULT 0,
                added_at INTEGER NOT NULL,
                FOREIGN KEY (discord_id) REFERENCES players(discord_id) ON DELETE CASCADE,
                UNIQUE (discord_id, steam_id),
                UNIQUE (steam_id)
            );

-- table: player_trivia_questions
CREATE TABLE player_trivia_questions (
                session_id INTEGER NOT NULL,
                question_number INTEGER NOT NULL CHECK(question_number > 0),
                question_key TEXT NOT NULL,
                category TEXT NOT NULL,
                spicy INTEGER NOT NULL DEFAULT 0 CHECK(spicy IN (0, 1)),
                question_text TEXT NOT NULL,
                options_json TEXT NOT NULL,
                correct_index INTEGER NOT NULL CHECK(correct_index >= 0),
                explanation TEXT,
                selected_index INTEGER CHECK(selected_index >= 0),
                is_correct INTEGER CHECK(is_correct IN (0, 1)),
                reward INTEGER NOT NULL DEFAULT 0 CHECK(reward >= 0),
                answered_at INTEGER,
                PRIMARY KEY (session_id, question_number),
                FOREIGN KEY (session_id)
                    REFERENCES player_trivia_sessions(session_id) ON DELETE CASCADE
            );

-- table: player_trivia_sessions
CREATE TABLE player_trivia_sessions (
                session_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                discord_id INTEGER NOT NULL,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                status TEXT NOT NULL DEFAULT 'active'
                    CHECK(status IN ('active', 'completed', 'timed_out', 'error', 'cancelled')),
                question_count INTEGER NOT NULL DEFAULT 0 CHECK(question_count >= 0),
                score INTEGER NOT NULL DEFAULT 0 CHECK(score >= 0),
                jc_earned INTEGER NOT NULL DEFAULT 0 CHECK(jc_earned >= 0)
            );

-- table: players
CREATE TABLE "players" (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                discord_username TEXT NOT NULL,
                dotabuff_url TEXT,
                initial_mmr INTEGER,
                current_mmr REAL,
                wins INTEGER DEFAULT 0,
                losses INTEGER DEFAULT 0,
                preferred_roles TEXT,
                main_role TEXT,
                glicko_rating REAL,
                glicko_rd REAL,
                glicko_volatility REAL,
                os_mu REAL,
                os_sigma REAL,
                steam_id INTEGER,
                jopacoin_balance INTEGER DEFAULT 3,
                exclusion_count INTEGER DEFAULT 0,
                lowest_balance_ever INTEGER,
                last_match_date TIMESTAMP,
                first_calibrated_at INTEGER,
                is_captain_eligible INTEGER DEFAULT 0,
                last_wheel_spin INTEGER,
                last_double_or_nothing INTEGER,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, personal_best_win_streak INTEGER DEFAULT 0, total_bets_placed INTEGER DEFAULT 0, first_leverage_used INTEGER DEFAULT 0, has_wheel_pardon INTEGER DEFAULT 0, last_trivia_session INTEGER, is_solo_grinder INTEGER DEFAULT 0, solo_grinder_checked_at TEXT, dota_streak_days INTEGER NOT NULL DEFAULT 0, dota_last_played_date TEXT, preferred_region TEXT, inferred_region TEXT, last_pingedash INTEGER, last_pingedkevin INTEGER, os_rating_version INTEGER NOT NULL DEFAULT 5, os_algorithm_fingerprint TEXT, timezone TEXT, dota_play_hours TEXT,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: prediction_fair_snapshots
CREATE TABLE prediction_fair_snapshots (
                snapshot_id INTEGER PRIMARY KEY AUTOINCREMENT,
                market_id   INTEGER NOT NULL,
                guild_id    INTEGER NOT NULL,
                snapshot_at INTEGER NOT NULL,
                fair_pct    INTEGER NOT NULL,
                reason      TEXT NOT NULL
            );

-- table: prediction_levels
CREATE TABLE prediction_levels (
                level_id INTEGER PRIMARY KEY AUTOINCREMENT,
                prediction_id INTEGER NOT NULL,
                side TEXT NOT NULL,
                price INTEGER NOT NULL,
                remaining_size INTEGER NOT NULL,
                posted_at INTEGER NOT NULL,
                FOREIGN KEY (prediction_id) REFERENCES predictions(prediction_id)
            );

-- table: prediction_positions
CREATE TABLE prediction_positions (
                prediction_id INTEGER NOT NULL,
                discord_id INTEGER NOT NULL,
                yes_contracts INTEGER NOT NULL DEFAULT 0,
                yes_cost_basis_total INTEGER NOT NULL DEFAULT 0,
                no_contracts INTEGER NOT NULL DEFAULT 0,
                no_cost_basis_total INTEGER NOT NULL DEFAULT 0, bankruptcy_penalty INTEGER NOT NULL DEFAULT 0, vanity_tax INTEGER NOT NULL DEFAULT 0, low_priority_tax INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (prediction_id, discord_id),
                FOREIGN KEY (prediction_id) REFERENCES predictions(prediction_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            );

-- table: prediction_trades
CREATE TABLE prediction_trades (
                trade_id INTEGER PRIMARY KEY AUTOINCREMENT,
                prediction_id INTEGER NOT NULL,
                discord_id INTEGER NOT NULL,
                action TEXT NOT NULL,
                contracts INTEGER NOT NULL,
                jopacoins INTEGER NOT NULL,
                vwap_x100 INTEGER NOT NULL,
                last_fill_price INTEGER,
                trade_time INTEGER NOT NULL,
                FOREIGN KEY (prediction_id) REFERENCES predictions(prediction_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            );

-- table: predictions
CREATE TABLE predictions (
                prediction_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                creator_id INTEGER NOT NULL,
                question TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'open',
                outcome TEXT,
                channel_id INTEGER,
                thread_id INTEGER,
                embed_message_id INTEGER,
                resolution_votes TEXT,
                created_at INTEGER NOT NULL,
                closes_at INTEGER NOT NULL,
                resolved_at INTEGER,
                resolved_by INTEGER, channel_message_id INTEGER, close_message_id INTEGER, current_price INTEGER, initial_fair INTEGER, last_refresh_at INTEGER, lp_pnl INTEGER DEFAULT 0, prev_price INTEGER,
                FOREIGN KEY (creator_id) REFERENCES players(discord_id)
            );

-- table: rating_history
CREATE TABLE rating_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER,
                rating REAL,
                rating_before REAL,
                rd_before REAL,
                rd_after REAL,
                volatility_before REAL,
                volatility_after REAL,
                expected_team_win_prob REAL,
                team_number INTEGER,
                won BOOLEAN,
                match_id INTEGER,
                timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP, os_mu_before REAL, os_mu_after REAL, os_sigma_before REAL, os_sigma_after REAL, fantasy_weight REAL, streak_length INTEGER, streak_multiplier REAL, guild_id INTEGER NOT NULL DEFAULT 0, os_algorithm_version INTEGER, os_algorithm_fingerprint TEXT, streak_multiplier_per_game REAL NOT NULL DEFAULT 0.20, streak_threshold INTEGER NOT NULL DEFAULT 3, low_priority_gain_multiplier REAL NOT NULL DEFAULT 1.0, base_rating_delta_multiplier REAL NOT NULL DEFAULT 0.75,
                FOREIGN KEY (discord_id) REFERENCES players(discord_id),
                FOREIGN KEY (match_id) REFERENCES matches(match_id)
            );

-- table: recalibration_state
CREATE TABLE "recalibration_state" (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                last_recalibration_at INTEGER,
                total_recalibrations INTEGER DEFAULT 0,
                rating_at_recalibration REAL,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: referrals
CREATE TABLE referrals (
                guild_id          INTEGER NOT NULL DEFAULT 0,
                referred_id       INTEGER NOT NULL,
                referrer_id       INTEGER NOT NULL,
                created_at        TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                rewarded_match_id INTEGER,
                reward_amount     INTEGER,
                rewarded_at       TIMESTAMP,
                PRIMARY KEY (guild_id, referred_id),
                CHECK (referrer_id != referred_id)
            );

-- table: push_notification_targets
CREATE TABLE push_notification_targets (
                discord_id               INTEGER NOT NULL,
                guild_id                 INTEGER NOT NULL DEFAULT 0,
                ntfy_topic               TEXT,
                readycheck_enabled       INTEGER NOT NULL DEFAULT 1,
                match_started_enabled    INTEGER NOT NULL DEFAULT 1,
                dm_readycheck_enabled    INTEGER NOT NULL DEFAULT 0,
                dm_match_started_enabled INTEGER NOT NULL DEFAULT 0,
                updated_at               INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id),
                UNIQUE (ntfy_topic),
                CHECK (ntfy_topic IS NULL OR length(ntfy_topic) = 53),
                CHECK (ntfy_topic IS NULL OR substr(ntfy_topic, 1, 5) = 'cama-'),
                CHECK (ntfy_topic IS NULL OR substr(ntfy_topic, 6) NOT GLOB '*[^0-9a-f]*'),
                CHECK (readycheck_enabled IN (0, 1)),
                CHECK (match_started_enabled IN (0, 1)),
                CHECK (dm_readycheck_enabled IN (0, 1)),
                CHECK (dm_match_started_enabled IN (0, 1))
            );

-- table: reminder_preferences
CREATE TABLE reminder_preferences (
                discord_id      INTEGER NOT NULL,
                guild_id        INTEGER NOT NULL DEFAULT 0,
                wheel_enabled   INTEGER NOT NULL DEFAULT 0,
                trivia_enabled  INTEGER NOT NULL DEFAULT 0,
                betting_enabled INTEGER NOT NULL DEFAULT 0,
                updated_at      INTEGER NOT NULL DEFAULT 0, dig_enabled INTEGER NOT NULL DEFAULT 0, lobby_enabled INTEGER NOT NULL DEFAULT 0, pet_enabled INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: schema_migrations
CREATE TABLE schema_migrations (
                name TEXT PRIMARY KEY,
                applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );

-- table: slow_drip_claims
CREATE TABLE slow_drip_claims (
                discord_id     INTEGER NOT NULL,
                guild_id       INTEGER NOT NULL DEFAULT 0,
                claim_date     TEXT NOT NULL,
                claimed_today  INTEGER NOT NULL DEFAULT 0,
                last_claim_at  INTEGER NOT NULL,
                PRIMARY KEY (discord_id, guild_id, claim_date)
            );

-- table: soft_avoids
CREATE TABLE soft_avoids (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                avoider_discord_id INTEGER NOT NULL,
                avoided_discord_id INTEGER NOT NULL,
                games_remaining INTEGER NOT NULL DEFAULT 10,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                UNIQUE (guild_id, avoider_discord_id, avoided_discord_id)
            );

-- table: spectator_bets
CREATE TABLE spectator_bets (
                bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                match_id INTEGER,
                discord_id INTEGER NOT NULL,
                team TEXT NOT NULL,
                amount INTEGER NOT NULL,
                bet_time INTEGER NOT NULL,
                payout INTEGER,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (match_id) REFERENCES matches(match_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            );

-- table: survey_answers
CREATE TABLE survey_answers (
                answer_id     INTEGER PRIMARY KEY AUTOINCREMENT,
                survey_id     INTEGER NOT NULL,
                guild_id      INTEGER NOT NULL DEFAULT 0,
                recipient_id  INTEGER NOT NULL,
                question_id   INTEGER NOT NULL,
                numeric_value INTEGER,
                text_value    TEXT CHECK(text_value IS NULL OR length(text_value) <= 1000),
                skipped       INTEGER NOT NULL DEFAULT 0 CHECK(skipped IN (0, 1)),
                created_at    INTEGER NOT NULL,
                updated_at    INTEGER NOT NULL,
                UNIQUE(recipient_id, question_id),
                CHECK(
                    (skipped = 1 AND numeric_value IS NULL AND text_value IS NULL)
                    OR
                    (skipped = 0 AND ((numeric_value IS NULL) != (text_value IS NULL)))
                ),
                FOREIGN KEY(survey_id) REFERENCES surveys(survey_id) ON DELETE CASCADE,
                FOREIGN KEY(recipient_id)
                    REFERENCES survey_recipients(recipient_id) ON DELETE CASCADE,
                FOREIGN KEY(question_id)
                    REFERENCES survey_questions(question_id) ON DELETE CASCADE
            );

-- table: survey_questions
CREATE TABLE survey_questions (
                question_id   INTEGER PRIMARY KEY AUTOINCREMENT,
                survey_id     INTEGER NOT NULL,
                guild_id      INTEGER NOT NULL DEFAULT 0,
                position      INTEGER NOT NULL CHECK(position BETWEEN 1 AND 10),
                prompt        TEXT NOT NULL
                    CHECK(length(trim(prompt)) BETWEEN 1 AND 1000),
                question_type TEXT NOT NULL
                    CHECK(question_type IN ('nps', 'rating', 'text')),
                required      INTEGER NOT NULL DEFAULT 1
                    CHECK(required IN (0, 1)),
                created_at    INTEGER NOT NULL,
                UNIQUE(guild_id, survey_id, position),
                FOREIGN KEY(survey_id) REFERENCES surveys(survey_id) ON DELETE CASCADE
            );

-- table: survey_recipients
CREATE TABLE survey_recipients (
                recipient_id       INTEGER PRIMARY KEY AUTOINCREMENT,
                survey_id          INTEGER NOT NULL,
                guild_id           INTEGER NOT NULL DEFAULT 0,
                discord_id         INTEGER NOT NULL CHECK(discord_id > 0),
                delivery_status    TEXT NOT NULL DEFAULT 'pending'
                    CHECK(delivery_status IN ('pending', 'sending', 'sent', 'failed')),
                attempt_count      INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
                receipt_checked_attempt INTEGER NOT NULL DEFAULT 0
                    CHECK(receipt_checked_attempt >= 0),
                retry_requested    INTEGER NOT NULL DEFAULT 0
                    CHECK(retry_requested IN (0, 1)),
                claimed_at         INTEGER,
                dm_channel_id      INTEGER,
                dm_message_id      INTEGER,
                ui_channel_id      INTEGER,
                ui_message_id      INTEGER,
                controls_finalized INTEGER NOT NULL DEFAULT 0
                    CHECK(controls_finalized IN (0, 1)),
                current_question_id INTEGER,
                in_review          INTEGER NOT NULL DEFAULT 0 CHECK(in_review IN (0, 1)),
                started_at         INTEGER,
                submitted_at       INTEGER,
                last_error         TEXT,
                created_at         INTEGER NOT NULL,
                updated_at         INTEGER NOT NULL,
                UNIQUE(survey_id, discord_id),
                FOREIGN KEY(survey_id) REFERENCES surveys(survey_id) ON DELETE CASCADE,
                FOREIGN KEY(current_question_id) REFERENCES survey_questions(question_id)
            );

-- table: surveys
CREATE TABLE surveys (
                survey_id       INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id        INTEGER NOT NULL DEFAULT 0,
                title           TEXT NOT NULL
                    CHECK(length(trim(title)) BETWEEN 1 AND 100),
                status          TEXT NOT NULL DEFAULT 'draft'
                    CHECK(status IN ('draft', 'open', 'closed')),
                target_type     TEXT
                    CHECK(target_type IS NULL OR target_type IN (
                        'all_registered', 'member', 'role'
                    )),
                registered_only INTEGER NOT NULL DEFAULT 0
                    CHECK(registered_only IN (0, 1)),
                created_at      INTEGER NOT NULL,
                opened_at       INTEGER,
                closed_at       INTEGER,
                CHECK(status = 'draft' OR (target_type IS NOT NULL AND opened_at IS NOT NULL)),
                CHECK(status != 'closed' OR closed_at IS NOT NULL)
            );

-- table: tax_fine_cooldowns
CREATE TABLE tax_fine_cooldowns (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                last_fined_at INTEGER NOT NULL,
                last_amount INTEGER NOT NULL DEFAULT 0,
                last_actor_id INTEGER,
                last_reason TEXT,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: tip_transactions
CREATE TABLE tip_transactions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sender_id INTEGER NOT NULL,
                recipient_id INTEGER NOT NULL,
                amount INTEGER NOT NULL,
                fee INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                timestamp INTEGER NOT NULL,
                FOREIGN KEY (sender_id) REFERENCES players(discord_id),
                FOREIGN KEY (recipient_id) REFERENCES players(discord_id)
            );

-- table: trivia_sessions
CREATE TABLE trivia_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                streak INTEGER NOT NULL DEFAULT 0,
                jc_earned INTEGER NOT NULL DEFAULT 0,
                played_at INTEGER NOT NULL
            );

-- table: tunnels
CREATE TABLE tunnels (
                discord_id       INTEGER NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                depth            INTEGER NOT NULL DEFAULT 0,
                max_depth        INTEGER NOT NULL DEFAULT 0,
                total_digs       INTEGER NOT NULL DEFAULT 0,
                total_jc_earned  INTEGER NOT NULL DEFAULT 0,
                last_dig_at      INTEGER,
                streak_days      INTEGER NOT NULL DEFAULT 0,
                streak_last_date TEXT,
                pickaxe_tier     INTEGER NOT NULL DEFAULT 0,
                prestige_level   INTEGER NOT NULL DEFAULT 0,
                prestige_perks   TEXT,
                tunnel_name      TEXT,
                boss_progress    TEXT,
                boss_attempts    TEXT,
                trap_active      INTEGER NOT NULL DEFAULT 0,
                trap_free_today  INTEGER NOT NULL DEFAULT 1,
                trap_date        TEXT,
                insured_until    INTEGER,
                reinforced_until INTEGER,
                injury_state     TEXT,
                paid_digs_today  INTEGER NOT NULL DEFAULT 0,
                paid_dig_date    TEXT,
                revenge_target   INTEGER,
                revenge_type     TEXT,
                revenge_until    INTEGER,
                hard_hat_charges INTEGER NOT NULL DEFAULT 0,
                void_bait_digs   INTEGER NOT NULL DEFAULT 0,
                cheer_data       TEXT,
                created_at       TIMESTAMP DEFAULT CURRENT_TIMESTAMP, luminosity INTEGER NOT NULL DEFAULT 100, temp_buffs TEXT, best_run_score INTEGER NOT NULL DEFAULT 0, current_run_jc INTEGER NOT NULL DEFAULT 0, current_run_artifacts INTEGER NOT NULL DEFAULT 0, current_run_events INTEGER NOT NULL DEFAULT 0, total_prestige_score INTEGER NOT NULL DEFAULT 0, mutations TEXT, thick_skin_date TEXT, engine_mode TEXT NOT NULL DEFAULT 'legacy', miner_origin TEXT NOT NULL DEFAULT '', miner_about TEXT NOT NULL DEFAULT '', stat_strength INTEGER NOT NULL DEFAULT 0, stat_smarts INTEGER NOT NULL DEFAULT 0, stat_stamina INTEGER NOT NULL DEFAULT 0, stat_points INTEGER NOT NULL DEFAULT 5, stat_boss_awards TEXT NOT NULL DEFAULT '[]', stinger_curse TEXT, last_lum_update_at INTEGER, pinnacle_boss_id TEXT, pinnacle_phase INTEGER NOT NULL DEFAULT 0, pinnacle_hp_remaining INTEGER, pinnacle_last_engaged_at INTEGER, retreat_cooldown_until INTEGER, last_cheer_at INTEGER, grappling_hook_charges INTEGER NOT NULL DEFAULT 0, sonar_skip_pending INTEGER NOT NULL DEFAULT 0, temp_curses TEXT, cavein_free_streak INTEGER NOT NULL DEFAULT 0, relic_trim_notice INTEGER NOT NULL DEFAULT 0, auto_buy_torch INTEGER NOT NULL DEFAULT 0, auto_buy_hard_hat INTEGER NOT NULL DEFAULT 0, lantern_stub_date TEXT, route_state TEXT,
                PRIMARY KEY (discord_id, guild_id)
            );

-- table: vanity_tax_enforcements
CREATE TABLE vanity_tax_enforcements (
                guild_id INTEGER NOT NULL DEFAULT 0,
                discord_id INTEGER NOT NULL,
                enforced_by INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (guild_id, discord_id)
            );

-- table: vanity_tax_exemptions
CREATE TABLE vanity_tax_exemptions (
                guild_id INTEGER NOT NULL DEFAULT 0,
                discord_id INTEGER NOT NULL,
                exempted_by INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (guild_id, discord_id)
            );

-- table: wheel_spins
CREATE TABLE wheel_spins (
                spin_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                discord_id INTEGER NOT NULL,
                result INTEGER NOT NULL,
                spin_time INTEGER NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, is_bankrupt INTEGER DEFAULT 0, is_golden INTEGER DEFAULT 0, outcome_code TEXT, is_bonus INTEGER NOT NULL DEFAULT 0, event_id TEXT, outcome_metadata TEXT,
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            );

-- table: wrapped_enrichment_facts
CREATE TABLE wrapped_enrichment_facts (
                guild_id       INTEGER NOT NULL,
                match_id       INTEGER NOT NULL,
                discord_id     INTEGER NOT NULL,
                actions_per_min NUMERIC,
                courier_kills  INTEGER,
                pings          INTEGER,
                rapier_count   INTEGER NOT NULL DEFAULT 0,
                lane_role      INTEGER,
                comeback       NUMERIC,
                throw          NUMERIC,
                PRIMARY KEY (guild_id, match_id, discord_id),
                FOREIGN KEY (match_id, discord_id)
                    REFERENCES match_participants(match_id, discord_id)
                    ON DELETE CASCADE
            );

-- table: wrapped_generation
CREATE TABLE wrapped_generation (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                year_month TEXT NOT NULL,
                channel_id INTEGER,
                message_id INTEGER,
                generated_at INTEGER NOT NULL,
                generated_by INTEGER,
                generation_type TEXT DEFAULT 'auto',
                stats_json TEXT,
                UNIQUE (guild_id, year_month)
            );

-- index: idx_autobet_investments_target
CREATE INDEX idx_autobet_investments_target
            ON autobet_investments(guild_id, target_id)
            ;

-- index: idx_bankruptcy_state_guild
CREATE INDEX idx_bankruptcy_state_guild ON bankruptcy_state(guild_id);

-- index: idx_bet_settlement_taxes_player
CREATE INDEX idx_bet_settlement_taxes_player
            ON bet_settlement_taxes(guild_id, discord_id)
            ;

-- index: idx_bets_autobet_profile
CREATE INDEX idx_bets_autobet_profile
            ON bets(guild_id, discord_id, match_id, is_blind, investment_target_id)
            ;

-- index: idx_bets_discord_id
CREATE INDEX idx_bets_discord_id ON bets(discord_id);

-- index: idx_bets_guild_discord
CREATE INDEX idx_bets_guild_discord ON bets(guild_id, discord_id);

-- index: idx_bets_guild_discord_time
CREATE INDEX idx_bets_guild_discord_time ON bets(guild_id, discord_id, bet_time);

-- index: idx_bets_guild_match_bet_time
CREATE INDEX idx_bets_guild_match_bet_time ON bets(guild_id, match_id, bet_time);

-- index: idx_bets_pending_match
CREATE INDEX idx_bets_pending_match ON bets(pending_match_id);

-- index: idx_dig_actions_guild_actor_created
CREATE INDEX idx_dig_actions_guild_actor_created
            ON dig_actions(guild_id, actor_id, created_at DESC)
            ;

-- index: idx_dig_actions_guild_target_created
CREATE INDEX idx_dig_actions_guild_target_created
            ON dig_actions(guild_id, target_id, created_at DESC)
            ;

-- index: idx_dig_actions_guild_type_created_actor
CREATE INDEX idx_dig_actions_guild_type_created_actor
            ON dig_actions(guild_id, action_type, created_at DESC, actor_id)
            ;

-- index: idx_dig_artifacts_player
CREATE INDEX idx_dig_artifacts_player
            ON dig_artifacts(discord_id, guild_id)
            ;

-- index: idx_dig_gear_player_slot
CREATE INDEX idx_dig_gear_player_slot
            ON dig_gear(discord_id, guild_id, slot)
            ;

-- index: idx_dig_guild_modifiers_active
CREATE INDEX idx_dig_guild_modifiers_active
            ON dig_guild_modifiers(guild_id, expires_at)
            ;

-- index: idx_dig_inventory_player
CREATE INDEX idx_dig_inventory_player
            ON dig_inventory(discord_id, guild_id)
            ;

-- index: idx_disburse_vote_history_proposal
CREATE INDEX idx_disburse_vote_history_proposal
            ON disburse_vote_history(guild_id, proposal_id, finalized_at)
            ;

-- index: idx_disburse_vote_history_voter
CREATE INDEX idx_disburse_vote_history_voter
            ON disburse_vote_history(guild_id, discord_id, voted_at DESC)
            ;

-- index: idx_don_discord_id
CREATE INDEX idx_don_discord_id ON double_or_nothing_spins(discord_id);

-- index: idx_duel_challenger_history
CREATE INDEX idx_duel_challenger_history ON duel_challenges(guild_id, challenger_id, created_at DESC);

-- index: idx_duel_due_expiry
CREATE INDEX idx_duel_due_expiry ON duel_challenges(status, expires_at);

-- index: idx_duel_due_reminder
CREATE INDEX idx_duel_due_reminder ON duel_challenges(status, next_reminder_at);

-- index: idx_duel_guild_status
CREATE INDEX idx_duel_guild_status ON duel_challenges(guild_id, status, created_at DESC);

-- index: idx_duel_recipient_history
CREATE INDEX idx_duel_recipient_history ON duel_challenges(guild_id, recipient_id, created_at DESC);

-- index: idx_draft_finalization_jobs_incomplete
CREATE INDEX idx_draft_finalization_jobs_incomplete
            ON draft_finalization_jobs(stage, lease_until, updated_at)
            ;

-- index: idx_draft_financial_effects_completion
CREATE INDEX idx_draft_financial_effects_completion
            ON draft_financial_effects(completion_key, ordinal)
            ;

-- index: idx_economy_events_active
CREATE INDEX idx_economy_events_active
            ON economy_daily_events(guild_id, starts_at, ends_at)
            ;

-- index: idx_economy_ledger_account
CREATE INDEX idx_economy_ledger_account
            ON economy_ledger_entries(guild_id, account_type, account_id, created_at DESC)
            ;

-- index: idx_economy_ledger_guild_created
CREATE INDEX idx_economy_ledger_guild_created
            ON economy_ledger_entries(guild_id, created_at DESC, ledger_id DESC)
            ;

-- index: idx_economy_ledger_source
CREATE INDEX idx_economy_ledger_source
            ON economy_ledger_entries(guild_id, source, created_at DESC)
            ;

-- index: idx_economy_snapshots_guild_date
CREATE INDEX idx_economy_snapshots_guild_date
            ON economy_daily_snapshots(guild_id, snapshot_date DESC)
            ;

-- index: idx_hostile_loss_kind
CREATE INDEX idx_hostile_loss_kind
            ON hostile_loss_events(guild_id, kind, occurred_at)
            ;

-- index: idx_hostile_loss_retro
CREATE INDEX idx_hostile_loss_retro
            ON hostile_loss_events(
                guild_id, victim_id, shieldable, occurred_at, event_id
            )
            ;

-- index: idx_llm_attempts_created
CREATE INDEX idx_llm_attempts_created
            ON llm_request_attempts(created_at, attempt_id)
            ;

-- index: idx_llm_attempts_provider_model
CREATE INDEX idx_llm_attempts_provider_model
            ON llm_request_attempts(provider, model, created_at DESC)
            ;

-- index: idx_llm_attempts_workload
CREATE INDEX idx_llm_attempts_workload
            ON llm_request_attempts(
                feature, operation, provider, model, created_at DESC
            )
            ;

-- index: idx_loan_state_guild
CREATE INDEX idx_loan_state_guild ON loan_state(guild_id);

-- index: idx_lobby_suspensions_active_guild
CREATE INDEX idx_lobby_suspensions_active_guild
            ON lobby_suspensions(guild_id, active, scope, discord_id)
            ;

-- index: idx_low_priority_active_guild
CREATE INDEX idx_low_priority_active_guild
            ON low_priority_state(guild_id, active, wins_remaining)
            ;

-- index: idx_mafia_actions_actor
CREATE INDEX idx_mafia_actions_actor ON mafia_actions(game_id, actor_id);

-- index: idx_mafia_actions_game_type
CREATE INDEX idx_mafia_actions_game_type ON mafia_actions(game_id, action_type);

-- index: idx_mafia_bounties_day
CREATE INDEX idx_mafia_bounties_day ON mafia_bounties(game_id, day_number);

-- index: idx_mafia_games_guild_phase
CREATE INDEX idx_mafia_games_guild_phase ON mafia_games(guild_id, phase);

-- index: idx_mafia_players_lookup
CREATE INDEX idx_mafia_players_lookup ON mafia_players(guild_id, discord_id);

-- index: idx_mana_protection_victim
CREATE INDEX idx_mana_protection_victim
            ON mana_protection_events(guild_id, victim_id, created_at)
            ;

-- index: idx_manashop_buffs_active
CREATE INDEX idx_manashop_buffs_active
            ON manashop_buffs(guild_id, discord_id, buff_type, triggered, expires_at)
            ;

-- index: idx_manashop_buffs_target
CREATE INDEX idx_manashop_buffs_target
            ON manashop_buffs(guild_id, target_id, buff_type, triggered, expires_at)
            ;

-- index: idx_manashop_daily_purchase
CREATE UNIQUE INDEX idx_manashop_daily_purchase
            ON manashop_daily_uses(purchase_id)
            WHERE purchase_id IS NOT NULL
            ;

-- index: idx_manashop_purchase_lookup
CREATE INDEX idx_manashop_purchase_lookup
            ON manashop_purchases(discord_id, guild_id, item_id, used_date, status)
            ;

-- index: idx_match_bans_match_team_hero
CREATE INDEX idx_match_bans_match_team_hero
            ON match_bans(match_id, team, hero_id)
            ;

-- index: idx_match_corrections_match_id
CREATE INDEX idx_match_corrections_match_id ON match_corrections(match_id);

-- index: idx_match_participants_discord_id
CREATE INDEX idx_match_participants_discord_id ON match_participants(discord_id);

-- index: idx_match_participants_guild
CREATE INDEX idx_match_participants_guild ON match_participants(guild_id, discord_id);

-- index: idx_match_participants_match_id
CREATE INDEX idx_match_participants_match_id ON match_participants(match_id);

-- index: idx_match_participants_scout
CREATE INDEX idx_match_participants_scout
            ON match_participants(
                guild_id, discord_id, match_id, team_number
            )
            ;

-- index: idx_matches_guild_date
CREATE INDEX idx_matches_guild_date ON matches(guild_id, match_date DESC);

-- index: idx_matches_guild_id
CREATE INDEX idx_matches_guild_id ON matches(guild_id);

-- index: idx_matches_match_date
CREATE INDEX idx_matches_match_date ON matches(match_date);

-- index: idx_matches_moderation_completion
CREATE INDEX idx_matches_moderation_completion
            ON matches(guild_id, pending_match_id, lobby_kind)
            ;

-- index: idx_moderation_events_guild_history
CREATE INDEX idx_moderation_events_guild_history
            ON moderation_events(guild_id, event_id DESC)
            ;

-- index: idx_moderation_events_target_history
CREATE INDEX idx_moderation_events_target_history
            ON moderation_events(guild_id, discord_id, event_id DESC)
            ;

-- index: idx_neon_events_user_event
CREATE INDEX idx_neon_events_user_event ON neon_events(discord_id, guild_id, event_type);

-- index: idx_openskill_events_guild_time
CREATE INDEX idx_openskill_events_guild_time
            ON openskill_rating_events(guild_id, event_at, event_id)
            ;

-- index: idx_package_deal_purchases_buyer
CREATE INDEX idx_package_deal_purchases_buyer ON package_deal_purchases(guild_id, buyer_discord_id, created_at);

-- index: idx_package_deal_purchases_partner
CREATE INDEX idx_package_deal_purchases_partner ON package_deal_purchases(guild_id, partner_discord_id, created_at);

-- index: idx_package_deals_buyer
CREATE INDEX idx_package_deals_buyer ON package_deals(guild_id, buyer_discord_id);

-- index: idx_package_deals_guild_active
CREATE INDEX idx_package_deals_guild_active ON package_deals(guild_id, games_remaining);

-- index: idx_package_deals_partner
CREATE INDEX idx_package_deals_partner ON package_deals(guild_id, partner_discord_id);

-- index: idx_pending_matches_guild
CREATE INDEX idx_pending_matches_guild ON pending_matches(guild_id);

-- index: idx_pending_matches_completion_key
CREATE UNIQUE INDEX idx_pending_matches_completion_key ON pending_matches(completion_key) WHERE completion_key IS NOT NULL;

-- index: idx_pet_brawls_open
CREATE INDEX idx_pet_brawls_open ON pet_brawls(guild_id, status) WHERE status IN ('pending', 'active');

-- index: idx_pet_brawls_record_loss
CREATE INDEX idx_pet_brawls_record_loss ON pet_brawls(loser_pet_id) WHERE status = 'done';

-- index: idx_pet_brawls_record_win
CREATE INDEX idx_pet_brawls_record_win ON pet_brawls(winner_pet_id) WHERE status = 'done';

-- index: idx_pet_refunds_unannounced
CREATE INDEX idx_pet_refunds_unannounced ON pet_refund_windows(paid_at) WHERE announcement_payload IS NOT NULL AND announced_at IS NULL;

-- index: idx_pets_alive_starve_scan
CREATE INDEX idx_pets_alive_starve_scan ON pets(last_fed_at) WHERE died_at IS NULL AND is_active = 1;

-- index: idx_pets_evolution_due
CREATE INDEX idx_pets_evolution_due ON pets(evolution_due_at, pet_id) WHERE died_at IS NULL AND is_active = 1 AND evolved_at IS NULL AND evolution_due_at IS NOT NULL;

-- index: idx_pets_evolution_unannounced
CREATE INDEX idx_pets_evolution_unannounced ON pets(evolved_at, pet_id) WHERE evolved_at IS NOT NULL AND evolution_announced_at IS NULL;

-- index: idx_pets_graveyard
CREATE INDEX idx_pets_graveyard ON pets(guild_id, discord_id, died_at DESC);

-- index: idx_pets_one_active
CREATE UNIQUE INDEX idx_pets_one_active ON pets(discord_id, guild_id) WHERE died_at IS NULL AND is_active = 1;

-- index: idx_pets_unannounced_deaths
CREATE INDEX idx_pets_unannounced_deaths ON pets(guild_id, died_at) WHERE died_at IS NOT NULL AND death_announced_at IS NULL;

-- index: idx_pets_unannounced_hatches
CREATE INDEX idx_pets_unannounced_hatches ON pets(guild_id, hatched_at) WHERE died_at IS NULL AND is_active = 1 AND hatch_announced_at IS NULL;

-- index: idx_player_pairings_guild
CREATE INDEX idx_player_pairings_guild ON player_pairings(guild_id);

-- index: idx_player_pairings_player1
CREATE INDEX idx_player_pairings_player1 ON player_pairings(guild_id, player1_id);

-- index: idx_player_pairings_player2
CREATE INDEX idx_player_pairings_player2 ON player_pairings(guild_id, player2_id);

-- index: idx_player_pool_bets_discord
CREATE INDEX idx_player_pool_bets_discord ON player_pool_bets(discord_id);

-- index: idx_player_pool_bets_guild_match
CREATE INDEX idx_player_pool_bets_guild_match ON player_pool_bets(guild_id, match_id, bet_time);

-- index: idx_player_stakes_discord
CREATE INDEX idx_player_stakes_discord ON player_stakes(discord_id);

-- index: idx_player_stakes_guild_match
CREATE INDEX idx_player_stakes_guild_match ON player_stakes(guild_id, match_id, stake_time);

-- index: idx_player_steam_ids_discord
CREATE INDEX idx_player_steam_ids_discord ON player_steam_ids(discord_id);

-- index: idx_player_steam_ids_steam
CREATE INDEX idx_player_steam_ids_steam ON player_steam_ids(steam_id);

-- index: idx_player_trivia_questions_key
CREATE INDEX idx_player_trivia_questions_key
            ON player_trivia_questions(question_key, session_id)
            ;

-- index: idx_player_trivia_sessions_guild_user_start
CREATE INDEX idx_player_trivia_sessions_guild_user_start
            ON player_trivia_sessions(guild_id, discord_id, started_at DESC, session_id DESC)
            ;

-- index: idx_players_guild_id
CREATE INDEX idx_players_guild_id ON players(guild_id);

-- index: idx_players_leaderboard
CREATE INDEX idx_players_leaderboard ON players(guild_id, jopacoin_balance DESC, wins DESC, glicko_rating DESC);

-- index: idx_players_steam_id
CREATE INDEX idx_players_steam_id ON players(steam_id);

-- index: idx_pred_levels_pred_side_price
CREATE INDEX idx_pred_levels_pred_side_price ON prediction_levels(prediction_id, side, price);

-- index: idx_pred_trades_pred
CREATE INDEX idx_pred_trades_pred ON prediction_trades(prediction_id);

-- index: idx_pred_trades_pred_time
CREATE INDEX idx_pred_trades_pred_time ON prediction_trades(prediction_id, trade_time);

-- index: idx_pred_trades_user
CREATE INDEX idx_pred_trades_user ON prediction_trades(discord_id);

-- index: idx_prediction_fair_snapshots_lookup
CREATE INDEX idx_prediction_fair_snapshots_lookup ON prediction_fair_snapshots(market_id, guild_id, snapshot_at);

-- index: idx_prediction_positions_user
CREATE INDEX idx_prediction_positions_user ON prediction_positions(discord_id, prediction_id);

-- index: idx_predictions_guild_status
CREATE INDEX idx_predictions_guild_status ON predictions(guild_id, status);

-- index: idx_rating_history_discord_id
CREATE INDEX idx_rating_history_discord_id ON rating_history(discord_id);

-- index: idx_rating_history_guild
CREATE INDEX idx_rating_history_guild ON rating_history(guild_id, discord_id);

-- index: idx_rating_history_guild_player_time
CREATE INDEX idx_rating_history_guild_player_time
            ON rating_history(guild_id, discord_id, timestamp DESC)
            ;

-- index: idx_rating_history_guild_time
CREATE INDEX idx_rating_history_guild_time
            ON rating_history(guild_id, timestamp DESC)
            ;

-- index: idx_rating_history_match_id
CREATE INDEX idx_rating_history_match_id ON rating_history(match_id);

-- index: idx_recalibration_state_guild
CREATE INDEX idx_recalibration_state_guild ON recalibration_state(guild_id);

-- index: idx_push_notification_targets_guild
CREATE INDEX idx_push_notification_targets_guild ON push_notification_targets(guild_id);

-- index: idx_reminder_prefs_betting
CREATE INDEX idx_reminder_prefs_betting ON reminder_preferences(guild_id, betting_enabled);

-- index: idx_reminder_prefs_dig
CREATE INDEX idx_reminder_prefs_dig ON reminder_preferences(guild_id, dig_enabled);

-- index: idx_reminder_prefs_guild
CREATE INDEX idx_reminder_prefs_guild ON reminder_preferences(guild_id);

-- index: idx_reminder_prefs_lobby
CREATE INDEX idx_reminder_prefs_lobby ON reminder_preferences(guild_id, lobby_enabled);

-- index: idx_reminder_prefs_pet
CREATE INDEX idx_reminder_prefs_pet ON reminder_preferences(guild_id, pet_enabled);

-- index: idx_reminder_prefs_trivia
CREATE INDEX idx_reminder_prefs_trivia ON reminder_preferences(guild_id, trivia_enabled);

-- index: idx_reminder_prefs_wheel
CREATE INDEX idx_reminder_prefs_wheel ON reminder_preferences(guild_id, wheel_enabled);

-- index: idx_soft_avoids_avoided
CREATE INDEX idx_soft_avoids_avoided ON soft_avoids(guild_id, avoided_discord_id);

-- index: idx_soft_avoids_avoider
CREATE INDEX idx_soft_avoids_avoider ON soft_avoids(guild_id, avoider_discord_id);

-- index: idx_soft_avoids_expired
CREATE INDEX idx_soft_avoids_expired ON soft_avoids(guild_id, games_remaining) WHERE games_remaining <= 0;

-- index: idx_spectator_bets_discord
CREATE INDEX idx_spectator_bets_discord ON spectator_bets(discord_id);

-- index: idx_spectator_bets_guild_match
CREATE INDEX idx_spectator_bets_guild_match ON spectator_bets(guild_id, match_id, bet_time);

-- index: idx_survey_answers_question
CREATE INDEX idx_survey_answers_question
            ON survey_answers(guild_id, survey_id, question_id, recipient_id)
            ;

-- index: idx_survey_questions_order
CREATE INDEX idx_survey_questions_order
            ON survey_questions(guild_id, survey_id, position)
            ;

-- index: idx_survey_recipients_active_ui
CREATE INDEX idx_survey_recipients_active_ui
            ON survey_recipients(survey_id, submitted_at, ui_message_id)
            ;

-- index: idx_survey_recipients_delivery
CREATE INDEX idx_survey_recipients_delivery
            ON survey_recipients(delivery_status, claimed_at, recipient_id)
            ;

-- index: idx_survey_recipients_guild_campaign
CREATE INDEX idx_survey_recipients_guild_campaign
            ON survey_recipients(guild_id, survey_id, delivery_status)
            ;

-- index: idx_surveys_guild_status
CREATE INDEX idx_surveys_guild_status
            ON surveys(guild_id, status, survey_id DESC)
            ;

-- index: idx_tax_fine_cooldowns_guild_last
CREATE INDEX idx_tax_fine_cooldowns_guild_last
            ON tax_fine_cooldowns(guild_id, last_fined_at DESC)
            ;

-- index: idx_tip_transactions_guild_recipient_time
CREATE INDEX idx_tip_transactions_guild_recipient_time
            ON tip_transactions(guild_id, recipient_id, timestamp DESC)
            ;

-- index: idx_tip_transactions_guild_sender_time
CREATE INDEX idx_tip_transactions_guild_sender_time
            ON tip_transactions(guild_id, sender_id, timestamp DESC)
            ;

-- index: idx_tip_transactions_recipient
CREATE INDEX idx_tip_transactions_recipient ON tip_transactions(recipient_id);

-- index: idx_tip_transactions_sender
CREATE INDEX idx_tip_transactions_sender ON tip_transactions(sender_id);

-- index: idx_tip_transactions_timestamp
CREATE INDEX idx_tip_transactions_timestamp ON tip_transactions(timestamp);

-- index: idx_trivia_sessions_guild_played
CREATE INDEX idx_trivia_sessions_guild_played
            ON trivia_sessions(guild_id, played_at)
            ;

-- index: idx_tunnels_guild_leaderboard
CREATE INDEX idx_tunnels_guild_leaderboard ON tunnels (guild_id, prestige_level DESC, depth DESC, discord_id);

-- index: idx_wheel_spins_discord_id
CREATE INDEX idx_wheel_spins_discord_id ON wheel_spins(discord_id);

-- index: idx_wheel_spins_guild_event
CREATE INDEX idx_wheel_spins_guild_event
            ON wheel_spins(guild_id, event_id)
            ;

-- index: idx_wheel_spins_guild_player_time
CREATE INDEX idx_wheel_spins_guild_player_time
            ON wheel_spins(guild_id, discord_id, spin_time DESC, spin_id DESC)
            ;

-- index: idx_wheel_spins_spin_time
CREATE INDEX idx_wheel_spins_spin_time ON wheel_spins(spin_time);

-- index: idx_wrapped_guild_month
CREATE INDEX idx_wrapped_guild_month ON wrapped_generation(guild_id, year_month);

-- index: idx_curfew_windows_guild_discord
CREATE INDEX idx_curfew_windows_guild_discord ON player_curfew_windows(guild_id, discord_id);

-- index: uq_dig_gear_one_equipped_per_slot
CREATE UNIQUE INDEX uq_dig_gear_one_equipped_per_slot
            ON dig_gear(discord_id, guild_id, slot) WHERE equipped = 1
            ;

-- index: uq_matches_guild_pending_match
CREATE UNIQUE INDEX uq_matches_guild_pending_match
            ON matches(guild_id, pending_match_id)
            WHERE pending_match_id IS NOT NULL
            ;

-- index: idx_match_discovery_attempts_match_time
CREATE INDEX idx_match_discovery_attempts_match_time
            ON match_discovery_attempts(guild_id, internal_match_id, attempted_at DESC)
            ;

-- index: uq_matches_guild_valve_match
CREATE UNIQUE INDEX uq_matches_guild_valve_match
            ON matches(guild_id, valve_match_id)
            WHERE valve_match_id IS NOT NULL
            ;

-- trigger: match_discovery_attempts_no_delete
CREATE TRIGGER match_discovery_attempts_no_delete
            BEFORE DELETE ON match_discovery_attempts
            BEGIN
                SELECT RAISE(ABORT, 'match_discovery_attempts is append-only');
            END;

-- trigger: match_discovery_attempts_no_update
CREATE TRIGGER match_discovery_attempts_no_update
            BEFORE UPDATE ON match_discovery_attempts
            BEGIN
                SELECT RAISE(ABORT, 'match_discovery_attempts is append-only');
            END;

-- trigger: moderation_events_no_delete
CREATE TRIGGER moderation_events_no_delete
            BEFORE DELETE ON moderation_events
            BEGIN
                SELECT RAISE(ABORT, 'moderation_events is append-only');
            END;

-- trigger: moderation_events_no_update
CREATE TRIGGER moderation_events_no_update
            BEFORE UPDATE ON moderation_events
            BEGIN
                SELECT RAISE(ABORT, 'moderation_events is append-only');
            END;

-- trigger: trg_economy_ledger_nonprofit_insert
CREATE TRIGGER trg_economy_ledger_nonprofit_insert
            AFTER INSERT ON nonprofit_fund
            WHEN COALESCE(NEW.total_collected, 0) != 0
            BEGIN
                INSERT INTO economy_ledger_entries (
                    guild_id, account_type, account_id, delta,
                    balance_before, balance_after, source, actor_id,
                    related_type, related_id, reason, metadata
                )
                VALUES (
                    COALESCE(NEW.guild_id, 0), 'nonprofit', COALESCE(NEW.guild_id, 0),
                    COALESCE(NEW.total_collected, 0), 0,
                    COALESCE(NEW.total_collected, 0),
                    COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'nonprofit_insert'),
                    (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT reason FROM economy_ledger_context WHERE id = 1),
                    (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                );
            END;

-- trigger: trg_economy_ledger_nonprofit_update
CREATE TRIGGER trg_economy_ledger_nonprofit_update
            AFTER UPDATE OF total_collected ON nonprofit_fund
            WHEN COALESCE(OLD.total_collected, 0) != COALESCE(NEW.total_collected, 0)
            BEGIN
                INSERT INTO economy_ledger_entries (
                    guild_id, account_type, account_id, delta,
                    balance_before, balance_after, source, actor_id,
                    related_type, related_id, reason, metadata
                )
                VALUES (
                    COALESCE(NEW.guild_id, 0), 'nonprofit', COALESCE(NEW.guild_id, 0),
                    COALESCE(NEW.total_collected, 0) - COALESCE(OLD.total_collected, 0),
                    COALESCE(OLD.total_collected, 0),
                    COALESCE(NEW.total_collected, 0),
                    COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'nonprofit_update'),
                    (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT reason FROM economy_ledger_context WHERE id = 1),
                    (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                );
            END;

-- trigger: trg_economy_ledger_players_insert
CREATE TRIGGER trg_economy_ledger_players_insert
            AFTER INSERT ON players
            WHEN COALESCE(NEW.jopacoin_balance, 0) != 0
            BEGIN
                INSERT INTO economy_ledger_entries (
                    guild_id, account_type, account_id, delta,
                    balance_before, balance_after, source, actor_id,
                    related_type, related_id, reason, metadata
                )
                VALUES (
                    COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                    COALESCE(NEW.jopacoin_balance, 0), 0,
                    COALESCE(NEW.jopacoin_balance, 0),
                    COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'player_insert'),
                    (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT reason FROM economy_ledger_context WHERE id = 1),
                    (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                );
            END;

-- trigger: trg_economy_ledger_players_update
CREATE TRIGGER trg_economy_ledger_players_update
            AFTER UPDATE OF jopacoin_balance ON players
            WHEN COALESCE(OLD.jopacoin_balance, 0) != COALESCE(NEW.jopacoin_balance, 0)
            BEGIN
                INSERT INTO economy_ledger_entries (
                    guild_id, account_type, account_id, delta,
                    balance_before, balance_after, source, actor_id,
                    related_type, related_id, reason, metadata
                )
                VALUES (
                    COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                    COALESCE(NEW.jopacoin_balance, 0) - COALESCE(OLD.jopacoin_balance, 0),
                    COALESCE(OLD.jopacoin_balance, 0),
                    COALESCE(NEW.jopacoin_balance, 0),
                    COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'balance_update'),
                    (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT reason FROM economy_ledger_context WHERE id = 1),
                    (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                );
            END;

-- trigger: trg_package_deals_games_remaining_insert_cap
CREATE TRIGGER trg_package_deals_games_remaining_insert_cap
            BEFORE INSERT ON package_deals
            WHEN NEW.games_remaining > 10
            BEGIN
                SELECT RAISE(ABORT, 'package deal games_remaining cannot exceed 10');
            END;

-- trigger: trg_package_deals_games_remaining_update_cap
CREATE TRIGGER trg_package_deals_games_remaining_update_cap
            BEFORE UPDATE OF games_remaining ON package_deals
            WHEN NEW.games_remaining > 10
            BEGIN
                SELECT RAISE(ABORT, 'package deal games_remaining cannot exceed 10');
            END;
