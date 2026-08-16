//! Discord embed-size validation and lossless line packing.

pub const TITLE_LIMIT: usize = 256;
pub const AUTHOR_NAME_LIMIT: usize = 256;
pub const FIELD_VALUE_LIMIT: usize = 1_024;
pub const FIELD_NAME_LIMIT: usize = 256;
pub const DESCRIPTION_LIMIT: usize = 4_096;
pub const FOOTER_LIMIT: usize = 2_048;
pub const TOTAL_LIMIT: usize = 6_000;
pub const MAX_FIELDS: usize = 25;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmbedModel {
    pub title: Option<String>,
    pub description: Option<String>,
    pub fields: Vec<EmbedField>,
    pub footer: Option<String>,
    pub author: Option<String>,
}

impl EmbedModel {
    pub fn add_field(&mut self, name: impl Into<String>, value: impl Into<String>, inline: bool) {
        self.fields.push(EmbedField {
            name: name.into(),
            value: value.into(),
            inline,
        });
    }
}

fn character_count(value: &str) -> usize {
    value.chars().count()
}

/// Truncate by Unicode scalar values, matching Python string length/slicing for
/// the Discord-facing text exercised by the application.
#[must_use]
pub fn truncate_field(text: &str, max_len: usize) -> String {
    if character_count(text) <= max_len {
        return text.to_owned();
    }

    let prefix_len = max_len.saturating_sub(3);
    let mut truncated = text.chars().take(prefix_len).collect::<String>();
    truncated.push_str("...");
    truncated
}

/// Add newline-joined lines, splitting only between lines and preserving their
/// order. A single overlong line is truncated rather than emitted invalidly.
pub fn add_lines_field(
    embed: &mut EmbedModel,
    name: &str,
    lines: &[String],
    inline: bool,
    max_len: usize,
) {
    if lines.is_empty() {
        return;
    }

    let mut chunks = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut length = 0;
    for line in lines {
        let line_len = character_count(line);
        let extra = line_len + usize::from(!current.is_empty());
        if !current.is_empty() && length + extra > max_len {
            chunks.push(current.join("\n"));
            current = vec![line];
            length = line_len;
        } else {
            current.push(line);
            length += extra;
        }
    }
    chunks.push(current.join("\n"));

    for (index, chunk) in chunks.into_iter().enumerate() {
        embed.add_field(
            if index == 0 { name } else { "\u{200b}" },
            truncate_field(&chunk, max_len),
            inline,
        );
    }
}

/// Return every Discord embed-limit violation in Python-compatible order.
#[must_use]
pub fn validate_embed(embed: &EmbedModel) -> Vec<String> {
    let mut errors = Vec::new();

    if let Some(title) = embed.title.as_deref()
        && character_count(title) > TITLE_LIMIT
    {
        errors.push(format!(
            "Title exceeds {TITLE_LIMIT} chars ({})",
            character_count(title)
        ));
    }
    if let Some(description) = embed.description.as_deref()
        && character_count(description) > DESCRIPTION_LIMIT
    {
        errors.push(format!(
            "Description exceeds {DESCRIPTION_LIMIT} chars ({})",
            character_count(description)
        ));
    }

    for (index, field) in embed.fields.iter().enumerate() {
        if character_count(&field.name) > FIELD_NAME_LIMIT {
            let preview = field.name.chars().take(20).collect::<String>();
            errors.push(format!(
                "Field {index} name '{preview}...' exceeds {FIELD_NAME_LIMIT} chars"
            ));
        }
        if character_count(&field.value) > FIELD_VALUE_LIMIT {
            errors.push(format!(
                "Field {index} '{}' value exceeds {FIELD_VALUE_LIMIT} chars ({})",
                field.name,
                character_count(&field.value)
            ));
        }
    }

    if let Some(footer) = embed.footer.as_deref()
        && character_count(footer) > FOOTER_LIMIT
    {
        errors.push(format!(
            "Footer exceeds {FOOTER_LIMIT} chars ({})",
            character_count(footer)
        ));
    }
    if let Some(author) = embed.author.as_deref()
        && character_count(author) > AUTHOR_NAME_LIMIT
    {
        errors.push(format!(
            "Author name exceeds {AUTHOR_NAME_LIMIT} chars ({})",
            character_count(author)
        ));
    }
    if embed.fields.len() > MAX_FIELDS {
        errors.push(format!(
            "Too many fields: {} > {MAX_FIELDS}",
            embed.fields.len()
        ));
    }

    let total = embed.title.as_deref().map_or(0, character_count)
        + embed.description.as_deref().map_or(0, character_count)
        + embed.footer.as_deref().map_or(0, character_count)
        + embed.author.as_deref().map_or(0, character_count)
        + embed
            .fields
            .iter()
            .map(|field| character_count(&field.name) + character_count(&field.value))
            .sum::<usize>();
    if total > TOTAL_LIMIT {
        errors.push(format!(
            "Total embed size exceeds {TOTAL_LIMIT} chars ({total})"
        ));
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repeated(character: char, count: usize) -> String {
        std::iter::repeat_n(character, count).collect()
    }

    #[test]
    fn test_short_text_unchanged() {
        assert_eq!(
            truncate_field("Short text", FIELD_VALUE_LIMIT),
            "Short text"
        );
    }

    #[test]
    fn test_exact_limit_unchanged() {
        let text = repeated('x', FIELD_VALUE_LIMIT);
        assert_eq!(truncate_field(&text, FIELD_VALUE_LIMIT), text);
    }

    #[test]
    fn test_over_limit_truncated() {
        let result = truncate_field(&repeated('x', 1_100), FIELD_VALUE_LIMIT);
        assert_eq!(character_count(&result), FIELD_VALUE_LIMIT);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_custom_limit() {
        let result = truncate_field(&repeated('x', 500), 100);
        assert_eq!(character_count(&result), 100);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_empty_text() {
        assert_eq!(truncate_field("", FIELD_VALUE_LIMIT), "");
    }

    #[test]
    fn test_truncation_preserves_content() {
        let text = format!("Hello World{}", repeated('x', 1_100));
        assert!(truncate_field(&text, FIELD_VALUE_LIMIT).starts_with("Hello World"));
    }

    #[test]
    fn test_valid_embed_no_errors() {
        let mut embed = EmbedModel {
            title: Some("Short title".to_owned()),
            description: Some("Short description".to_owned()),
            ..EmbedModel::default()
        };
        embed.add_field("Field", "Value", false);
        assert!(validate_embed(&embed).is_empty());
    }

    #[test]
    fn test_title_too_long() {
        let embed = EmbedModel {
            title: Some(repeated('x', TITLE_LIMIT + 1)),
            ..EmbedModel::default()
        };
        let errors = validate_embed(&embed);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Title"));
    }

    #[test]
    fn test_description_too_long() {
        let embed = EmbedModel {
            description: Some(repeated('x', DESCRIPTION_LIMIT + 1)),
            ..EmbedModel::default()
        };
        let errors = validate_embed(&embed);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Description"));
    }

    #[test]
    fn test_field_value_too_long() {
        let mut embed = EmbedModel::default();
        embed.add_field("Test Field", repeated('x', FIELD_VALUE_LIMIT + 1), false);
        let errors = validate_embed(&embed);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Field 0"));
        assert!(errors[0].contains("Test Field"));
    }

    #[test]
    fn test_field_name_too_long() {
        let mut embed = EmbedModel::default();
        embed.add_field(repeated('x', FIELD_NAME_LIMIT + 1), "Value", false);
        let errors = validate_embed(&embed);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("name"));
    }

    #[test]
    fn test_footer_too_long() {
        let embed = EmbedModel {
            footer: Some(repeated('x', FOOTER_LIMIT + 1)),
            ..EmbedModel::default()
        };
        let errors = validate_embed(&embed);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Footer"));
    }

    #[test]
    fn test_too_many_fields() {
        let mut embed = EmbedModel::default();
        for index in 0..=MAX_FIELDS {
            embed.add_field(format!("Field {index}"), "Value", false);
        }
        let errors = validate_embed(&embed);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Too many fields"));
    }

    #[test]
    fn test_multiple_errors() {
        let mut embed = EmbedModel {
            description: Some(repeated('x', DESCRIPTION_LIMIT + 1)),
            ..EmbedModel::default()
        };
        embed.add_field("Test", repeated('x', FIELD_VALUE_LIMIT + 1), false);
        assert_eq!(validate_embed(&embed).len(), 2);
    }

    #[test]
    fn test_author_name_too_long() {
        let embed = EmbedModel {
            author: Some(repeated('x', AUTHOR_NAME_LIMIT + 1)),
            ..EmbedModel::default()
        };
        let errors = validate_embed(&embed);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Author"));
    }

    #[test]
    fn test_total_size_too_large() {
        let mut embed = EmbedModel {
            description: Some(repeated('x', 4_000)),
            ..EmbedModel::default()
        };
        embed.add_field("a", repeated('y', FIELD_VALUE_LIMIT), false);
        embed.add_field("b", repeated('z', FIELD_VALUE_LIMIT), false);
        let errors = validate_embed(&embed);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Total"));
    }

    #[test]
    fn test_empty_lines_adds_no_field() {
        let mut embed = EmbedModel::default();
        add_lines_field(&mut embed, "Empty", &[], false, FIELD_VALUE_LIMIT);
        assert!(embed.fields.is_empty());
    }

    #[test]
    fn test_short_list_single_field() {
        let mut embed = EmbedModel::default();
        let lines = ["alpha", "beta", "gamma"].map(str::to_owned);
        add_lines_field(&mut embed, "Greek", &lines, false, FIELD_VALUE_LIMIT);
        assert_eq!(embed.fields.len(), 1);
        assert_eq!(embed.fields[0].name, "Greek");
        assert_eq!(embed.fields[0].value, "alpha\nbeta\ngamma");
    }

    #[test]
    fn test_long_list_splits_without_dropping_lines() {
        let lines = (0..40)
            .map(|index| format!("item-{index:03}-{}", repeated('x', 50)))
            .collect::<Vec<_>>();
        let mut embed = EmbedModel::default();
        add_lines_field(&mut embed, "Stuff", &lines, false, FIELD_VALUE_LIMIT);
        assert!(embed.fields.len() >= 2);
        assert!(
            embed
                .fields
                .iter()
                .all(|field| character_count(&field.value) <= FIELD_VALUE_LIMIT)
        );
        assert_eq!(embed.fields[0].name, "Stuff");
        assert!(
            embed
                .fields
                .iter()
                .skip(1)
                .all(|field| field.name == "\u{200b}")
        );
        let recovered = embed
            .fields
            .iter()
            .flat_map(|field| field.value.split('\n'))
            .collect::<Vec<_>>();
        assert_eq!(
            recovered,
            lines.iter().map(String::as_str).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_single_overlong_line_is_truncated() {
        let mut embed = EmbedModel::default();
        add_lines_field(
            &mut embed,
            "Big",
            &[repeated('y', 2_000)],
            false,
            FIELD_VALUE_LIMIT,
        );
        assert_eq!(embed.fields.len(), 1);
        assert!(character_count(&embed.fields[0].value) <= FIELD_VALUE_LIMIT);
        assert!(embed.fields[0].value.ends_with("..."));
    }

    #[test]
    fn unicode_limits_count_characters_not_utf8_bytes() {
        let text = repeated('🦙', FIELD_VALUE_LIMIT + 1);
        let truncated = truncate_field(&text, FIELD_VALUE_LIMIT);
        assert_eq!(character_count(&truncated), FIELD_VALUE_LIMIT);
        assert!(truncated.is_char_boundary(truncated.len()));
    }
}
