use super::*;

#[test]
fn test_post_match_gif_theme_matches_the_verified_event_buyback_denied() {
    let context = JopatPostMatchContext {
        loser_name: Some("LeveragedLarry".to_owned()),
        loss: 500,
        leverage: 5,
        ..JopatPostMatchContext::default()
    };

    assert_eq!(
        post_match_gif_theme(&context),
        Some(PostMatchGifSpec {
            kind: PostMatchGifKind::BuybackDenied,
            client_name: "LeveragedLarry".to_owned(),
            value: 500,
        })
    );
}

#[test]
fn test_post_match_gif_theme_matches_the_verified_event_odds_anomaly() {
    let context = JopatPostMatchContext {
        winner_name: Some("UpsetWinner".to_owned()),
        expected_win_prob: Some(NumericFact::from(0.25)),
        ..JopatPostMatchContext::default()
    };

    assert_eq!(
        post_match_gif_theme(&context),
        Some(PostMatchGifSpec {
            kind: PostMatchGifKind::OddsAnomaly,
            client_name: "UpsetWinner".to_owned(),
            value: 25,
        })
    );
}

#[test]
fn test_post_match_gif_theme_matches_the_verified_event_beyond_godlike() {
    let context = JopatPostMatchContext {
        winner_name: Some("HotHand".to_owned()),
        streak: 7,
        ..JopatPostMatchContext::default()
    };

    assert_eq!(
        post_match_gif_theme(&context),
        Some(PostMatchGifSpec {
            kind: PostMatchGifKind::BeyondGodlike,
            client_name: "HotHand".to_owned(),
            value: 7,
        })
    );
}

#[test]
fn test_post_match_gif_theme_matches_the_verified_event_divine_rapier_position() {
    let context = JopatPostMatchContext {
        winner_name: Some("Climber".to_owned()),
        rating_change: Some(NumericFact::from(40_i64)),
        ..JopatPostMatchContext::default()
    };

    assert_eq!(
        post_match_gif_theme(&context),
        Some(PostMatchGifSpec {
            kind: PostMatchGifKind::DivineRapierPosition,
            client_name: "Climber".to_owned(),
            value: 40,
        })
    );
}

#[test]
fn test_post_match_gif_theme_matches_the_verified_event_ancient_liquidated() {
    let context = JopatPostMatchContext {
        winner_name: Some("Bettor".to_owned()),
        payout: 750,
        ..JopatPostMatchContext::default()
    };

    assert_eq!(
        post_match_gif_theme(&context),
        Some(PostMatchGifSpec {
            kind: PostMatchGifKind::AncientLiquidated,
            client_name: "Bettor".to_owned(),
            value: 750,
        })
    );
}

#[test]
fn test_post_match_gif_theme_matches_the_verified_event_negative_streak_is_none() {
    let context = JopatPostMatchContext {
        loser_name: Some("ColdHand".to_owned()),
        streak: -7,
        ..JopatPostMatchContext::default()
    };

    assert_eq!(post_match_gif_theme(&context), None);
}

#[test]
fn test_post_match_gif_theme_matches_the_verified_event_missing_winner_is_none() {
    let context = JopatPostMatchContext {
        expected_win_prob: Some(NumericFact::from(0.25)),
        ..JopatPostMatchContext::default()
    };

    assert_eq!(post_match_gif_theme(&context), None);
}
