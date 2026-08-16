use std::{collections::BTreeMap, io::Cursor};

use gif::{ColorOutput, DecodeOptions};

use super::*;

#[derive(Clone, Debug, Default)]
struct FakeAi {
    output: Option<String>,
    fail: bool,
    calls: usize,
}

impl JopatAiPort for FakeAi {
    fn complete(&mut self, _request: &AiRequest) -> Result<Option<String>, RuntimePortError> {
        self.calls += 1;
        if self.fail {
            Err(RuntimePortError("provider unavailable".to_owned()))
        } else {
            Ok(self.output.clone())
        }
    }
}

#[derive(Clone, Debug, Default)]
struct FakeIdentity {
    names: BTreeMap<u64, String>,
}

impl JopatIdentityPort for FakeIdentity {
    fn player_name(
        &mut self,
        discord_id: u64,
        _guild_id: Option<u64>,
    ) -> Result<Option<String>, RuntimePortError> {
        Ok(self.names.get(&discord_id).cloned())
    }

    fn hero_name(&mut self, hero_id: u16) -> Result<Option<String>, RuntimePortError> {
        Ok(Some(format!("Hero-{hero_id}")))
    }
}

#[derive(Clone, Debug, Default)]
struct FakeGifRenderer {
    fail: bool,
    rendered: Vec<PostMatchGifSpec>,
}

impl PostMatchGifPort for FakeGifRenderer {
    fn render(&mut self, spec: &PostMatchGifSpec) -> Result<GifAsset, RuntimePortError> {
        self.rendered.push(spec.clone());
        if self.fail {
            return Err(RuntimePortError("render failed".to_owned()));
        }
        ContractGifRenderer.render(spec)
    }
}

fn service(
    random: ScriptedJopatMatchRandom,
) -> JopatMatchService<ScriptedJopatMatchRandom, FakeIdentity, FakeAi, FakeGifRenderer> {
    JopatMatchService::new(
        random,
        FakeIdentity::default(),
        FakeAi::default(),
        FakeGifRenderer::default(),
    )
}

#[derive(Default)]
struct FakePets {
    feeds: Vec<PetFeed>,
    calls: usize,
}

impl PetMatchPort for FakePets {
    fn on_match_win(
        &mut self,
        _winning_player_ids: &[u64],
        _guild_id: Option<u64>,
    ) -> Result<Vec<PetFeed>, RuntimePortError> {
        self.calls += 1;
        Ok(self.feeds.clone())
    }
}

#[derive(Default)]
struct FakeDelivery {
    texts: Vec<String>,
    neon: Vec<NeonResult>,
}

impl MatchDeliveryPort for FakeDelivery {
    fn send_public_text(&mut self, content: String) -> Result<(), RuntimePortError> {
        self.texts.push(content);
        Ok(())
    }

    fn send_neon(&mut self, result: NeonResult) -> Result<(), RuntimePortError> {
        self.neon.push(result);
        Ok(())
    }
}

fn pet_feed(fullness_gain: i64) -> PetFeed {
    PetFeed {
        pet_name: "Blep".to_owned(),
        species_id: "common_cama".to_owned(),
        fullness_gain,
    }
}

#[test]
fn test_pet_match_hook_reports_fullness_gain() {
    let mut pets = FakePets {
        feeds: vec![pet_feed(10)],
        ..FakePets::default()
    };
    let mut delivery = FakeDelivery::default();

    run_pet_match_hooks(&mut pets, &mut delivery, Some(10), &[1]);

    assert_eq!(pets.calls, 1);
    assert_eq!(delivery.texts.len(), 1);
    assert!(delivery.texts[0].contains("munches +10 fullness in celebration"));
}

#[test]
fn test_pet_match_hook_calls_out_already_full_pet() {
    let mut pets = FakePets {
        feeds: vec![pet_feed(0)],
        ..FakePets::default()
    };
    let mut delivery = FakeDelivery::default();

    run_pet_match_hooks(&mut pets, &mut delivery, Some(10), &[1]);

    assert!(
        delivery.texts[0].contains("munches a victory snack — already full"),
        "{}",
        delivery.texts[0]
    );
    assert!(!delivery.texts[0].contains("+0 fullness"));
}

#[test]
fn test_post_match_debrief_uses_base_chance_for_ordinary_match() {
    let mut service = service(ScriptedJopatMatchRandom::always(false));
    let context = JopatPostMatchContext {
        winner_name: Some("Winner".to_owned()),
        ..JopatPostMatchContext::default()
    };

    let result = service.on_post_match_debrief(Some(10), context, None, None);

    assert!(result.is_none());
    assert_eq!(
        service.random().observed_chances(),
        &[POST_MATCH_BASE_CHANCE]
    );
}

#[test]
fn test_post_match_debrief_notable_context_uses_middle_chance() {
    let context = JopatPostMatchContext {
        rating_change: Some(NumericFact::from(25_i64)),
        ..JopatPostMatchContext::default()
    };

    assert_eq!(post_match_chance(&context), POST_MATCH_NOTABLE_CHANCE);
}

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

#[test]
fn test_post_match_debrief_uses_extreme_chance_and_ansi_fallback() {
    let mut service = service(ScriptedJopatMatchRandom::with_rolls([true, false]));
    let context = JopatPostMatchContext {
        loser_name: Some("LeveragedLarry".to_owned()),
        loss: 500,
        leverage: 5,
        ..JopatPostMatchContext::default()
    };

    let result = service
        .on_post_match_debrief(Some(10), context, None, None)
        .expect("extreme roll succeeds");

    assert_eq!(result.layer, 2);
    let text = result.text_block.expect("fallback text");
    assert!(text.starts_with("```ansi\n"));
    assert!(text.contains("JOPA-T"));
    assert_eq!(
        service.random().observed_chances().first(),
        Some(&POST_MATCH_EXTREME_CHANCE)
    );
}

#[test]
fn test_post_match_debrief_can_promote_extreme_context_to_themed_gif() {
    let mut service = service(ScriptedJopatMatchRandom::with_rolls([true, true]));
    let context = JopatPostMatchContext {
        winner_name: Some("UpsetWinner".to_owned()),
        expected_win_prob: Some(NumericFact::from(0.25)),
        rating_change: Some(NumericFact::from(42_i64)),
        ..JopatPostMatchContext::default()
    };

    let result = service
        .on_post_match_debrief(Some(10), context, None, None)
        .expect("extreme roll succeeds");

    assert_eq!(result.layer, 3);
    let gif = result.gif_file.expect("themed GIF");
    assert!(gif.bytes.starts_with(b"GIF87a") || gif.bytes.starts_with(b"GIF89a"));
    assert_eq!(gif.frame_durations_ms.len(), 18);
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::Indexed);
    let mut decoder = options
        .read_info(Cursor::new(&gif.bytes))
        .expect("decode production themed GIF");
    assert_eq!((decoder.width(), decoder.height()), (400, 300));
    let mut frame_count = 0;
    while decoder
        .read_next_frame()
        .expect("read production themed GIF frame")
        .is_some()
    {
        frame_count += 1;
    }
    assert_eq!(frame_count, 18);
}

#[test]
fn test_post_match_debrief_falls_back_when_ai_fails() {
    let mut service = JopatMatchService::new(
        ScriptedJopatMatchRandom::always(true),
        FakeIdentity::default(),
        FakeAi {
            fail: true,
            ..FakeAi::default()
        },
        FakeGifRenderer::default(),
    )
    .with_ai_enabled(true);
    let context = JopatPostMatchContext {
        winner_name: Some("Winner".to_owned()),
        payout: 250,
        ..JopatPostMatchContext::default()
    };

    let result = service
        .on_post_match_debrief(Some(10), context, None, None)
        .expect("debrief fallback");

    let text = result.text_block.expect("fallback text");
    assert!(text.starts_with("```ansi\n"));
    assert!(text.contains("JOPA-T"));
    assert_eq!(service.ai().calls, 1);
}

#[test]
fn test_post_match_debrief_keeps_text_when_gif_rendering_fails() {
    let mut service = JopatMatchService::new(
        ScriptedJopatMatchRandom::with_rolls([true, true]),
        FakeIdentity::default(),
        FakeAi::default(),
        FakeGifRenderer {
            fail: true,
            ..FakeGifRenderer::default()
        },
    );
    let context = JopatPostMatchContext {
        loser_name: Some("Loser".to_owned()),
        loss: 500,
        leverage: 5,
        ..JopatPostMatchContext::default()
    };

    let result = service
        .on_post_match_debrief(Some(10), context, None, None)
        .expect("text survives renderer error");

    assert_eq!(result.layer, 2);
    assert!(result.text_block.is_some());
    assert!(result.gif_file.is_none());
}

#[derive(Default)]
struct FakeNeon {
    big_win_result: Option<NeonResult>,
    milestone_result: Option<NeonResult>,
    debrief_result: Option<NeonResult>,
    scores: BTreeMap<u64, i64>,
    big_win_calls: usize,
    degen_calls: Vec<(Vec<u64>, Option<u64>)>,
    milestone_calls: Vec<(u64, Option<u64>, i64)>,
    debrief_calls: Vec<(JopatPostMatchContext, Option<u64>, Option<u64>)>,
}

impl MatchNeonPort for FakeNeon {
    fn on_big_win(
        &mut self,
        _discord_id: u64,
        _guild_id: Option<u64>,
        _payout: i64,
        _flavor: BigWinFlavor,
    ) -> Option<NeonResult> {
        self.big_win_calls += 1;
        self.big_win_result.take()
    }

    fn degen_scores(&mut self, discord_ids: &[u64], guild_id: Option<u64>) -> BTreeMap<u64, i64> {
        self.degen_calls.push((discord_ids.to_vec(), guild_id));
        self.scores.clone()
    }

    fn on_degen_milestone(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        score: i64,
    ) -> Option<NeonResult> {
        self.milestone_calls.push((discord_id, guild_id, score));
        self.milestone_result.take()
    }

    fn on_post_match_debrief(
        &mut self,
        _guild_id: Option<u64>,
        context: JopatPostMatchContext,
        winner_id: Option<u64>,
        loser_id: Option<u64>,
    ) -> Option<NeonResult> {
        self.debrief_calls.push((context, winner_id, loser_id));
        self.debrief_result.take()
    }
}

#[derive(Default)]
struct FakeBalances {
    bulk: BTreeMap<u64, i64>,
    individual: BTreeMap<u64, i64>,
    bulk_calls: usize,
    individual_calls: usize,
}

impl MatchBalancePort for FakeBalances {
    fn get_balances(
        &mut self,
        _discord_ids: &[u64],
        _guild_id: Option<u64>,
    ) -> Result<BTreeMap<u64, i64>, RuntimePortError> {
        self.bulk_calls += 1;
        Ok(self.bulk.clone())
    }

    fn get_balance(
        &mut self,
        discord_id: u64,
        _guild_id: Option<u64>,
    ) -> Result<i64, RuntimePortError> {
        self.individual_calls += 1;
        Ok(self.individual.get(&discord_id).copied().unwrap_or(100))
    }
}

#[derive(Default)]
struct FakeRatings {
    history: Vec<RatingHistoryEntry>,
    calls: usize,
}

impl MatchRatingPort for FakeRatings {
    fn rating_history(
        &mut self,
        _match_id: i64,
    ) -> Result<Vec<RatingHistoryEntry>, RuntimePortError> {
        self.calls += 1;
        Ok(self.history.clone())
    }
}

fn run_route(
    neon: &mut FakeNeon,
    balances: &mut FakeBalances,
    ratings: &mut FakeRatings,
    delivery: &mut FakeDelivery,
    winners: &[SettlementWinner],
    losers: &[SettlementLoser],
    record: &MatchRouteRecord,
) {
    let mut choice = ScriptedJopatMatchRandom::always(false);
    run_neon_match_hooks(
        MatchRouteInput {
            guild_id: Some(10),
            winners,
            losers,
            record,
            max_debt: 1_000,
        },
        neon,
        balances,
        ratings,
        delivery,
        &mut choice,
    );
}

#[test]
fn test_match_hook_keeps_big_win_priority_and_sends_only_one() {
    let mut neon = FakeNeon {
        big_win_result: Some(NeonResult::text(3, "headline")),
        debrief_result: Some(NeonResult::text(2, "debrief")),
        ..FakeNeon::default()
    };
    let mut balances = FakeBalances::default();
    let mut ratings = FakeRatings::default();
    let mut delivery = FakeDelivery::default();
    let winners = [SettlementWinner {
        discord_id: 1,
        amount: 50,
        payout: 500,
        refunded: false,
    }];
    let losers = [SettlementLoser {
        discord_id: 2,
        amount: 100,
        ..SettlementLoser::default()
    }];
    let record = MatchRouteRecord {
        match_id: 99,
        winning_player_ids: vec![1],
        notable_streak: None,
    };

    run_route(
        &mut neon,
        &mut balances,
        &mut ratings,
        &mut delivery,
        &winners,
        &losers,
        &record,
    );

    assert_eq!(delivery.neon.len(), 1);
    assert!(neon.debrief_calls.is_empty());
}

#[test]
fn test_match_hook_routes_ordinary_match_through_single_jopat_gateway() {
    let mut neon = FakeNeon {
        debrief_result: Some(NeonResult::text(2, "debrief")),
        ..FakeNeon::default()
    };
    let mut balances = FakeBalances::default();
    let mut ratings = FakeRatings::default();
    let mut delivery = FakeDelivery::default();
    let record = MatchRouteRecord {
        match_id: 99,
        winning_player_ids: vec![1],
        notable_streak: None,
    };

    run_route(
        &mut neon,
        &mut balances,
        &mut ratings,
        &mut delivery,
        &[],
        &[],
        &record,
    );

    assert_eq!(neon.debrief_calls.len(), 1);
    assert_eq!(delivery.neon.len(), 1);
}

#[test]
fn test_match_hook_keeps_selected_facts_attached_to_their_player() {
    let mut neon = FakeNeon {
        debrief_result: Some(NeonResult::text(2, "debrief")),
        ..FakeNeon::default()
    };
    let mut balances = FakeBalances::default();
    let mut ratings = FakeRatings {
        history: vec![RatingHistoryEntry {
            discord_id: 1,
            rating_before: Some(1_500.0),
            rating_after: Some(1_550.0),
            expected_win_prob: Some(0.25),
            ..RatingHistoryEntry::default()
        }],
        ..FakeRatings::default()
    };
    let mut delivery = FakeDelivery::default();
    let losers = [SettlementLoser {
        discord_id: 2,
        amount: 100,
        effective_bet: Some(500),
        leverage: 5,
        ..SettlementLoser::default()
    }];
    let record = MatchRouteRecord {
        match_id: 99,
        winning_player_ids: vec![1],
        notable_streak: None,
    };

    run_route(
        &mut neon,
        &mut balances,
        &mut ratings,
        &mut delivery,
        &[],
        &losers,
        &record,
    );

    let (context, winner_id, loser_id) = &neon.debrief_calls[0];
    assert_eq!(context.loss, 500);
    assert_eq!(context.leverage, 5);
    assert!(context.rating_change.is_none());
    assert_eq!((*winner_id, *loser_id), (None, Some(2)));
}

#[test]
fn test_match_hook_reuses_settlement_balances_and_bulk_degen_scores() {
    let mut neon = FakeNeon {
        milestone_result: Some(NeonResult::text(2, "milestone")),
        scores: BTreeMap::from([(2, 91), (3, 40)]),
        ..FakeNeon::default()
    };
    let mut balances = FakeBalances::default();
    let mut ratings = FakeRatings::default();
    let mut delivery = FakeDelivery::default();
    let losers = [
        SettlementLoser {
            discord_id: 2,
            amount: 10,
            balance_after: Some(100),
            ..SettlementLoser::default()
        },
        SettlementLoser {
            discord_id: 3,
            amount: 20,
            balance_after: Some(200),
            ..SettlementLoser::default()
        },
    ];
    let record = MatchRouteRecord {
        match_id: 99,
        winning_player_ids: Vec::new(),
        notable_streak: None,
    };

    run_route(
        &mut neon,
        &mut balances,
        &mut ratings,
        &mut delivery,
        &[],
        &losers,
        &record,
    );

    assert_eq!(neon.degen_calls, [(vec![2, 3], Some(10))]);
    assert_eq!(neon.milestone_calls, [(2, Some(10), 91)]);
    assert_eq!(balances.bulk_calls, 0);
    assert_eq!(balances.individual_calls, 0);
    assert_eq!(delivery.neon.len(), 1);
}

fn enriched_winner(discord_id: u64, kills: i32) -> EnrichedPlayer {
    EnrichedPlayer {
        discord_id: Some(discord_id),
        hero_id: Some(1),
        kills: Some(kills),
        deaths: Some(2),
        assists: Some(10),
        gpm: Some(500 + kills),
        fantasy_points: Some(f64::from(20 + kills)),
        ..EnrichedPlayer::default()
    }
}

#[test]
fn test_enrichment_rolls_once_and_returns_at_most_one_callout() {
    let mut service = service(ScriptedJopatMatchRandom::always(true));
    let winners = (5..10)
        .enumerate()
        .map(|(index, kills)| enriched_winner(index as u64 + 1, kills))
        .collect::<Vec<_>>();

    let results = service.on_match_enriched(Some(10), &winners, &[]);

    assert_eq!(results.len(), 1);
    assert_eq!(service.random().observed_chances().len(), 1);
}

#[test]
fn test_enrichment_uses_the_configured_per_match_chance() {
    let mut service = service(ScriptedJopatMatchRandom::always(false)).with_mvp_chance(0.23);

    let results = service.on_match_enriched(Some(10), &[enriched_winner(1, 8)], &[]);

    assert!(results.is_empty());
    assert_eq!(service.random().observed_chances(), &[0.23]);
}

#[test]
fn test_enrichment_can_roast_a_genuinely_extreme_loser() {
    let mut service = service(ScriptedJopatMatchRandom::always(true));
    let loser = EnrichedPlayer {
        discord_id: Some(44),
        hero_id: Some(2),
        kills: Some(0),
        deaths: Some(16),
        assists: Some(2),
        gpm: Some(180),
        xpm: Some(210),
        ..EnrichedPlayer::default()
    };

    let results = service.on_match_enriched(Some(10), &[], &[loser]);

    assert_eq!(results.len(), 1);
    let text = results[0].text_block.as_deref().expect("callout text");
    assert!(text.contains("Client-44") || text.contains("<@44>"));
}

#[test]
fn test_enrichment_does_not_roast_an_ordinary_loser() {
    let mut service = service(ScriptedJopatMatchRandom::always(true));
    let loser = EnrichedPlayer {
        discord_id: Some(45),
        hero_id: Some(2),
        kills: Some(0),
        deaths: Some(6),
        assists: Some(1),
        gpm: Some(410),
        xpm: Some(430),
        ..EnrichedPlayer::default()
    };

    let results = service.on_match_enriched(Some(10), &[], &[loser]);

    assert!(results.is_empty());
    assert!(service.random().observed_chances().is_empty());
}
