//! Prediction-market helpers shared by the runtime provider and workers.

use cama_db::predictions_repository::{Book, ContractSide};

/// Mark-to-market price for a position: the best YES bid for YES holders and
/// the complement of the best YES ask for NO holders.
#[must_use]
pub fn position_mark(book: &Book, side: ContractSide) -> Option<i64> {
    match side {
        ContractSide::Yes => book.yes_bids.first().map(|level| level.0),
        ContractSide::No => book.yes_asks.first().map(|level| 100 - level.0),
    }
}

#[must_use]
pub fn neutralize_discord_mentions(question: &str) -> String {
    resolve_and_neutralize_discord_mentions(question, |_| None, |_| None)
}

#[must_use]
pub fn resolve_and_neutralize_discord_mentions<U, R>(
    question: &str,
    user_name: U,
    role_name: R,
) -> String
where
    U: Fn(i64) -> Option<String>,
    R: Fn(i64) -> Option<String>,
{
    let mut resolved = String::with_capacity(question.len());
    let mut cursor = 0;
    while cursor < question.len() {
        let suffix = &question[cursor..];
        let (prefix_len, role) = if suffix.starts_with("<@&") {
            (3, true)
        } else if suffix.starts_with("<@") {
            (2, false)
        } else {
            let character = suffix.chars().next().expect("cursor is in bounds");
            resolved.push(character);
            cursor += character.len_utf8();
            continue;
        };
        let Some(close_offset) = suffix.find('>') else {
            resolved.push_str(suffix);
            break;
        };
        let raw_id = &suffix[prefix_len..close_offset];
        let raw_id = if role {
            raw_id
        } else {
            raw_id.strip_prefix('!').unwrap_or(raw_id)
        };
        let Ok(discord_id) = raw_id.parse::<i64>() else {
            resolved.push_str(&suffix[..=close_offset]);
            cursor += close_offset + 1;
            continue;
        };
        let name = if role {
            role_name(discord_id)
        } else {
            user_name(discord_id)
        }
        .unwrap_or_else(|| raw_id.to_owned());
        resolved.push('@');
        resolved.push_str(&name);
        cursor += close_offset + 1;
    }
    resolved
        .replace("@everyone", "@\u{200b}everyone")
        .replace("@here", "@\u{200b}here")
}

#[cfg(test)]
mod tests {
    use cama_db::predictions_repository::{Book, ContractSide};

    use super::{
        neutralize_discord_mentions, position_mark, resolve_and_neutralize_discord_mentions,
    };

    #[test]
    fn test_position_mark_helper() {
        let book = Book {
            current_price: Some(50),
            yes_asks: vec![(55, 5), (56, 5)],
            yes_bids: vec![(45, 5), (44, 5)],
        };
        assert_eq!(position_mark(&book, ContractSide::Yes), Some(45));
        assert_eq!(position_mark(&book, ContractSide::No), Some(45));
        let one_sided = Book {
            yes_asks: vec![],
            ..book
        };
        assert_eq!(position_mark(&one_sided, ContractSide::No), None);
    }

    #[test]
    fn test_resolve_neutralizes_mentions_in_market_name() {
        let sanitized = resolve_and_neutralize_discord_mentions(
            "Will @everyone and @here back <@123> or <@&456>?",
            |id| (id == 123).then(|| "Player".to_owned()),
            |id| (id == 456).then(|| "Favorites".to_owned()),
        );
        assert_eq!(
            sanitized,
            "Will @\u{200b}everyone and @\u{200b}here back @Player or @Favorites?"
        );
    }

    #[test]
    fn test_neutralize_without_names_keeps_raw_ids() {
        assert_eq!(
            neutralize_discord_mentions("<@!123> <@&456> <@abc> @here"),
            "@123 @456 <@abc> @\u{200b}here"
        );
    }
}
