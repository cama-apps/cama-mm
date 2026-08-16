use super::*;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
enum ChannelMode {
    Send,
    Missing,
    SynchronousFailure,
}

#[derive(Clone)]
struct RecordingChannel {
    sent: Arc<Mutex<Vec<String>>>,
    mode: ChannelMode,
}

impl RecordingChannel {
    fn new(mode: ChannelMode) -> (Self, Arc<Mutex<Vec<String>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                sent: Arc::clone(&sent),
                mode,
            },
            sent,
        )
    }
}

impl AtmosphericChannel for RecordingChannel {
    fn send(&self, content: String) -> Result<Option<FlameSendFuture>, FlameSendError> {
        match self.mode {
            ChannelMode::Send => {
                self.sent
                    .lock()
                    .expect("recording channel lock")
                    .push(content);
                Ok(Some(Box::pin(async { Ok(()) })))
            }
            ChannelMode::Missing => Ok(None),
            ChannelMode::SynchronousFailure => Err(FlameSendError),
        }
    }
}

#[derive(Default)]
struct RecordingSpawner {
    tasks: Mutex<Vec<FlameSendFuture>>,
}

impl AtmosphericTaskSpawner for RecordingSpawner {
    fn spawn(&self, task: FlameSendFuture) -> Result<(), FlameTaskError> {
        self.tasks
            .lock()
            .expect("recording spawner lock")
            .push(task);
        Ok(())
    }
}

fn sent_messages(sent: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    sent.lock().expect("recorded message lock").clone()
}

#[test]
fn pool_is_non_empty() {
    assert!(!CATASTROPHIC_LINES.is_empty());
}

#[test]
fn lines_are_non_blank_strings() {
    for line in CATASTROPHIC_LINES {
        assert!(!line.trim().is_empty());
    }
}

#[test]
fn lines_are_unique() {
    let mut unique = CATASTROPHIC_LINES.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), CATASTROPHIC_LINES.len());
}

#[test]
fn lines_contain_no_proper_nouns_marker() {
    let banned = ["luminosity", "prestige", "jc", "jopacoin", "cave-in chance"];
    for line in CATASTROPHIC_LINES {
        let lowered = line.to_ascii_lowercase();
        for term in banned {
            assert!(!lowered.contains(term), "{term:?} leaked in {line:?}");
        }
    }
}

#[test]
fn pick_returns_member_of_pool() {
    let mut entropy = SplitMix64Entropy::seeded(0);
    for _ in 0..100 {
        assert!(CATASTROPHIC_LINES.contains(&pick_catastrophic_line(&mut entropy)));
    }
}

#[test]
fn pick_is_seeded_deterministic() {
    let mut first_entropy = SplitMix64Entropy::seeded(1234);
    let first = pick_catastrophic_line(&mut first_entropy);
    let mut second_entropy = SplitMix64Entropy::seeded(1234);
    assert_eq!(pick_catastrophic_line(&mut second_entropy), first);
}

#[test]
fn pick_covers_multiple_lines() {
    let mut entropy = SplitMix64Entropy::seeded(99);
    let mut seen = Vec::new();
    for _ in 0..400 {
        let line = pick_catastrophic_line(&mut entropy);
        if !seen.contains(&line) {
            seen.push(line);
        }
    }
    assert!(seen.len() > 1);
}

#[test]
fn formats_with_prefix_and_italics() {
    let (channel, sent) = RecordingChannel::new(ChannelMode::Send);
    let spawner = RecordingSpawner::default();
    post_atmospheric(Some(&channel), Some(&spawner), "X", "a tunnel folds shut");
    assert_eq!(sent_messages(&sent), ["X *a tunnel folds shut*"]);
    assert_eq!(spawner.tasks.lock().expect("spawner lock").len(), 1);
}

#[test]
fn strips_surrounding_whitespace_from_line() {
    let (channel, sent) = RecordingChannel::new(ChannelMode::Send);
    let spawner = RecordingSpawner::default();
    post_atmospheric(Some(&channel), Some(&spawner), "P", "   spaced line   ");
    assert_eq!(sent_messages(&sent), ["P *spaced line*"]);
}

#[test]
fn none_channel_is_a_noop() {
    let spawner = RecordingSpawner::default();
    post_atmospheric::<RecordingChannel, _>(None, Some(&spawner), "P", "line");
    assert!(spawner.tasks.lock().expect("spawner lock").is_empty());
}

#[test]
fn empty_line_is_a_noop() {
    let (channel, sent) = RecordingChannel::new(ChannelMode::Send);
    let spawner = RecordingSpawner::default();
    post_atmospheric(Some(&channel), Some(&spawner), "P", "");
    assert!(sent_messages(&sent).is_empty());
    assert!(spawner.tasks.lock().expect("spawner lock").is_empty());
}

#[test]
fn channel_without_send_is_a_noop() {
    let (channel, sent) = RecordingChannel::new(ChannelMode::Missing);
    let spawner = RecordingSpawner::default();
    post_atmospheric(Some(&channel), Some(&spawner), "P", "line");
    assert!(sent_messages(&sent).is_empty());
    assert!(spawner.tasks.lock().expect("spawner lock").is_empty());
}

#[test]
fn send_raising_is_swallowed() {
    let (channel, sent) = RecordingChannel::new(ChannelMode::SynchronousFailure);
    let spawner = RecordingSpawner::default();
    post_atmospheric(Some(&channel), Some(&spawner), "P", "line");
    assert!(sent_messages(&sent).is_empty());
    assert!(spawner.tasks.lock().expect("spawner lock").is_empty());
}

#[test]
fn no_running_loop_is_swallowed() {
    let (channel, sent) = RecordingChannel::new(ChannelMode::Send);
    post_atmospheric::<_, RecordingSpawner>(Some(&channel), None, "P", "line");
    assert_eq!(sent_messages(&sent), ["P *line*"]);
}

#[test]
fn catastrophic_post_uses_prefix_and_pool_line() {
    let mut expected_entropy = SplitMix64Entropy::seeded(7);
    let expected_line = pick_catastrophic_line(&mut expected_entropy);
    let (channel, sent) = RecordingChannel::new(ChannelMode::Send);
    let spawner = RecordingSpawner::default();
    let mut entropy = SplitMix64Entropy::seeded(7);
    post_catastrophic(Some(&channel), Some(&spawner), &mut entropy);
    assert_eq!(
        sent_messages(&sent),
        [format!("{CATASTROPHIC_PREFIX} *{expected_line}*")]
    );
}

#[test]
fn catastrophic_post_with_none_channel_is_a_noop() {
    let mut entropy = SplitMix64Entropy::seeded(0);
    let spawner = RecordingSpawner::default();
    post_catastrophic::<RecordingChannel, _, _>(None, Some(&spawner), &mut entropy);
    assert!(spawner.tasks.lock().expect("spawner lock").is_empty());
}
