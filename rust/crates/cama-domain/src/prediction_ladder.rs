//! Pure prediction-market ladder and digest rendering.
//!
//! The Python command layer receives dictionaries from persistence.  This
//! module keeps Discord and repository I/O outside the domain boundary by
//! accepting typed snapshots while preserving the deployed strings, ordering,
//! truncation, and biggest-mover selection rules.

/// Discord embed fields are limited to 1,024 Unicode characters.
pub const DISCORD_EMBED_FIELD_MAX_CHARS: usize = 1_024;

/// The number of visible order-book levels on each side of the midpoint.
pub const LADDER_MAX_ROWS_PER_SIDE: usize = 8;

const BAR_CAP: usize = 10;
const GREEN: &str = "\u{1b}[0;32m";
const RED: &str = "\u{1b}[0;31m";
const RESET: &str = "\u{1b}[0;0m";

/// One resting YES order-book level.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderLevel {
    pub price: i32,
    pub size: u32,
}

impl OrderLevel {
    #[must_use]
    pub const fn new(price: i32, size: u32) -> Self {
        Self { price, size }
    }
}

/// Read-only order-book state needed to render the buy-only ladder.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OrderBookSnapshot {
    /// The market's fair belief. It is deliberately not displayed in the book.
    pub current_price: Option<i32>,
    /// Cheapest YES ask first.
    pub yes_asks: Vec<OrderLevel>,
    /// Highest YES bid first, which mirrors to cheapest NO first.
    pub yes_bids: Vec<OrderLevel>,
}

/// A transport-neutral Discord embed field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

/// Read-only market state used by links, list fields, and mover selection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarketSnapshot {
    pub prediction_id: Option<i64>,
    pub question: Option<String>,
    pub current_price: Option<i32>,
    pub prev_price: Option<i32>,
    pub guild_id: Option<u64>,
    pub thread_id: Option<u64>,
    pub embed_message_id: Option<u64>,
}

fn ladder_row(level: OrderLevel, displayed_price: i32) -> String {
    let bar = "█".repeat((level.size as usize / 10).min(BAR_CAP));
    format!("  {displayed_price:>3}  {bar:<BAR_CAP$}  {:>3}", level.size)
}

/// Render the deployed, buy-only depth-of-market field.
///
/// YES asks sit above the midpoint from deepest to cheapest. YES bids mirror
/// into NO prices below it from cheapest to deepest. Only the eight nearest
/// levels per side render, while a marker discloses hidden sweep depth.
#[must_use]
pub fn build_ladder_fields(book: &OrderBookSnapshot) -> Vec<EmbedField> {
    let hidden_asks = book.yes_asks.len().saturating_sub(LADDER_MAX_ROWS_PER_SIDE);
    let hidden_bids = book.yes_bids.len().saturating_sub(LADDER_MAX_ROWS_PER_SIDE);
    let worst_hidden_yes = (hidden_asks > 0)
        .then(|| book.yes_asks.last().map(|level| level.price))
        .flatten();
    let worst_hidden_no = (hidden_bids > 0)
        .then(|| book.yes_bids.last().map(|level| 100 - level.price))
        .flatten();
    let asks = &book.yes_asks[..book.yes_asks.len().min(LADDER_MAX_ROWS_PER_SIDE)];
    let bids = &book.yes_bids[..book.yes_bids.len().min(LADDER_MAX_ROWS_PER_SIDE)];

    let mut lines = vec![format!("{GREEN}🟢 Buy YES{RESET}")];
    if asks.is_empty() {
        lines.push("  (none — refreshes daily)".to_owned());
    } else {
        if let Some(price) = worst_hidden_yes {
            lines.push(format!("  (+{hidden_asks} deeper to {price})"));
        }
        lines.extend(
            asks.iter()
                .rev()
                .map(|level| format!("{GREEN}{}{RESET}", ladder_row(*level, level.price))),
        );
    }

    let cheapest_yes = asks.first().map(|level| level.price);
    let cheapest_no = bids.first().map(|level| 100 - level.price);
    lines.push(String::new());
    if cheapest_yes.is_none() && cheapest_no.is_none() && book.current_price.is_none() {
        lines.push("  ── ? ──".to_owned());
    } else {
        let yes = cheapest_yes.map_or_else(|| "YES —".to_owned(), |price| format!("YES {price}%"));
        let no = cheapest_no.map_or_else(|| "NO —".to_owned(), |price| format!("NO {price}%"));
        lines.push(format!("  ── {yes}  ·  {no} ──"));
    }
    lines.push(String::new());

    lines.push(format!("{RED}🔴 Buy NO{RESET}"));
    if bids.is_empty() {
        lines.push("  (none — refreshes daily)".to_owned());
    } else {
        lines.extend(
            bids.iter()
                .map(|level| format!("{RED}{}{RESET}", ladder_row(*level, 100 - level.price))),
        );
        if let Some(price) = worst_hidden_no {
            lines.push(format!("  (+{hidden_bids} deeper to {price})"));
        }
    }

    vec![EmbedField {
        name: "Order book".to_owned(),
        value: format!("```ansi\n{}\n```", lines.join("\n")),
        inline: false,
    }]
}

/// Build a Discord deep link to a market's trade controls.
///
/// A missing message falls back to its thread; a missing guild or thread
/// means there is no usable link.
#[must_use]
pub fn trade_link(market: &MarketSnapshot) -> Option<String> {
    let guild_id = market.guild_id?;
    let thread_id = market.thread_id?;
    let base = format!("https://discord.com/channels/{guild_id}/{thread_id}");
    Some(
        market
            .embed_message_id
            .map_or(base.clone(), |message_id| format!("{base}/{message_id}")),
    )
}

fn delta_phrase(current: i32, previous: i32) -> String {
    let arrow = if current > previous { '↑' } else { '↓' };
    let magnitude = (i64::from(current) - i64::from(previous)).abs();
    format!("{arrow}{magnitude} today")
}

fn truncate_question(question: &str) -> String {
    let trimmed = question.trim();
    if trimmed.chars().count() <= 200 {
        return trimmed.to_owned();
    }
    let prefix: String = trimmed.chars().take(197).collect();
    format!("{prefix}...")
}

/// Build the exact `(name, value)` Discord field used by market lists/digests.
#[must_use]
pub fn format_market_field(market: &MarketSnapshot, with_delta: bool) -> (String, String) {
    let prediction_id = market
        .prediction_id
        .map_or_else(|| "?".to_owned(), |value| value.to_string());
    let price = market
        .current_price
        .map_or_else(|| "?".to_owned(), |value| format!("{value}%"));

    let delta = if with_delta {
        match (market.current_price, market.prev_price) {
            (Some(current), Some(previous)) if current != previous => {
                format!("  ({})", delta_phrase(current, previous))
            }
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let name = format!("📈 #{prediction_id}  ·  YES {price}{delta}");

    let question = market
        .question
        .as_deref()
        .map(truncate_question)
        .unwrap_or_default();
    let quoted = if question.is_empty() {
        String::new()
    } else {
        format!("\"{question}\"")
    };
    let link_line =
        trade_link(market).map_or_else(String::new, |link| format!("\n[Trade →]({link})"));
    let value = format!("{quoted}{link_line}");
    let value = value.trim();
    (
        name,
        if value.is_empty() {
            "—".to_owned()
        } else {
            value.to_owned()
        },
    )
}

/// Choose the first market with the greatest non-zero absolute one-day move.
///
/// Markets without either price are skipped. Equal swings preserve input
/// order, matching Python's stable `max` behavior.
#[must_use]
pub fn pick_biggest_mover(markets: &[MarketSnapshot]) -> Option<&MarketSnapshot> {
    let mut best = None;
    let mut best_magnitude = 0_i64;
    for market in markets {
        let (Some(current), Some(previous)) = (market.current_price, market.prev_price) else {
            continue;
        };
        let magnitude = (i64::from(current) - i64::from(previous)).abs();
        if magnitude > best_magnitude {
            best = Some(market);
            best_magnitude = magnitude;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::{
        DISCORD_EMBED_FIELD_MAX_CHARS, MarketSnapshot, OrderBookSnapshot, OrderLevel,
        build_ladder_fields, format_market_field, pick_biggest_mover, trade_link,
    };

    fn level(price: i32, size: u32) -> OrderLevel {
        OrderLevel::new(price, size)
    }

    fn symmetric_book() -> OrderBookSnapshot {
        OrderBookSnapshot {
            current_price: Some(50),
            yes_asks: vec![level(51, 5)],
            yes_bids: vec![level(49, 5)],
        }
    }

    fn asymmetric_book() -> OrderBookSnapshot {
        OrderBookSnapshot {
            current_price: Some(17),
            yes_asks: vec![level(18, 5), level(19, 5), level(20, 5)],
            yes_bids: vec![level(16, 5), level(15, 5), level(14, 5)],
        }
    }

    fn linked_market(prediction_id: i64, question: &str, price: i32) -> MarketSnapshot {
        MarketSnapshot {
            prediction_id: Some(prediction_id),
            question: Some(question.to_owned()),
            current_price: Some(price),
            guild_id: Some(1),
            thread_id: Some(2),
            embed_message_id: Some(3),
            ..MarketSnapshot::default()
        }
    }

    fn mover(prediction_id: i64, current: i32, previous: i32) -> MarketSnapshot {
        MarketSnapshot {
            prediction_id: Some(prediction_id),
            current_price: Some(current),
            prev_price: Some(previous),
            ..MarketSnapshot::default()
        }
    }

    #[test]
    fn test_ladder_dom_style_yes_above_no_converging_at_mid() {
        let fields = build_ladder_fields(&asymmetric_book());
        assert_eq!(fields.len(), 1);
        let field = &fields[0];
        assert_eq!(field.name, "Order book");
        assert!(!field.inline);
        let value = &field.value;
        let yes_index = value.find("Buy YES").unwrap();
        let mid_index = value.find("YES 18%").unwrap();
        let no_index = value.find("Buy NO").unwrap();
        assert!(yes_index < mid_index && mid_index < no_index);
        let yes_section = &value[yes_index..mid_index];
        assert!(
            yes_section.find("  20  ").unwrap() < yes_section.find("  19  ").unwrap()
                && yes_section.find("  19  ").unwrap() < yes_section.find("  18  ").unwrap()
        );
        let no_section = &value[no_index..];
        assert!(
            no_section.find("  84  ").unwrap() < no_section.find("  85  ").unwrap()
                && no_section.find("  85  ").unwrap() < no_section.find("  86  ").unwrap()
        );
        assert!(value.contains("YES 18%") && value.contains("NO 84%"));
        assert!(!value.contains("Sell"));
        assert!(!value.contains("<- top"));
        assert!(!value.contains("ASK") && !value.contains("BID"));
        assert!(!value.contains("price 17"));
    }

    #[test]
    fn test_ladder_dom_order_is_yes_above_no_for_symmetric_market() {
        let value = &build_ladder_fields(&symmetric_book())[0].value;
        assert!(
            value.find("Buy YES").unwrap() < value.find("YES 51%").unwrap()
                && value.find("YES 51%").unwrap() < value.find("Buy NO").unwrap()
        );
    }

    #[test]
    fn test_ladder_color_codes_yes_green_no_red() {
        let value = &build_ladder_fields(&symmetric_book())[0].value;
        assert!(value.starts_with("```ansi\n"));
        assert!(value.contains("🟢 Buy YES"));
        assert!(value.contains("🔴 Buy NO"));
        assert!(value.contains("\u{1b}[0;32m"));
        assert!(value.contains("\u{1b}[0;31m"));
        assert!(value.contains("\u{1b}[0;0m"));
    }

    #[test]
    fn test_ladder_asymmetric_market_mirror_prices() {
        let value = &build_ladder_fields(&asymmetric_book())[0].value;
        for price in [18, 19, 20] {
            assert!(value.contains(&price.to_string()));
        }
        for price in [84, 85, 86] {
            assert!(value.contains(&price.to_string()));
        }
        assert!(value.contains("YES 18%"));
        assert!(value.contains("NO 84%"));
    }

    #[test]
    fn test_ladder_depth_bars_scale_per_ten_size() {
        let book = OrderBookSnapshot {
            current_price: Some(50),
            yes_asks: vec![level(60, 3), level(61, 25), level(62, 80), level(63, 250)],
            yes_bids: Vec::new(),
        };
        let value = &build_ladder_fields(&book)[0].value;
        let row_for = |price: i32| {
            let pattern = format!("  {price:>3}  ");
            value.lines().find(|row| row.contains(&pattern)).unwrap()
        };
        assert!(!row_for(60).contains('█'));
        assert!(row_for(61).contains("██") && !row_for(61).contains("███"));
        assert!(row_for(62).contains("████████") && !row_for(62).contains("█████████"));
        assert!(row_for(63).contains("██████████") && !row_for(63).contains("███████████"));
    }

    #[test]
    fn test_ladder_empty_book() {
        let book = OrderBookSnapshot {
            current_price: Some(50),
            ..OrderBookSnapshot::default()
        };
        let value = &build_ladder_fields(&book)[0].value;
        assert_eq!(value.matches("(none — refreshes daily)").count(), 2);
        assert!(value.contains("YES —") && value.contains("NO —"));
        assert!(!value.contains("price 50"));
    }

    #[test]
    fn test_ladder_only_asks_keeps_no_section_empty() {
        let book = OrderBookSnapshot {
            current_price: Some(50),
            yes_asks: vec![level(51, 5)],
            yes_bids: Vec::new(),
        };
        let value = &build_ladder_fields(&book)[0].value;
        assert!(value.contains("51"));
        assert!(value.contains("(none — refreshes daily)"));
        assert!(value.contains("YES 51%") && value.contains("NO —"));
    }

    #[test]
    fn test_ladder_handles_missing_current_price() {
        let value = &build_ladder_fields(&OrderBookSnapshot::default())[0].value;
        assert!(value.contains('?'));
        assert!(!value.contains("price"));
    }

    #[test]
    fn test_ladder_caps_rows_and_stays_under_embed_field_limit() {
        let book = OrderBookSnapshot {
            current_price: Some(50),
            yes_asks: (0..30).map(|index| level(51 + index, 10)).collect(),
            yes_bids: (0..30).map(|index| level(49 - index, 10)).collect(),
        };
        let value = &build_ladder_fields(&book)[0].value;
        assert!(value.chars().count() <= DISCORD_EMBED_FIELD_MAX_CHARS);
        assert!(value.contains("  51  "));
        assert!(!value.contains("  80  "));
        assert_eq!(value.matches("(+22 deeper to 80)").count(), 2);
        assert!(value.contains("YES 51%") && value.contains("NO 51%"));
    }

    #[test]
    fn test_ladder_no_truncation_marker_when_book_fits() {
        let book = OrderBookSnapshot {
            current_price: Some(50),
            yes_asks: (0..8).map(|index| level(51 + index, 10)).collect(),
            yes_bids: (0..8).map(|index| level(49 - index, 10)).collect(),
        };
        let value = &build_ladder_fields(&book)[0].value;
        assert!(!value.contains("deeper"));
        assert!(value.contains("  58  "));
    }

    #[test]
    fn test_trade_link_with_full_ids_lands_on_message() {
        let market = linked_market(5, "x?", 50);
        assert_eq!(
            trade_link(&market).as_deref(),
            Some("https://discord.com/channels/1/2/3")
        );
    }

    #[test]
    fn test_trade_link_falls_back_to_thread_when_message_missing() {
        let mut market = linked_market(5, "x?", 50);
        market.embed_message_id = None;
        assert_eq!(
            trade_link(&market).as_deref(),
            Some("https://discord.com/channels/1/2")
        );
    }

    #[test]
    fn test_trade_link_returns_none_when_thread_missing() {
        let mut market = linked_market(5, "x?", 50);
        market.thread_id = None;
        assert_eq!(trade_link(&market), None);
    }

    #[test]
    fn test_format_market_field_basic() {
        let market = linked_market(5, "Will @Luke hit immortal?", 17);
        let (name, value) = format_market_field(&market, false);
        assert!(name.contains("📈 #5"));
        assert!(name.contains("YES 17%"));
        assert!(!name.contains("price"));
        assert!(value.contains("@Luke"));
        assert!(value.contains("[Trade →](https://discord.com/channels/1/2/3)"));
    }

    #[test]
    fn test_format_market_field_with_delta_up() {
        let mut market = linked_market(5, "x?", 19);
        market.prev_price = Some(17);
        let (name, _) = format_market_field(&market, true);
        assert!(name.contains("YES 19%"));
        assert!(name.contains("↑2 today"));
    }

    #[test]
    fn test_format_market_field_with_delta_down() {
        let mut market = linked_market(6, "y?", 50);
        market.prev_price = Some(53);
        let (name, _) = format_market_field(&market, true);
        assert!(name.contains("YES 50%"));
        assert!(name.contains("↓3 today"));
    }

    #[test]
    fn test_format_market_field_with_delta_no_movement_omits_arrow() {
        let mut market = linked_market(7, "z?", 50);
        market.prev_price = Some(50);
        let (name, _) = format_market_field(&market, true);
        assert!(!name.contains('↑') && !name.contains('↓'));
    }

    #[test]
    fn test_format_market_field_omits_link_when_thread_missing() {
        let market = MarketSnapshot {
            prediction_id: Some(8),
            question: Some("no thread yet".to_owned()),
            current_price: Some(50),
            guild_id: Some(1),
            ..MarketSnapshot::default()
        };
        let (_, value) = format_market_field(&market, false);
        assert!(!value.contains("Trade"));
    }

    #[test]
    fn test_pick_biggest_mover_returns_largest_absolute_swing() {
        let markets = vec![mover(1, 52, 50), mover(2, 30, 38), mover(3, 60, 55)];
        assert_eq!(pick_biggest_mover(&markets).unwrap().prediction_id, Some(2));
    }

    #[test]
    fn test_pick_biggest_mover_counts_down_moves_by_magnitude() {
        let markets = vec![mover(1, 55, 50), mover(2, 40, 49)];
        assert_eq!(pick_biggest_mover(&markets).unwrap().prediction_id, Some(2));
    }

    #[test]
    fn test_pick_biggest_mover_skips_markets_without_prev_price() {
        let markets = vec![
            MarketSnapshot {
                prediction_id: Some(1),
                current_price: Some(90),
                ..MarketSnapshot::default()
            },
            mover(2, 52, 50),
        ];
        assert_eq!(pick_biggest_mover(&markets).unwrap().prediction_id, Some(2));
    }

    #[test]
    fn test_pick_biggest_mover_returns_none_when_nothing_moved() {
        let markets = vec![mover(1, 50, 50), mover(2, 17, 17)];
        assert_eq!(pick_biggest_mover(&markets), None);
    }

    #[test]
    fn test_pick_biggest_mover_returns_none_for_empty_list() {
        assert_eq!(pick_biggest_mover(&[]), None);
    }
}
