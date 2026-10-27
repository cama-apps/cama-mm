//! Public Discord embed for an activated economy event, shared by the
//! economy-events worker announcement path.

use cama_domain::formatting::format_grouped;

use crate::economy_event_service::EconomyEventEffects;

#[derive(Clone, Debug, PartialEq)]
pub struct EconomyEvent {
    pub name: Option<String>,
    pub severity: u8,
    pub direction: String,
    pub announcement: Option<String>,
    pub ends_at: Option<i64>,
    pub effects: EconomyEventEffects,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PublicEconomyEventEmbed {
    pub title: String,
    pub description: String,
    pub color: u32,
    pub fields: Vec<(String, String)>,
    pub thumbnail_url: Option<String>,
    pub footer: String,
}

fn roman_level(level: u8) -> &'static str {
    match level {
        1 => "I",
        2 => "II",
        3 => "III",
        4 => "IV",
        5 => "V",
        _ => "I",
    }
}

fn relative_effect(multiplier: f64, lower: &str, higher: &str) -> Option<String> {
    let change = (multiplier - 1.0) * 100.0;
    if change.abs() < 0.05 {
        return None;
    }
    let magnitude = (change.abs() * 10.0).round() / 10.0;
    let formatted = if magnitude.fract() == 0.0 {
        format!("{}%", magnitude as i64)
    } else {
        format!("{magnitude:.1}%")
    };
    Some(format!(
        "**{formatted} {}**",
        if change < 0.0 { lower } else { higher }
    ))
}

fn event_effect_lines(effects: &EconomyEventEffects) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(effect) = relative_effect(effects.reward_multiplier, "lower", "higher") {
        lines.push(format!("🎁 Generated rewards are {effect}."));
    }
    let mut gamba = Vec::new();
    if let Some(effect) = relative_effect(effects.gamba_win_multiplier, "lower", "higher") {
        gamba.push(format!("wins {effect}"));
    }
    if let Some(effect) = relative_effect(effects.gamba_loss_multiplier, "softer", "harsher") {
        gamba.push(format!("losses {effect}"));
    }
    if !gamba.is_empty() {
        lines.push(format!("🎰 Gamba: {}.", gamba.join(", ")));
    }
    if let Some(effect) = relative_effect(effects.bet_payout_multiplier, "lower", "higher") {
        lines.push(format!("⚔️ Match-bet payouts are {effect}."));
    }
    let mut prediction = Vec::new();
    if let Some(effect) = relative_effect(effects.prediction_payout_multiplier, "lower", "higher") {
        prediction.push(format!("resolution payouts {effect}"));
    }
    if let Some(effect) = relative_effect(effects.prediction_depth_multiplier, "thinner", "deeper")
    {
        prediction.push(format!("liquidity {effect}"));
    }
    if effects.prediction_spread_ticks_delta != 0 {
        prediction.push(format!(
            "spreads **{} ticks {}**",
            effects.prediction_spread_ticks_delta.abs(),
            if effects.prediction_spread_ticks_delta > 0 {
                "wider"
            } else {
                "tighter"
            }
        ));
    }
    if !prediction.is_empty() {
        lines.push(format!("📈 Prediction markets: {}.", prediction.join(", ")));
    }
    if effects.reserve_voting_disabled {
        lines.push("🏛️ Jopacoin Reserve allocation voting is suspended.".to_owned());
    }
    if lines.is_empty() {
        lines.push("⚖️ Payouts and market conditions remain at their normal strength.".to_owned());
    }
    lines
}

fn immediate_effect_text(effects: &EconomyEventEffects) -> String {
    let mut actions = Vec::new();
    if effects.reserve_burn_jc != 0 {
        actions.push(format!(
            "**{} JC** burned from the Jopa Reserve",
            format_grouped(effects.reserve_burn_jc)
        ));
    }
    if effects.wallet_burn_jc != 0 {
        actions.push(format!(
            "**{} JC** burned from positive wallets",
            format_grouped(effects.wallet_burn_jc)
        ));
    }
    if effects.reserve_release_jc != 0 {
        actions.push(format!(
            "**{} JC** redistributed from the Jopa Reserve",
            format_grouped(effects.reserve_release_jc)
        ));
    }
    if actions.is_empty() {
        concat!(
            "No JC moved when this spell activated. Its pressure arrives through ",
            "adjusted payouts and liquidity as activity settles."
        )
        .to_owned()
    } else {
        format!("The spell took immediate hold: {}.", actions.join("; "))
    }
}

#[must_use]
pub fn build_public_economy_event_embed(
    event: &EconomyEvent,
    icon_url: Option<&str>,
) -> PublicEconomyEventEmbed {
    let name = event.name.as_deref().unwrap_or("Economic Event");
    let (icon, color, edict) = match event.direction.as_str() {
        "deflationary" => ("🌑", 0xD9_4B_4B, "A deflationary edict grips the server."),
        "boon" => ("✨", 0x43_B5_81, "A boon washes over the server."),
        _ => (
            "🕰️",
            0x7F_8C_8D,
            "The market bends without changing course.",
        ),
    };
    let announcement = event.announcement.as_deref().unwrap_or("").trim();
    let flavor = announcement
        .lines()
        .next()
        .filter(|line| !line.is_empty())
        .unwrap_or(edict);
    let expiry = event.ends_at.map_or_else(
        || "Active until the next 10 AM Pacific rollover.".to_owned(),
        |ends_at| format!("Active until <t:{ends_at}:R>."),
    );
    PublicEconomyEventEmbed {
        title: format!("{icon} {name} — Level {}", roman_level(event.severity)),
        description: format!("*{flavor}*\n\n**{edict}** {expiry}"),
        color,
        fields: vec![
            (
                "The Spell Takes Effect".to_owned(),
                event_effect_lines(&event.effects).join("\n"),
            ),
            (
                "Immediate Impact".to_owned(),
                immediate_effect_text(&event.effects),
            ),
        ],
        thumbnail_url: icon_url.map(str::to_owned),
        footer: concat!(
            "The treasury watches. Bands follow the rolling 7-day change ",
            "around a 2% annual target."
        )
        .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ravage_event() -> EconomyEvent {
        EconomyEvent {
            name: Some("Ravage".to_owned()),
            severity: 3,
            direction: "deflationary".to_owned(),
            announcement: Some("A tidal shock tears through the Jopacoin economy.".to_owned()),
            ends_at: Some(1_752_943_600),
            effects: EconomyEventEffects {
                reward_multiplier: 0.76,
                reserve_burn_jc: 300,
                ..EconomyEventEffects::default()
            },
        }
    }

    #[test]
    fn test_economy_event_announcement_uses_shared_public_embed() {
        let embed = build_public_economy_event_embed(
            &ravage_event(),
            Some("https://cdn.example/ravage.png"),
        );
        assert_eq!(embed.title, "🌑 Ravage — Level III");
        assert!(embed.fields[0].1.contains("24% lower"));
        assert!(embed.fields[1].1.contains("300 JC"));
        assert_eq!(
            embed.thumbnail_url.as_deref(),
            Some("https://cdn.example/ravage.png")
        );
        assert_eq!(
            embed.footer,
            "The treasury watches. Bands follow the rolling 7-day change around a 2% annual target."
        );
    }
}
