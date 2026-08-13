//! Discord message content limits and natural-boundary chunking.

use std::error::Error;
use std::fmt::{Display, Formatter};

pub const DISCORD_MESSAGE_MAX_CHARS: usize = 2_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidMaxChars;

impl Display for InvalidMaxChars {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("max_chars must be positive")
    }
}

impl Error for InvalidMaxChars {}

/// Split Discord content on newlines, then spaces, then hard character limits.
///
/// Character counts use Unicode scalar values, matching Python's `len(str)` for
/// valid Unicode text instead of Rust's UTF-8 byte length.
pub fn chunk_discord_content(
    content: &str,
    max_chars: usize,
) -> Result<Vec<String>, InvalidMaxChars> {
    if max_chars == 0 {
        return Err(InvalidMaxChars);
    }

    let mut remaining = content.trim().to_owned();
    let mut chunks = Vec::new();

    while remaining.chars().count() > max_chars {
        // Python searches the half-open slice ending at `max_chars + 1`, so a
        // separator exactly on the limit is still a preferred boundary.
        let prefix: String = remaining.chars().take(max_chars + 1).collect();
        let newline = prefix.rfind('\n');
        let space = prefix.rfind(' ');
        let (split_at_chars, separator_chars) = if let Some(byte_index) = newline.filter(|i| *i > 0)
        {
            (remaining[..byte_index].chars().count(), 1)
        } else if let Some(byte_index) = space.filter(|i| *i > 0) {
            (remaining[..byte_index].chars().count(), 1)
        } else {
            (max_chars, 0)
        };

        let split_at_bytes = byte_index_at_char(&remaining, split_at_chars);
        chunks.push(remaining[..split_at_bytes].trim_end().to_owned());

        let rest_start_chars = split_at_chars + separator_chars;
        let rest_start_bytes = byte_index_at_char(&remaining, rest_start_chars);
        remaining = remaining[rest_start_bytes..].trim_start().to_owned();
    }

    if !remaining.is_empty() {
        chunks.push(remaining);
    }
    Ok(chunks)
}

/// Chunk with Discord's normal 2,000-character content limit.
pub fn chunk_default_discord_content(content: &str) -> Vec<String> {
    chunk_discord_content(content, DISCORD_MESSAGE_MAX_CHARS)
        .expect("the Discord content limit is positive")
}

/// One Discord follow-up message emitted for a reliable match announcement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnouncementFollowup {
    /// Message content, already constrained to Discord's character limit.
    pub content: String,
    /// Whether Discord should show the follow-up only to the invoking user.
    pub ephemeral: bool,
}

/// Transport boundary for ordered match-announcement follow-ups.
pub trait AnnouncementSender {
    type Error;

    /// Send one follow-up, completing before the next call begins.
    fn send_followup(&mut self, followup: AnnouncementFollowup) -> Result<(), Self::Error>;
}

/// Send reliable match result details as ordered, public Discord follow-ups.
///
/// Transport errors propagate immediately, matching the sequential `await`
/// behavior of the Python command.  Every emitted follow-up has
/// `ephemeral=false`.
pub fn send_record_announcement<S: AnnouncementSender>(
    sender: &mut S,
    result_message: &str,
) -> Result<(), S::Error> {
    for content in chunk_default_discord_content(result_message) {
        sender.send_followup(AnnouncementFollowup {
            content,
            ephemeral: false,
        })?;
    }
    Ok(())
}

fn byte_index_at_char(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map_or(value.len(), |(byte_index, _)| byte_index)
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use super::{
        AnnouncementFollowup, AnnouncementSender, DISCORD_MESSAGE_MAX_CHARS,
        chunk_default_discord_content, send_record_announcement,
    };

    #[derive(Default)]
    struct RecordingSender {
        sent: Vec<AnnouncementFollowup>,
    }

    impl AnnouncementSender for RecordingSender {
        type Error = Infallible;

        fn send_followup(&mut self, followup: AnnouncementFollowup) -> Result<(), Self::Error> {
            self.sent.push(followup);
            Ok(())
        }
    }

    #[test]
    fn test_chunk_discord_content_prefers_newlines() {
        let first_line = "a".repeat(1_500);
        let second_line = "b".repeat(600);
        let chunks = chunk_default_discord_content(&format!("{first_line}\n{second_line}"));
        assert_eq!(chunks, vec![first_line, second_line]);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= DISCORD_MESSAGE_MAX_CHARS)
        );
    }

    #[test]
    fn test_chunk_discord_content_hard_splits_oversized_line() {
        let content = "x".repeat(DISCORD_MESSAGE_MAX_CHARS * 2 + 1);
        let chunks = chunk_default_discord_content(&content);
        assert_eq!(
            chunks.iter().map(|chunk| chunk.len()).collect::<Vec<_>>(),
            vec![2_000, 2_000, 1]
        );
        assert_eq!(chunks.concat(), content);
    }

    #[test]
    fn test_record_announcement_chunks_the_reliable_result_message() {
        let payouts = (0..45)
            .map(|index| format!("payout-{index}: {}", "x".repeat(90)))
            .collect::<Vec<_>>()
            .join("\n");
        let result_message = format!("✅ Match recorded.\n{payouts}");
        let result_chunks = chunk_default_discord_content(&result_message);
        let mut sender = RecordingSender::default();

        send_record_announcement(&mut sender, &result_message).unwrap();

        let sent = sender
            .sent
            .iter()
            .map(|followup| followup.content.as_str())
            .collect::<Vec<_>>();
        let expected = result_chunks.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(sent, expected);
        assert!(result_chunks.len() > 1);
        assert!(
            sent.iter()
                .all(|content| content.chars().count() <= DISCORD_MESSAGE_MAX_CHARS)
        );
        assert!(sender.sent.iter().all(|followup| !followup.ephemeral));
    }
}
