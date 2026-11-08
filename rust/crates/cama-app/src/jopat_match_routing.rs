//! Post-match GIF theme selection for JOPA-T debrief callouts.

use crate::jopat_post_match::{JopatPostMatchContext, NumericFact};
use crate::neon_degen::GifAsset;
use crate::post_match_gif_media::render_post_match_gif;

pub const POST_MATCH_GIF_CHANCE: f64 = 0.20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePortError(pub String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PostMatchGifKind {
    BuybackDenied,
    OddsAnomaly,
    BeyondGodlike,
    DivineRapierPosition,
    AncientLiquidated,
}

impl PostMatchGifKind {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::BuybackDenied => "buyback_denied",
            Self::OddsAnomaly => "odds_anomaly",
            Self::BeyondGodlike => "beyond_godlike",
            Self::DivineRapierPosition => "divine_rapier_position",
            Self::AncientLiquidated => "ancient_liquidated",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PostMatchGifSpec {
    pub kind: PostMatchGifKind,
    pub client_name: String,
    pub value: i64,
}

pub trait PostMatchGifPort {
    fn render(&mut self, spec: &PostMatchGifSpec) -> Result<GifAsset, RuntimePortError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ContractGifRenderer;

impl PostMatchGifPort for ContractGifRenderer {
    fn render(&mut self, spec: &PostMatchGifSpec) -> Result<GifAsset, RuntimePortError> {
        render_post_match_gif(&spec.client_name, spec.value, spec.kind.key())
            .map_err(|error| RuntimePortError(error.to_string()))
    }
}

#[must_use]
pub fn post_match_gif_theme(context: &JopatPostMatchContext) -> Option<PostMatchGifSpec> {
    if context.loss >= 500 && context.leverage >= 5 {
        return Some(PostMatchGifSpec {
            kind: PostMatchGifKind::BuybackDenied,
            client_name: context
                .loser_name
                .clone()
                .unwrap_or_else(|| "UNKNOWN CLIENT".to_owned()),
            value: context.loss,
        });
    }
    if let (Some(winner_name), Some(expected_win_prob)) = (
        context.winner_name.as_ref(),
        numeric(context.expected_win_prob.as_ref()),
    ) && expected_win_prob <= 0.30
    {
        return Some(PostMatchGifSpec {
            kind: PostMatchGifKind::OddsAnomaly,
            client_name: winner_name.clone(),
            value: (expected_win_prob * 100.0).round() as i64,
        });
    }
    if context.streak >= 7 {
        return Some(PostMatchGifSpec {
            kind: PostMatchGifKind::BeyondGodlike,
            client_name: context
                .winner_name
                .clone()
                .unwrap_or_else(|| "UNKNOWN CLIENT".to_owned()),
            value: context.streak,
        });
    }
    if let Some(rating_change) = numeric(context.rating_change.as_ref())
        && rating_change >= 40.0
    {
        return Some(PostMatchGifSpec {
            kind: PostMatchGifKind::DivineRapierPosition,
            client_name: context
                .winner_name
                .clone()
                .unwrap_or_else(|| "UNKNOWN CLIENT".to_owned()),
            value: rating_change.round() as i64,
        });
    }
    (context.payout >= 750).then(|| PostMatchGifSpec {
        kind: PostMatchGifKind::AncientLiquidated,
        client_name: context
            .winner_name
            .clone()
            .unwrap_or_else(|| "UNKNOWN CLIENT".to_owned()),
        value: context.payout,
    })
}

fn numeric(value: Option<&NumericFact>) -> Option<f64> {
    value?.as_str().parse().ok()
}

#[cfg(test)]
#[path = "jopat_match_routing/tests.rs"]
mod tests;
