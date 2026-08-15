use std::cell::RefCell;
use std::convert::Infallible;

use super::*;

const GUILD: GuildId = GuildId(42);

#[derive(Debug)]
struct FakeSeedPort {
    calls: RefCell<Vec<String>>,
    previews: FirstGamePoolBalances,
    claim: i64,
    reservation: SeedReservation,
    settlement: SeedSettlement,
    abort: SeedAbort,
}

impl Default for FakeSeedPort {
    fn default() -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            previews: FirstGamePoolBalances::default(),
            claim: 0,
            reservation: SeedReservation::default(),
            settlement: SeedSettlement::default(),
            abort: SeedAbort::default(),
        }
    }
}

impl DotaBetSeedPort for FakeSeedPort {
    type Error = Infallible;

    fn first_game_pool_previews(
        &self,
        guild_id: GuildId,
        regular_seed_amount: i64,
        game_date: &str,
        daily_amount: i64,
    ) -> Result<FirstGamePoolBalances, Self::Error> {
        self.calls.borrow_mut().push(format!(
            "preview:{}:{regular_seed_amount}:{game_date}:{daily_amount}",
            guild_id.0
        ));
        Ok(self.previews)
    }

    fn fund_first_game_pools(
        &self,
        guild_id: GuildId,
        game_date: &str,
        daily_amount: i64,
    ) -> Result<FundingOutcome, Self::Error> {
        self.calls
            .borrow_mut()
            .push(format!("fund:{}:{game_date}:{daily_amount}", guild_id.0));
        Ok(FundingOutcome::default())
    }

    fn claim_first_game_pool(
        &self,
        guild_id: GuildId,
        lobby: LobbyKind,
        game_date: &str,
        pending_match_id: i64,
    ) -> Result<i64, Self::Error> {
        self.calls.borrow_mut().push(format!(
            "claim:{}:{lobby:?}:{game_date}:{pending_match_id}",
            guild_id.0
        ));
        Ok(self.claim)
    }

    fn reserve_seed_atomic(
        &self,
        guild_id: GuildId,
        pending_match_id: i64,
        max_seed_amount: i64,
        first_game_reserved: i64,
        mode: BettingMode,
    ) -> Result<SeedReservation, Self::Error> {
        self.calls.borrow_mut().push(format!(
            "reserve:{}:{pending_match_id}:{max_seed_amount}:{first_game_reserved}:{mode:?}",
            guild_id.0
        ));
        Ok(self.reservation)
    }

    fn settle_seed_atomic(
        &self,
        guild_id: GuildId,
        pending_match_id: i64,
        mode: BettingMode,
        winning_team: Option<BettingTeam>,
    ) -> Result<SeedSettlement, Self::Error> {
        self.calls.borrow_mut().push(format!(
            "settle:{}:{pending_match_id}:{mode:?}:{winning_team:?}",
            guild_id.0
        ));
        Ok(self.settlement)
    }

    fn abort_seed_atomic(
        &self,
        guild_id: GuildId,
        pending_match_id: i64,
    ) -> Result<SeedAbort, Self::Error> {
        self.calls
            .borrow_mut()
            .push(format!("abort:{}:{pending_match_id}", guild_id.0));
        Ok(self.abort)
    }
}

fn bet(bet_id: i64, user_id: i64, team: BettingTeam, amount: i64) -> PendingBet {
    PendingBet {
        bet_id,
        user_id: UserId(user_id),
        team,
        amount,
        leverage: 1,
    }
}

#[test]
fn test_loan_service_previews_use_current_game_date() {
    fn fixed_game_date() -> String {
        "2026-08-05".to_owned()
    }

    let port = FakeSeedPort {
        previews: FirstGamePoolBalances {
            open: 150,
            low_skill: 150,
        },
        ..FakeSeedPort::default()
    };
    let service = DotaBetSeedService::new(port, 50, 100).with_game_date_provider(fixed_game_date);

    let previews = service.get_first_game_pool_previews(GUILD).unwrap();

    assert_eq!(
        previews,
        FirstGamePoolBalances {
            open: 150,
            low_skill: 150
        }
    );
    assert_eq!(
        service.port().calls.borrow().as_slice(),
        ["preview:42:50:2026-08-05:100"]
    );
}

#[test]
fn test_draft_pending_match_reserves_pool_seed_from_nonprofit_fund() {
    let reservation = SeedReservation {
        reserved: 50,
        split: SeedSplit {
            radiant: 25,
            dire: 25,
            bonus: 0,
        },
        first_game_reserved: 0,
    };
    let port = FakeSeedPort {
        reservation,
        ..FakeSeedPort::default()
    };
    let service = DotaBetSeedService::new(port, 50, 100);

    let result = service
        .reserve_pending(PendingSeedRequest {
            guild_id: GUILD,
            pending_match_id: 111,
            mode: BettingMode::Pool,
            lobby: LobbyKind::Open,
            game_date: "2026-08-05",
        })
        .unwrap();

    assert_eq!(result, reservation);
    assert_eq!(
        service.port().calls.borrow().as_slice(),
        [
            "fund:42:2026-08-05:100",
            "claim:42:Open:2026-08-05:111",
            "reserve:42:111:50:0:Pool",
        ]
    );
}

#[test]
fn test_pool_settlement_pays_losing_seed_and_returns_winning_seed() {
    let distribution = calculate_settlement(
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[
            bet(1, 30150, BettingTeam::Radiant, 10),
            bet(2, 30151, BettingTeam::Dire, 10),
        ],
        SeedSplit {
            radiant: 25,
            dire: 25,
            bonus: 0,
        },
    );

    assert_eq!(distribution.total_payout(), 45);
    assert_eq!(distribution.winners[0].user_id, UserId(30150));
    assert_eq!(distribution.losers[0].user_id, UserId(30151));
    assert_eq!(distribution.seed_returned, 25);
}

#[test]
fn test_pool_settlement_burns_losers_when_only_seed_is_on_winning_side() {
    let distribution = calculate_settlement(
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[bet(1, 30250, BettingTeam::Dire, 10)],
        SeedSplit {
            radiant: 25,
            dire: 25,
            bonus: 0,
        },
    );

    assert!(distribution.winners.is_empty());
    assert_eq!(distribution.losers[0].user_id, UserId(30250));
    assert_eq!(distribution.seed_returned, 50);
}

#[test]
fn test_house_settlement_adds_seed_bonus_to_winning_bettors() {
    let distribution = calculate_settlement(
        BettingMode::House,
        BettingTeam::Radiant,
        &[bet(1, 30350, BettingTeam::Radiant, 10)],
        SeedSplit {
            radiant: 0,
            dire: 0,
            bonus: 50,
        },
    );

    assert_eq!(distribution.total_payout(), 70);
    assert_eq!(distribution.seed_returned, 0);
}

#[test]
fn test_pool_betting_display_includes_seeded_odds() {
    let field = betting_field(
        BettingMode::Pool,
        BetTotals {
            radiant: 10,
            dire: 0,
        },
        SeedSplit {
            radiant: 25,
            dire: 25,
            bonus: 0,
        },
    );

    assert_eq!(field.name, "💰 Pool Betting");
    assert!(field.value.contains("Radiant: 10"));
    assert!(field.value.contains("(3.50x)"));
    assert!(field.value.contains("Dire: 0"));
    assert!(field.value.contains("Seed: Radiant 25"));
    assert!(field.value.contains("Dire 25"));
}

#[test]
fn test_house_betting_display_includes_seed_bonus() {
    let field = betting_field(
        BettingMode::House,
        BetTotals {
            radiant: 10,
            dire: 0,
        },
        SeedSplit {
            radiant: 0,
            dire: 0,
            bonus: 50,
        },
    );

    assert_eq!(field.name, "💰 House Betting (1:1)");
    assert!(field.value.contains("Winner bonus: 50"));
}

#[test]
fn test_mybets_pool_potential_payout_uses_seeded_pool() {
    let projection = project_my_bet(
        BettingMode::Pool,
        BettingTeam::Radiant,
        10,
        BetTotals {
            radiant: 10,
            dire: 0,
        },
        SeedSplit {
            radiant: 25,
            dire: 25,
            bonus: 0,
        },
    )
    .expect("pool projection");
    let rendered = projection.render(BettingMode::Pool, BettingTeam::Radiant);

    assert_eq!(projection.payout, 35);
    assert!(rendered.contains("If Radiant wins: ~35"));
    assert!(rendered.contains("(2.50:1)"));
}

#[test]
fn test_mybets_house_potential_payout_includes_seed_bonus_share() {
    let projection = project_my_bet(
        BettingMode::House,
        BettingTeam::Radiant,
        10,
        BetTotals {
            radiant: 20,
            dire: 0,
        },
        SeedSplit {
            radiant: 0,
            dire: 0,
            bonus: 50,
        },
    )
    .expect("house projection");
    let rendered = projection.render(BettingMode::House, BettingTeam::Radiant);

    assert_eq!(projection.payout, 45);
    assert_eq!(projection.bonus_share, 25);
    assert!(rendered.contains("If Radiant wins: 45"));
    assert!(rendered.contains("1:1 + ~25 bonus"));
}

#[test]
fn test_bets_house_display_shows_bonus_not_pool_odds() {
    let overview = bets_overview(
        503,
        1,
        BettingMode::House,
        BetTotals {
            radiant: 10,
            dire: 0,
        },
        SeedSplit {
            radiant: 0,
            dire: 0,
            bonus: 50,
        },
    );

    assert!(overview.title.contains("House Bets"));
    assert!(overview.current_odds.contains("(1:1)"));
    assert!(overview.current_odds.contains("Winner bonus: 50"));
    assert!(!overview.title.contains("Pool"));
    assert!(!overview.current_odds.contains("3.50x"));
}

#[test]
fn test_roll_and_russianroulette_extensions_are_removed() {
    let report =
        inspect_extension_manifest(["commands.lobby", "commands.betting", "commands.mafia"]);

    assert!(report.is_valid());
    assert!(report.retired_loaded.is_empty());
    assert!(!report.required_missing);
}

#[test]
fn test_house_shuffle_routes_queued_pot_to_bonus_and_pays_it_out() {
    let seed = cama_db::dota_bet_seed_repository::split_seed(175, BettingMode::House);
    let distribution = calculate_settlement(
        BettingMode::House,
        BettingTeam::Radiant,
        &[bet(1, 30550, BettingTeam::Radiant, 10)],
        seed,
    );

    assert_eq!(
        seed,
        SeedSplit {
            radiant: 0,
            dire: 0,
            bonus: 175
        }
    );
    assert_eq!(distribution.total_payout(), 195);
    assert_eq!(distribution.seed_returned, 0);
}

#[test]
fn test_house_settlement_folds_legacy_split_seed_into_bonus() {
    let legacy = SeedSplit {
        radiant: 88,
        dire: 87,
        bonus: 0,
    };
    let normalized = normalize_seed_for_mode(BettingMode::House, legacy);
    let distribution = calculate_settlement(
        BettingMode::House,
        BettingTeam::Radiant,
        &[bet(1, 30650, BettingTeam::Radiant, 10)],
        legacy,
    );

    assert_eq!(
        normalized,
        SeedSplit {
            radiant: 0,
            dire: 0,
            bonus: 175
        }
    );
    assert_eq!(distribution.total_payout(), 195);
}

#[test]
fn stronger_service_derives_real_winner_before_consuming_seed() {
    let port = FakeSeedPort {
        settlement: SeedSettlement {
            routed: SeedSplit {
                radiant: 0,
                dire: 0,
                bonus: 50,
            },
            winner_seed: 50,
            returned_to_fund: 0,
            first_game_settled: 0,
        },
        ..FakeSeedPort::default()
    };
    let service = DotaBetSeedService::new(port, 50, 100);

    let result = service
        .settle_pending(
            GUILD,
            700,
            BettingMode::House,
            BettingTeam::Radiant,
            &[bet(1, 9, BettingTeam::Radiant, 10)],
        )
        .unwrap();

    assert_eq!(result.distribution.total_payout(), 70);
    assert_eq!(
        service.port().calls.borrow().as_slice(),
        ["settle:42:700:House:Some(Radiant)"]
    );
}

#[test]
fn stronger_house_bonus_rounding_is_deterministic_and_conserves_seed() {
    let distribution = calculate_settlement(
        BettingMode::House,
        BettingTeam::Radiant,
        &[
            bet(1, 20, BettingTeam::Radiant, 1),
            bet(2, 10, BettingTeam::Radiant, 1),
            bet(3, 30, BettingTeam::Radiant, 1),
        ],
        SeedSplit {
            radiant: 0,
            dire: 0,
            bonus: 2,
        },
    );
    let base = 6;

    assert_eq!(distribution.total_payout().saturating_sub(base), 2);
    assert_eq!(
        distribution
            .winners
            .iter()
            .map(|winner| winner.user_id)
            .collect::<Vec<_>>(),
        [UserId(20), UserId(10), UserId(30)]
    );
}
