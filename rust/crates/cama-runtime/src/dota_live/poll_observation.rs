//! Throttle repeated polling health without hiding source status changes.

use std::collections::BTreeMap;

use super::LiveSnapshotSource;

const REPEAT_INTERVAL_SECONDS: i64 = 300;

#[derive(Clone, Debug, Default)]
pub(super) struct PollObservation {
    sources: BTreeMap<&'static str, (String, i64)>,
}

impl PollObservation {
    /// Return whether the caller should log this source's fixed status label.
    /// Callers must not pass upstream error text, URLs, or response bodies.
    pub(super) fn observe(&mut self, source: LiveSnapshotSource, status: &str, now: i64) -> bool {
        let key = source.as_str();
        if self.sources.get(key).is_some_and(|(previous, at)| {
            previous == status && now.saturating_sub(*at) < REPEAT_INTERVAL_SECONDS
        }) {
            return false;
        }
        self.sources.insert(key, (status.to_owned(), now));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_changes_are_reported_immediately() {
        let mut observation = PollObservation::default();
        let source = LiveSnapshotSource::RealtimeStats;
        assert!(observation.observe(source, "unavailable", 100));
        assert!(observation.observe(source, "qualified", 101));
        assert!(observation.observe(source, "unavailable", 102));
    }

    #[test]
    fn unchanged_status_repeats_at_five_minutes_without_sliding_deadline() {
        let mut observation = PollObservation::default();
        let source = LiveSnapshotSource::RealtimeStats;
        assert!(observation.observe(source, "qualified", 100));
        assert!(!observation.observe(source, "qualified", 115));
        assert!(!observation.observe(source, "qualified", 399));
        assert!(observation.observe(source, "qualified", 400));
        assert!(!observation.observe(source, "qualified", 415));
        assert!(observation.observe(source, "qualified", 700));
    }

    #[test]
    fn sources_have_independent_status_and_repeat_deadlines() {
        let mut observation = PollObservation::default();
        let realtime = LiveSnapshotSource::RealtimeStats;
        let league = LiveSnapshotSource::LiveLeagueGames;
        assert!(observation.observe(realtime, "qualified", 100));
        assert!(observation.observe(league, "qualified", 110));
        assert!(!observation.observe(realtime, "qualified", 120));
        assert!(observation.observe(league, "unavailable", 130));
        assert!(!observation.observe(realtime, "qualified", 140));
        assert!(observation.observe(realtime, "qualified", 400));
        assert!(!observation.observe(league, "unavailable", 400));
        assert!(observation.observe(league, "unavailable", 430));
    }
}
