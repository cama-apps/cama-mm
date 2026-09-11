//! Conservative summaries of successive live snapshots, never guessed combat logs.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveAnnouncementFrame {
    pub match_id: u64,
    pub game_time: i64,
    pub radiant_score: Option<i64>,
    pub dire_score: Option<i64>,
    pub radiant_net_worth: Option<i64>,
    pub dire_net_worth: Option<i64>,
    pub buildings: Vec<LiveBuilding>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveBuilding {
    pub key: String,
    pub radiant: bool,
    pub destroyed: bool,
}

/// A baseline and a reasonably adjacent sample are required. Reconnects must
/// not narrate historical deaths or classify missing fields as zero.
#[must_use]
pub fn announcements(
    previous: Option<&LiveAnnouncementFrame>,
    current: &LiveAnnouncementFrame,
) -> Vec<String> {
    let Some(previous) = previous else {
        return Vec::new();
    };
    let gap = current.game_time.saturating_sub(previous.game_time);
    if previous.match_id != current.match_id || !(1..=90).contains(&gap) {
        return Vec::new();
    }
    let mut lines = Vec::new();
    if let (Some(r0), Some(d0), Some(r1), Some(d1)) = (
        previous.radiant_score,
        previous.dire_score,
        current.radiant_score,
        current.dire_score,
    ) {
        let radiant = r1.saturating_sub(r0);
        let dire = d1.saturating_sub(d0);
        if radiant >= 0 && dire >= 0 && radiant.saturating_add(dire) >= 3 {
            lines.push(format!(
                "⚔️ **Radiant {radiant}–{dire} Dire** in kills over the last {gap}s"
            ));
        }
    }
    if let (Some(r0), Some(d0), Some(r1), Some(d1)) = (
        previous.radiant_net_worth,
        previous.dire_net_worth,
        current.radiant_net_worth,
        current.dire_net_worth,
    ) {
        let before = r0.saturating_sub(d0);
        let after = r1.saturating_sub(d1);
        let change = after.saturating_sub(before);
        if change.unsigned_abs() >= 3000 {
            lines.push(format!(
                "💰 **{} swing to {}** — {}",
                compact(change.unsigned_abs()),
                if change > 0 { "Radiant" } else { "Dire" },
                economy_lead(after)
            ));
        }
    }
    for radiant in [true, false] {
        let lost = current
            .buildings
            .iter()
            .filter(|building| {
                building.radiant == radiant
                    && building.destroyed
                    && previous.buildings.iter().any(|old| {
                        old.key == building.key && old.radiant == radiant && !old.destroyed
                    })
            })
            .map(|building| &building.key)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        if lost > 0 {
            lines.push(format!(
                "🏰 **{}** lost {}",
                if radiant { "Radiant" } else { "Dire" },
                if lost == 1 {
                    "a structure".into()
                } else {
                    format!("**{lost} structures**")
                }
            ));
        }
    }
    lines.truncate(8);
    lines
}

fn compact(value: u64) -> String {
    if value < 1000 {
        return value.to_string();
    }
    let tenths = value.saturating_add(50) / 100;
    if tenths.is_multiple_of(10) {
        format!("{}k", tenths / 10)
    } else {
        format!("{}.{:01}k", tenths / 10, tenths % 10)
    }
}

fn economy_lead(lead: i64) -> String {
    if lead == 0 {
        "net worth is **level**".into()
    } else {
        format!(
            "{} leads by **{}**",
            if lead > 0 { "Radiant" } else { "Dire" },
            compact(lead.unsigned_abs())
        )
    }
}

fn headline(previous: &LiveAnnouncementFrame, current: &LiveAnnouncementFrame) -> &'static str {
    if let (Some(r0), Some(d0), Some(r1), Some(d1)) = (
        previous.radiant_net_worth,
        previous.dire_net_worth,
        current.radiant_net_worth,
        current.dire_net_worth,
    ) {
        let before = r0.saturating_sub(d0);
        let after = r1.saturating_sub(d1);
        let swing = after.saturating_sub(before);
        if swing.unsigned_abs() >= 3000 {
            return match (swing > 0, before, after) {
                (_, _, 0) => "Back to even",
                (true, ..=0, 1..) => "Radiant takes the lead",
                (false, 0.., ..=-1) => "Dire takes the lead",
                (true, _, 1..) => "Radiant extends the lead",
                (false, _, ..=-1) => "Dire extends the lead",
                (true, _, _) => "Radiant closes the gap",
                (false, _, _) => "Dire closes the gap",
            };
        }
    }
    if let (Some(r0), Some(d0), Some(r1), Some(d1)) = (
        previous.radiant_score,
        previous.dire_score,
        current.radiant_score,
        current.dire_score,
    ) {
        let r = r1.saturating_sub(r0);
        let d = d1.saturating_sub(d0);
        if r >= 0 && d >= 0 && r.saturating_add(d) >= 3 {
            return match r.cmp(&d) {
                std::cmp::Ordering::Greater => "Radiant racks up kills",
                std::cmp::Ordering::Less => "Dire racks up kills",
                std::cmp::Ordering::Equal => "Kills traded on both sides",
            };
        }
    }
    "Structures fall"
}

/// Render only verified changes; scoreboard deltas never claim a single fight.
#[must_use]
pub fn announcement_message(
    previous: Option<&LiveAnnouncementFrame>,
    current: &LiveAnnouncementFrame,
    delay_seconds: Option<i64>,
) -> Option<String> {
    let lines = announcements(previous, current);
    if lines.is_empty() {
        return None;
    }
    let mut message = format!(
        "📻 **{}:{:02} — {}**\n{}",
        current.game_time / 60,
        current.game_time.rem_euclid(60),
        headline(previous?, current),
        lines.join("\n")
    );
    let mut footer = Vec::new();
    if let (Some(r), Some(d)) = (current.radiant_score, current.dire_score) {
        footer.push(format!("Score: Radiant {r}–{d} Dire"));
    }
    match delay_seconds {
        Some(delay) if delay > 0 => footer.push(format!("Feed delay: {delay}s")),
        None => footer.push("Feed delay unknown".into()),
        _ => {}
    }
    if !footer.is_empty() {
        message.push_str(&format!("\n_{}_", footer.join(" · ")));
    }
    Some(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(time: i64) -> LiveAnnouncementFrame {
        LiveAnnouncementFrame {
            match_id: 1,
            game_time: time,
            radiant_score: Some(5),
            dire_score: Some(6),
            radiant_net_worth: Some(20_000),
            dire_net_worth: Some(20_000),
            buildings: vec![LiveBuilding {
                key: "tower".into(),
                radiant: true,
                destroyed: false,
            }],
        }
    }
    #[test]
    fn baseline_missing_stale_and_other_match_never_invent_events() {
        let a = frame(100);
        assert!(announcements(None, &a).is_empty());
        let mut b = frame(101);
        b.radiant_score = None;
        b.radiant_net_worth = None;
        assert!(announcements(Some(&a), &b).is_empty());
        b = frame(300);
        b.radiant_score = Some(20);
        assert!(announcements(Some(&a), &b).is_empty());
        b.game_time = 90;
        assert!(announcements(Some(&a), &b).is_empty());
        b.game_time = 101;
        b.match_id = 2;
        assert!(announcements(Some(&a), &b).is_empty());
    }
    #[test]
    fn detects_conservative_deltas_once() {
        let a = frame(100);
        let mut b = frame(145);
        b.radiant_score = Some(9);
        b.dire_net_worth = Some(24_000);
        b.buildings[0].destroyed = true;
        let lines = announcements(Some(&a), &b);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("Radiant 4–0 Dire"));
        assert!(lines[1].contains("4k swing to Dire"));
        assert!(announcements(Some(&b), &b).is_empty());
    }
    #[test]
    fn lead_change_renders_readable_bulletin_with_score_and_no_zero_delay_noise() {
        let mut before = frame(1350);
        before.radiant_net_worth = Some(19_000);
        before.buildings[0].radiant = false;
        let mut after = before.clone();
        after.game_time = 1395;
        after.radiant_score = Some(9);
        after.dire_score = Some(7);
        after.radiant_net_worth = Some(22_500);
        after.buildings[0].destroyed = true;
        assert_eq!(
            announcement_message(Some(&before), &after, Some(0)).unwrap(),
            "📻 **23:15 — Radiant takes the lead**\n⚔️ **Radiant 4–1 Dire** in kills over the last 45s\n💰 **3.5k swing to Radiant** — Radiant leads by **2.5k**\n🏰 **Dire** lost a structure\n_Score: Radiant 9–7 Dire_"
        );
    }

    #[test]
    fn comeback_that_remains_behind_does_not_claim_the_lead() {
        let mut before = frame(100);
        before.radiant_net_worth = Some(30_000);
        let mut after = before.clone();
        after.game_time = 145;
        after.dire_net_worth = Some(24_000);
        let text = announcement_message(Some(&before), &after, Some(120)).unwrap();
        assert!(text.contains("Dire closes the gap"));
        assert!(text.contains("4k swing to Dire** — Radiant leads by **6k"));
        assert!(text.contains("Feed delay: 120s"));
        assert!(!text.contains("Dire takes the lead"));
        after.dire_net_worth = Some(30_000);
        let text = announcement_message(Some(&before), &after, None).unwrap();
        assert!(text.contains("Back to even"));
        assert!(text.contains("net worth is **level**"));
        assert!(text.contains("Feed delay unknown"));
    }

    #[test]
    fn dire_lead_change_and_extension_are_not_reported_as_radiant_advantage() {
        let mut before = frame(100);
        before.radiant_net_worth = Some(21_000);
        let mut after = before.clone();
        after.game_time = 145;
        after.dire_net_worth = Some(24_500);
        let text = announcement_message(Some(&before), &after, Some(0)).unwrap();
        assert!(text.contains("Dire takes the lead"));
        assert!(text.contains("Dire leads by **3.5k**"));
        before.dire_net_worth = Some(21_500);
        assert!(
            announcement_message(Some(&before), &after, Some(0))
                .unwrap()
                .contains("Dire extends the lead")
        );
    }

    #[test]
    fn structures_are_grouped_without_duplicate_or_invented_objectives() {
        let mut before = frame(100);
        before.radiant_score = None;
        before.dire_score = None;
        let mut second = before.buildings[0].clone();
        second.key = "second".into();
        before.buildings.push(second);
        let mut after = before.clone();
        after.game_time = 145;
        after.buildings.iter_mut().for_each(|b| b.destroyed = true);
        after.buildings.push(after.buildings[0].clone());
        let text = announcement_message(Some(&before), &after, Some(0)).unwrap();
        assert_eq!(
            text,
            "📻 **2:25 — Structures fall**\n🏰 **Radiant** lost **2 structures**"
        );
    }

    #[test]
    fn quiet_stale_and_missing_samples_do_not_create_a_bulletin() {
        let before = frame(100);
        assert!(announcement_message(None, &before, Some(0)).is_none());
        assert!(announcement_message(Some(&before), &frame(145), Some(0)).is_none());
        let mut after = frame(200);
        after.radiant_score = Some(10);
        assert!(announcement_message(Some(&before), &after, Some(0)).is_none());
        after.game_time = 145;
        after.dire_score = None;
        assert!(announcement_message(Some(&before), &after, Some(0)).is_none());
    }
}
