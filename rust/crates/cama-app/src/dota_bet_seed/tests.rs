use super::*;

#[test]
fn betting_seed_keeps_regular_contributions_but_excludes_daily_pool() {
    let pool = SeedSplit {
        radiant: 76,
        dire: 75,
        bonus: 0,
    };
    assert_eq!(
        betting_seed_without_lobby_bonus(pool, 50, BettingMode::Pool),
        SeedSplit {
            radiant: 51,
            dire: 50,
            bonus: 0
        }
    );
    assert_eq!(
        betting_seed_without_lobby_bonus(pool, 0, BettingMode::Pool),
        pool
    );
    assert_eq!(
        betting_seed_without_lobby_bonus(pool, 200, BettingMode::Pool),
        SeedSplit::default()
    );
    assert_eq!(
        betting_seed_without_lobby_bonus(
            SeedSplit {
                radiant: 0,
                dire: 0,
                bonus: 175
            },
            50,
            BettingMode::House
        ),
        SeedSplit {
            radiant: 0,
            dire: 0,
            bonus: 125
        }
    );
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
fn test_house_split_routes_queued_pot_to_bonus() {
    let seed = cama_db::dota_bet_seed_repository::split_seed(175, BettingMode::House);
    assert_eq!(
        seed,
        SeedSplit {
            radiant: 0,
            dire: 0,
            bonus: 175
        }
    );
}

#[test]
fn test_house_mode_folds_legacy_split_seed_into_bonus() {
    let legacy = SeedSplit {
        radiant: 88,
        dire: 87,
        bonus: 0,
    };
    assert_eq!(
        normalize_seed_for_mode(BettingMode::House, legacy),
        SeedSplit {
            radiant: 0,
            dire: 0,
            bonus: 175
        }
    );
}
