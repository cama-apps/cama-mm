//! Pure Discord voice-stream state projection.

use std::collections::{HashMap, HashSet};

/// The portion of a Discord voice state needed for Go Live detection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VoiceState {
    pub self_stream: bool,
}

/// Discord member presence projected into domain-owned data.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MemberPresence {
    pub voice: Option<VoiceState>,
    /// Retained to make it explicit that game activities do not affect the decision.
    pub activities: Vec<String>,
}

/// Return requested player IDs whose guild member is actively using Discord Go Live.
#[must_use]
pub fn get_streaming_player_ids(
    members: &HashMap<i64, MemberPresence>,
    player_ids: &[i64],
) -> HashSet<i64> {
    player_ids
        .iter()
        .copied()
        .filter(|player_id| {
            members
                .get(player_id)
                .and_then(|member| member.voice)
                .is_some_and(|voice| voice.self_stream)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{MemberPresence, VoiceState, get_streaming_player_ids};
    use std::collections::{HashMap, HashSet};

    fn member(in_voice: bool, self_stream: bool, activities: &[&str]) -> MemberPresence {
        MemberPresence {
            voice: in_voice.then_some(VoiceState { self_stream }),
            activities: activities.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn test_go_live_included() {
        let members = HashMap::from([(1, member(true, true, &["Dota 2"]))]);
        assert_eq!(get_streaming_player_ids(&members, &[1]), HashSet::from([1]));
    }

    #[test]
    fn test_go_live_non_dota_game_included() {
        let members = HashMap::from([(1, member(true, true, &["Counter-Strike 2"]))]);
        assert_eq!(get_streaming_player_ids(&members, &[1]), HashSet::from([1]));
    }

    #[test]
    fn test_go_live_no_activities_included() {
        let members = HashMap::from([(1, member(true, true, &[]))]);
        assert_eq!(get_streaming_player_ids(&members, &[1]), HashSet::from([1]));
    }

    #[test]
    fn test_in_voice_not_go_live_excluded() {
        let members = HashMap::from([(1, member(true, false, &["Dota 2"]))]);
        assert!(get_streaming_player_ids(&members, &[1]).is_empty());
    }

    #[test]
    fn test_not_in_voice_excluded() {
        let members = HashMap::from([(1, member(false, false, &["Dota 2"]))]);
        assert!(get_streaming_player_ids(&members, &[1]).is_empty());
    }

    #[test]
    fn test_none_voice_state_excluded() {
        let members = HashMap::from([(1, member(false, false, &[]))]);
        assert!(get_streaming_player_ids(&members, &[1]).is_empty());
    }

    #[test]
    fn test_member_not_found_excluded() {
        assert!(get_streaming_player_ids(&HashMap::new(), &[999]).is_empty());
    }

    #[test]
    fn test_multiple_players_mixed() {
        let members = HashMap::from([
            (1, member(true, true, &["Dota 2"])),
            (2, member(true, false, &["Dota 2"])),
            (3, member(true, true, &["Deadlock"])),
            (4, member(true, true, &[])),
        ]);
        assert_eq!(
            get_streaming_player_ids(&members, &[1, 2, 3, 4]),
            HashSet::from([1, 3, 4])
        );
    }

    #[test]
    fn test_empty_player_list() {
        assert!(get_streaming_player_ids(&HashMap::new(), &[]).is_empty());
    }
}
