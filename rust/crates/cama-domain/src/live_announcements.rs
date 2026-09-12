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
    #[serde(default)]
    pub players: Vec<LiveHero>,
    #[serde(default)]
    pub events: Vec<LiveEvent>,
    #[serde(default)]
    pub radiant_win: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveBuilding {
    pub key: String,
    pub radiant: bool,
    pub destroyed: bool,
    #[serde(default)]
    pub name: Option<String>,
}

pub const ANNOUNCEMENT_INTERVAL_SECONDS: i64 = 15;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveHero {
    pub hero_id: u32,
    #[serde(default)]
    pub account_id: Option<u32>,
    #[serde(default)]
    pub display_name: Option<String>,
    pub name: String,
    pub radiant: bool,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveEvent {
    pub id: String,
    pub game_time: i64,
    #[serde(flatten)]
    pub kind: LiveEventKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiveEventKind {
    HeroKill {
        killer: String,
        victim: String,
    },
    FirstBlood {
        radiant: bool,
        killer: Option<String>,
        victim: Option<String>,
    },
    TeamFight {
        started_at: i64,
        radiant_kills: i64,
        dire_kills: i64,
        radiant_gold_delta: Option<i64>,
        dire_gold_delta: Option<i64>,
    },
    Roshan {
        radiant: Option<bool>,
    },
    Aegis {
        hero: Option<String>,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AnnouncementMemory {
    pub match_id: Option<u64>,
    pub first_blood_announced: bool,
    pub radiant_gold_milestone: u64,
    pub dire_gold_milestone: u64,
    pub seen_events: std::collections::BTreeSet<String>,
    pub winner_announced: bool,
    pub delay_announced: bool,
}

fn side(radiant: bool) -> &'static str {
    if radiant { "Radiant" } else { "Dire" }
}

fn label(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '\'' | '-'))
        .take(64)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn hero_actor(hero: &LiveHero) -> String {
    let hero_name = label(&hero.name);
    match hero
        .display_name
        .as_deref()
        .map(label)
        .filter(|s| !s.is_empty())
    {
        Some(name) => format!("{name}'s {hero_name}"),
        None => hero_name,
    }
}

fn event_actor(name: &str, frame: &LiveAnnouncementFrame) -> String {
    let mut matching = frame.players.iter().filter(|hero| hero.name == name);
    match (matching.next(), matching.next()) {
        (Some(hero), None) => hero_actor(hero),
        _ => label(name),
    }
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

fn lead(frame: &LiveAnnouncementFrame) -> Option<i64> {
    let (r, d) = (frame.radiant_net_worth?, frame.dire_net_worth?);
    (r >= 0 && d >= 0).then(|| r.saturating_sub(d))
}

fn milestone(frame: &LiveAnnouncementFrame, lead: i64) -> u64 {
    let step = if frame.game_time <= 600 {
        1000
    } else if frame.game_time <= 1200 {
        2000
    } else {
        5000
    };
    lead.unsigned_abs() / step * step
}

fn remember_event(memory: &mut AnnouncementMemory, id: &str) {
    memory.seen_events.insert(id.to_owned());
    // Events also require a current timestamp; ancient IDs need not grow the
    // durable state forever. Keys from source normalization are bounded.
    while memory.seen_events.len() > 256 {
        memory.seen_events.pop_first();
    }
}

fn observe_baseline(memory: &mut AnnouncementMemory, frame: &LiveAnnouncementFrame) {
    if let Some(lead) = lead(frame) {
        let slot = if lead > 0 {
            &mut memory.radiant_gold_milestone
        } else {
            &mut memory.dire_gold_milestone
        };
        *slot = (*slot).max(milestone(frame, lead));
    }
    for event in frame
        .events
        .iter()
        .filter(|e| e.game_time <= frame.game_time && e.id.len() <= 128)
        .take(64)
    {
        remember_event(memory, &event.id);
    }
    memory.winner_announced |= frame.radiant_win.is_some();
}

fn score_delta(a: &LiveAnnouncementFrame, b: &LiveAnnouncementFrame) -> Option<(i64, i64)> {
    let (r0, d0, r1, d1) = (
        a.radiant_score?,
        a.dire_score?,
        b.radiant_score?,
        b.dire_score?,
    );
    if [r0, d0, r1, d1].iter().any(|v| !(0..=10_000).contains(v)) || r1 < r0 || d1 < d0 {
        return None;
    }
    Some((r1 - r0, d1 - d0))
}

/// Complete matching hero rosters are required for counter-based attribution.
/// Team scores include deaths without hero kill credit, so scores alone must
/// never decide the first-blood hero or attribute an individual kill.
fn hero_deltas<'a>(
    a: &LiveAnnouncementFrame,
    b: &'a LiveAnnouncementFrame,
) -> Option<Vec<(&'a LiveHero, i64, i64)>> {
    let ids = |f: &LiveAnnouncementFrame| {
        f.players
            .iter()
            .map(|p| p.hero_id)
            .collect::<std::collections::BTreeSet<_>>()
    };
    if a.players.len() != 10
        || b.players.len() != 10
        || ids(a).len() != 10
        || ids(a) != ids(b)
        || b.players.iter().filter(|p| p.radiant).count() != 5
        || b.players
            .iter()
            .any(|p| p.hero_id == 0 || label(&p.name).is_empty())
    {
        return None;
    }
    b.players
        .iter()
        .map(|p| {
            let old = a
                .players
                .iter()
                .find(|old| old.hero_id == p.hero_id && old.radiant == p.radiant)?;
            if let (Some(old_account), Some(account)) = (old.account_id, p.account_id)
                && old_account != account
            {
                return None;
            }
            let (k0, k1, d0, d1) = (old.kills?, p.kills?, old.deaths?, p.deaths?);
            if [k0, k1, d0, d1].iter().any(|v| !(0..=10_000).contains(v)) || k1 < k0 || d1 < d0 {
                return None;
            }
            Some((p, k1 - k0, d1 - d0))
        })
        .collect()
}

fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        _ => format!(
            "{} and {}",
            names[..names.len() - 1].join(", "),
            names.last().unwrap()
        ),
    }
}

// Counter snapshots establish pairs only when one credited killer accounts
// for every enemy death and the matching team-score increase in the interval.
fn resolved_kills(
    changes: &[(&LiveHero, i64, i64)],
    radiant_score: i64,
    dire_score: i64,
) -> std::collections::BTreeMap<bool, String> {
    let mut resolved = std::collections::BTreeMap::new();
    for radiant in [true, false] {
        let killers: Vec<_> = changes
            .iter()
            .filter(|(p, k, _)| p.radiant == radiant && *k > 0)
            .collect();
        let [killer] = killers.as_slice() else {
            continue;
        };
        let score = if radiant { radiant_score } else { dire_score };
        let victims: Vec<_> = changes
            .iter()
            .filter(|(p, _, d)| p.radiant != radiant && *d > 0)
            .collect();
        let deaths: i64 = victims.iter().map(|(_, _, d)| *d).sum();
        if killer.1 != score || deaths != score {
            continue;
        }
        let names: Vec<_> = victims
            .iter()
            .map(|(p, _, deaths)| {
                let name = hero_actor(p);
                if *deaths == 1 {
                    name
                } else {
                    format!("{name} ({deaths} times)")
                }
            })
            .collect();
        resolved.insert(
            radiant,
            format!("{} killed {}", hero_actor(killer.0), join_names(&names)),
        );
    }
    resolved
}

fn kill_line(
    a: &LiveAnnouncementFrame,
    b: &LiveAnnouncementFrame,
    memory: &mut AnnouncementMemory,
) -> Option<String> {
    let (r, d) = score_delta(a, b)?;
    if r + d == 0 {
        return None;
    }
    let deltas = hero_deltas(a, b);
    if let Some(changes) = &deltas {
        let paired = resolved_kills(changes, r, d);
        let kills: i64 = changes.iter().map(|(_, k, _)| *k).sum();
        let old_total: i64 = a.players.iter().map(|p| p.kills.unwrap_or(0)).sum();
        if old_total == 0 && kills == 1 && !memory.first_blood_announced {
            memory.first_blood_announced = true;
            let killer = changes.iter().find(|(_, k, _)| *k == 1)?.0;
            let name = hero_actor(killer);
            if let Some(pair) = paired.get(&killer.radiant) {
                return Some(format!(
                    "{pair} — first hero kill for {}!",
                    side(killer.radiant)
                ));
            }
            return Some(format!(
                "{name} gets the first hero kill for {}. We're off!",
                side(killer.radiant)
            ));
        }
        // Counter deltas describe the interval, not an inferred combat log.
        if kills > 0 {
            let mut credits = changes
                .iter()
                .filter(|(_, k, _)| *k > 0)
                .map(|(p, k, _)| {
                    let name = hero_actor(p);
                    if let Some(pair) = paired.get(&p.radiant) {
                        return pair.clone();
                    }
                    if *k == 1 {
                        format!("{name} finds a kill")
                    } else {
                        format!("{name} picks up {k} kills")
                    }
                })
                .collect::<Vec<_>>();
            credits.truncate(10);
            let mut line = format!("{}!", credits.join("; "));
            let victims = changes
                .iter()
                .filter(|(p, _, d)| *d > 0 && !paired.contains_key(&!p.radiant))
                .map(|(p, _, _)| hero_actor(p))
                .take(10)
                .collect::<Vec<_>>();
            if !victims.is_empty() {
                line.push_str(&format!(
                    " {} {} down.",
                    join_names(&victims),
                    if victims.len() == 1 { "goes" } else { "go" }
                ));
            }
            if r >= 3 && d == 0 {
                line.push_str(" Radiant are rolling.");
            } else if d >= 3 && r == 0 {
                line.push_str(" Dire are rolling.");
            }
            return Some(line);
        }
    }
    Some(match (r, d) {
        (1, 0) => "Radiant add one to the board.".into(),
        (0, 1) => "Dire add one to the board.".into(),
        (r, 0) => format!(
            "{r} unanswered for Radiant in the last {}s. They're rolling.",
            b.game_time - a.game_time
        ),
        (0, d) => format!(
            "{d} unanswered for Dire in the last {}s. They're rolling.",
            b.game_time - a.game_time
        ),
        (r, d) => format!(
            "Busy last {}s — {r}–{d} on the board for Radiant and Dire.",
            b.game_time - a.game_time
        ),
    })
}

fn economy_line(
    a: &LiveAnnouncementFrame,
    b: &LiveAnnouncementFrame,
    memory: &mut AnnouncementMemory,
) -> Option<String> {
    let (before, after) = (lead(a)?, lead(b)?);
    let change = after.saturating_sub(before);
    let threshold = if b.game_time <= 600 { 1000 } else { 2000 };
    let reached = milestone(b, after);
    let old_peak = if after > 0 {
        memory.radiant_gold_milestone
    } else {
        memory.dire_gold_milestone
    };
    let flip = before.signum() != after.signum() && after.unsigned_abs() >= 1000;
    let milestone_crossed = reached > old_peak && reached > 0;
    let swing = change.unsigned_abs() >= threshold;
    if !flip && !milestone_crossed && !swing {
        return None;
    }
    let slot = if after > 0 {
        &mut memory.radiant_gold_milestone
    } else {
        &mut memory.dire_gold_milestone
    };
    *slot = (*slot).max(reached);
    let amount = compact(after.unsigned_abs());
    let gain = compact(change.unsigned_abs());
    let ahead = side(after > 0);
    let gaining = side(change > 0);
    if after == 0 {
        return Some(format!(
            "Back to even. {gaining} just clawed back {gain} gold."
        ));
    }
    if flip && before != 0 {
        return Some(format!(
            "{gaining} have flipped it — {amount} gold ahead now."
        ));
    }
    if milestone_crossed && !swing {
        return Some(if b.game_time <= 600 {
            format!("{ahead} hit a {amount} gold lead in lanes. That's starting to add up.")
        } else {
            format!("{ahead} are {amount} gold ahead now. The gap keeps growing.")
        });
    }
    if change.signum() != after.signum() {
        Some(format!(
            "{gaining} claw back {gain} gold. Still {amount} behind, but that's a start."
        ))
    } else if b.game_time <= 600 {
        Some(format!(
            "{ahead} are {amount} gold up in lanes — a {gain} swing their way."
        ))
    } else {
        Some(format!(
            "Another {gain} gold swings to {gaining}. {amount} ahead now."
        ))
    }
}

fn event_line(event: &LiveEvent, frame: &LiveAnnouncementFrame) -> Option<String> {
    match &event.kind {
        LiveEventKind::HeroKill { killer, victim } => {
            let (killer, victim) = (event_actor(killer, frame), event_actor(victim, frame));
            (!killer.is_empty() && !victim.is_empty() && killer != victim)
                .then(|| format!("{killer} killed {victim}!"))
        }
        LiveEventKind::FirstBlood {
            radiant,
            killer,
            victim,
        } => {
            let mut line = match killer
                .as_deref()
                .map(|name| event_actor(name, frame))
                .filter(|s| !s.is_empty())
            {
                Some(killer) => format!("First blood! {killer} gets {} started", side(*radiant)),
                None => format!("First blood for {}", side(*radiant)),
            };
            if let Some(victim) = victim
                .as_deref()
                .map(|name| event_actor(name, frame))
                .filter(|s| !s.is_empty())
            {
                line.push_str(&format!(" — {victim} goes down"));
            }
            line.push('.');
            Some(line)
        }
        LiveEventKind::TeamFight {
            started_at,
            radiant_kills: r,
            dire_kills: d,
            radiant_gold_delta: rg,
            dire_gold_delta: dg,
        } => {
            if *started_at < 0
                || *started_at >= event.game_time
                || !(0..=100).contains(r)
                || !(0..=100).contains(d)
                || r + d == 0
            {
                return None;
            }
            let span = format!(
                "{}:{:02}–{}:{:02}",
                started_at / 60,
                started_at % 60,
                event.game_time / 60,
                event.game_time % 60
            );
            let mut line = if r == d {
                format!("That fight ends {r}–{d}. Both sides trade blows ({span}).")
            } else {
                let (w, l) = (r.max(d), r.min(d));
                format!(
                    "{} take that fight {w}–{l} on kills ({span}). {}",
                    side(r > d),
                    if *l == 0 {
                        "No kills given back."
                    } else {
                        "They come out on top there."
                    }
                )
            };
            if let (Some(rg), Some(dg)) = (rg, dg) {
                let swing = rg.saturating_sub(*dg);
                if swing.unsigned_abs() >= 500 {
                    line.push_str(&format!(
                        " Gold change favors {} by {}.",
                        side(swing > 0),
                        compact(swing.unsigned_abs())
                    ));
                }
            }
            Some(line)
        }
        LiveEventKind::Roshan { radiant } => Some(match radiant {
            Some(team) => format!("Roshan goes to {}. Big pickup.", side(*team)),
            None => "Roshan is down. No confirmed team credit yet.".into(),
        }),
        LiveEventKind::Aegis { hero } => Some(
            match hero
                .as_deref()
                .map(|name| event_actor(name, frame))
                .filter(|s| !s.is_empty())
            {
                Some(hero) => format!("Aegis on {hero}. They've got a second life to work with."),
                None => "Aegis picked up. Someone's got a second life now.".into(),
            },
        ),
    }
}

fn lines_with_memory(
    previous: Option<&LiveAnnouncementFrame>,
    current: &LiveAnnouncementFrame,
    memory: &mut AnnouncementMemory,
) -> Vec<String> {
    if memory.match_id != Some(current.match_id) {
        *memory = AnnouncementMemory {
            match_id: Some(current.match_id),
            ..Default::default()
        };
        if let Some(previous) = previous.filter(|p| p.match_id == current.match_id) {
            observe_baseline(memory, previous);
        }
    }
    let Some(previous) = previous else {
        observe_baseline(memory, current);
        return Vec::new();
    };
    let gap = current.game_time.saturating_sub(previous.game_time);
    if previous.match_id != current.match_id || !(1..=90).contains(&gap) || current.game_time < 0 {
        if gap > 90 || previous.match_id != current.match_id {
            observe_baseline(memory, current);
        }
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut explicit_kills = false;
    for event in current
        .events
        .iter()
        .filter(|event| {
            event.game_time > previous.game_time && event.game_time <= current.game_time
        })
        .take(64)
    {
        if event.id.is_empty()
            || event.id.len() > 128
            || event.game_time <= previous.game_time
            || event.game_time > current.game_time
            || memory.seen_events.contains(&event.id)
        {
            continue;
        }
        if matches!(event.kind, LiveEventKind::FirstBlood { .. }) && memory.first_blood_announced {
            continue;
        }
        if let Some(line) = event_line(event, current) {
            explicit_kills |= matches!(
                event.kind,
                LiveEventKind::FirstBlood { .. }
                    | LiveEventKind::TeamFight { .. }
                    | LiveEventKind::HeroKill { .. }
            );
            memory.first_blood_announced |= matches!(event.kind, LiveEventKind::FirstBlood { .. });
            remember_event(memory, &event.id);
            lines.push(line);
        }
    }
    if !explicit_kills && let Some(line) = kill_line(previous, current, memory) {
        lines.push(line);
    }
    if let Some(line) = economy_line(previous, current, memory) {
        lines.push(line);
    }
    let mut seen = std::collections::BTreeSet::new();
    for radiant in [true, false] {
        let lost = current
            .buildings
            .iter()
            .filter(|b| b.radiant == radiant && b.destroyed)
            .filter(|b| {
                previous
                    .buildings
                    .iter()
                    .any(|old| old.key == b.key && old.radiant == b.radiant && !old.destroyed)
            })
            .filter(|b| seen.insert((&b.key, b.radiant)))
            .collect::<Vec<_>>();
        if lost.is_empty() {
            continue;
        }
        let names = lost
            .iter()
            .map(|b| {
                b.name
                    .as_deref()
                    .map(label)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "structure".into())
            })
            .collect::<Vec<_>>();
        if names.iter().all(|n| n == "structure") {
            lines.push(if names.len() == 1 {
                format!("{} lose a structure.", side(radiant))
            } else {
                format!("{} lose {} structures.", side(radiant), names.len())
            });
        } else if names.len() > 1 && names.iter().all(|n| n == "tower" || n == "barracks") {
            let towers = names.iter().filter(|n| *n == "tower").count();
            let barracks = names.len() - towers;
            let mut lost = Vec::new();
            if towers > 0 {
                lost.push(format!(
                    "{towers} {}",
                    if towers == 1 { "tower" } else { "towers" }
                ));
            }
            if barracks > 0 {
                lost.push(format!("{barracks} barracks"));
            }
            lines.push(format!("{} lose {}.", side(radiant), join_names(&lost)));
        } else {
            let high_ground = names
                .iter()
                .any(|n| n.contains("tier 3") || n.contains("barracks"));
            lines.push(format!(
                "{}'s {} {} down.{}",
                side(radiant),
                join_names(&names),
                if names.len() == 1 { "goes" } else { "go" },
                if high_ground {
                    " High ground is taking a beating."
                } else {
                    ""
                }
            ));
        }
    }
    if let Some(radiant) = current.radiant_win
        && previous.radiant_win.is_none()
        && !memory.winner_announced
    {
        memory.winner_announced = true;
        lines.push(format!("That's it — {} take the game. GG.", side(radiant)));
    }
    lines
}

/// Stateless compatibility helper; long-running delivery uses persisted memory.
#[must_use]
pub fn announcements(
    previous: Option<&LiveAnnouncementFrame>,
    current: &LiveAnnouncementFrame,
) -> Vec<String> {
    lines_with_memory(previous, current, &mut AnnouncementMemory::default())
}

#[must_use]
pub fn announcement_message(
    previous: Option<&LiveAnnouncementFrame>,
    current: &LiveAnnouncementFrame,
    delay_seconds: Option<i64>,
) -> Option<String> {
    announcement_message_with_memory(
        previous,
        current,
        delay_seconds,
        &mut AnnouncementMemory::default(),
    )
}

/// Casual, deterministic commentary from measured changes. No LLM, invented
/// fight grouping or guessed net worth. The caller persists memory with outbox.
#[must_use]
pub fn announcement_message_with_memory(
    previous: Option<&LiveAnnouncementFrame>,
    current: &LiveAnnouncementFrame,
    delay_seconds: Option<i64>,
    memory: &mut AnnouncementMemory,
) -> Option<String> {
    let lines = lines_with_memory(previous, current, memory);
    if lines.is_empty() {
        return None;
    }
    let mut text = format!(
        "**{}:{:02}** {}",
        current.game_time / 60,
        current.game_time % 60,
        lines.join("\n")
    );
    if let (Some(r), Some(d)) = (current.radiant_score, current.dire_score) {
        text.push_str(&format!("\n_Radiant {r}–{d} Dire_"));
    }
    if !memory.delay_announced {
        if let Some(delay) = delay_seconds.filter(|d| *d > 0) {
            text.push_str(&format!(" · feed {delay}s behind"));
        } else if delay_seconds.is_none() {
            text.push_str(" · feed delay unknown");
        }
        memory.delay_announced = true;
    }
    // All input labels and event counts are bounded; stay within one Discord
    // message without dropping the persisted record halfway through delivery.
    if text.encode_utf16().count() > 1900 {
        let mut length = 0;
        text = text
            .chars()
            .take_while(|c| {
                length += c.len_utf16();
                length <= 1800
            })
            .collect();
        text.push_str("\n…busy update; some details omitted to fit this message.");
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(time: i64) -> LiveAnnouncementFrame {
        LiveAnnouncementFrame {
            match_id: 1,
            game_time: time,
            radiant_score: Some(0),
            dire_score: Some(0),
            radiant_net_worth: Some(20_000),
            dire_net_worth: Some(20_000),
            buildings: vec![LiveBuilding {
                key: "r-bottom-1".into(),
                radiant: true,
                destroyed: false,
                name: Some("bottom tier 1 tower".into()),
            }],
            ..Default::default()
        }
    }
    fn heroes(frame: &mut LiveAnnouncementFrame) {
        frame.players = (1..=10)
            .map(|id| LiveHero {
                hero_id: id,
                account_id: Some(id),
                display_name: None,
                name: format!("Hero {id}"),
                radiant: id <= 5,
                kills: Some(0),
                deaths: Some(0),
            })
            .collect();
    }
    fn text(a: &LiveAnnouncementFrame, b: &LiveAnnouncementFrame) -> String {
        announcement_message(Some(a), b, Some(0)).unwrap()
    }

    #[test]
    fn baseline_missing_stale_and_other_match_never_invent_events() {
        let a = frame(100);
        assert!(announcements(None, &a).is_empty());
        let mut b = frame(115);
        b.radiant_score = None;
        b.radiant_net_worth = None;
        assert!(announcements(Some(&a), &b).is_empty());
        b = frame(300);
        b.radiant_score = Some(20);
        assert!(announcements(Some(&a), &b).is_empty());
        for time in [90, 100] {
            b.game_time = time;
            assert!(announcements(Some(&a), &b).is_empty());
        }
        b.game_time = 115;
        b.match_id = 2;
        assert!(announcements(Some(&a), &b).is_empty());
    }

    #[test]
    fn a_single_kill_is_reported_without_inventing_a_fight_or_first_blood() {
        let a = frame(100);
        let mut b = frame(115);
        b.dire_score = Some(1);
        let message = text(&a, &b);
        assert!(message.contains("**1:55** Dire add one to the board."));
        assert!(!message.contains("fight"));
        assert!(!message.contains("blood"));
        assert!(!message.contains("feed"));
        b.dire_score = Some(4);
        assert!(text(&a, &b).contains("4 unanswered for Dire in the last 15s"));
    }

    #[test]
    fn only_complete_hero_counters_attribute_first_hero_kill() {
        let mut a = frame(100);
        heroes(&mut a);
        let mut b = a.clone();
        b.game_time = 115;
        // Team scores can include uncredited deaths on the other side.
        b.radiant_score = Some(1);
        b.dire_score = Some(1);
        b.players[5].name = "Oracle".into();
        b.players[5].kills = Some(1);
        b.players[0].deaths = Some(1);
        assert!(text(&a, &b).contains("Oracle killed Hero 1 — first hero kill for Dire"));
        assert!(!text(&a, &b).contains("First blood"));
        b.players[9].deaths = None;
        assert!(!text(&a, &b).contains("Oracle"));
        b.players[9].deaths = Some(0);
        b.players[9].hero_id = b.players[8].hero_id;
        assert!(!text(&a, &b).contains("Oracle"));
    }

    #[test]
    fn hero_interval_counts_do_not_claim_a_killer_victim_pair() {
        let mut a = frame(100);
        heroes(&mut a);
        a.players[5].kills = Some(1);
        a.dire_score = Some(1);
        let mut b = a.clone();
        b.game_time = 115;
        b.dire_score = Some(3);
        b.players[5].name = "Oracle".into();
        b.players[5].kills = Some(3);
        b.players[0].name = "Enigma".into();
        b.players[0].deaths = Some(1);
        let message = text(&a, &b);
        assert!(message.contains("Oracle picks up 2 kills"));
        assert!(message.contains("Enigma goes down"));
        assert!(!message.contains("Oracle killed Enigma"));
    }

    #[test]
    fn unique_counter_pair_uses_current_discord_names_and_hero_fallback() {
        let mut a = frame(100);
        heroes(&mut a);
        a.players[0].name = "Skywrath Mage".into();
        a.players[0].kills = Some(1);
        a.players[5].name = "Shadow Fiend".into();
        a.players[5].deaths = Some(1);
        a.radiant_score = Some(1);
        let mut b = a.clone();
        b.game_time += 15;
        b.radiant_score = Some(2);
        b.players[0].kills = Some(2);
        b.players[5].deaths = Some(2);
        b.players[0].display_name = Some("jaso7".into());
        b.players[5].display_name = Some("pf".into());
        assert!(text(&a, &b).contains("jaso7's Skywrath Mage killed pf's Shadow Fiend!"));
        b.players[5].display_name = None;
        assert!(text(&a, &b).contains("jaso7's Skywrath Mage killed Shadow Fiend!"));
        b.players[0].display_name = Some("**@everyone**".into());
        let message = text(&a, &b);
        assert!(message.contains("everyone's Skywrath Mage"));
        assert!(!message.contains('@'));
        b.players[0].display_name = Some("**<>**".into());
        assert!(text(&a, &b).contains("Skywrath Mage killed Shadow Fiend!"));
        b.players[0].account_id = Some(999);
        assert!(!text(&a, &b).contains("Skywrath"));
    }

    #[test]
    fn single_killer_can_resolve_multiple_victims_but_multiple_killers_cannot() {
        let mut a = frame(100);
        heroes(&mut a);
        a.players[0].kills = Some(1);
        a.radiant_score = Some(1);
        let mut b = a.clone();
        b.game_time += 15;
        b.radiant_score = Some(3);
        b.players[0].kills = Some(3);
        b.players[5].deaths = Some(1);
        b.players[6].deaths = Some(1);
        assert!(text(&a, &b).contains("Hero 1 killed Hero 6 and Hero 7!"));
        b.players[0].kills = Some(2);
        b.players[1].kills = Some(1);
        let ambiguous = text(&a, &b);
        assert!(ambiguous.contains("Hero 1 finds a kill; Hero 2 finds a kill"));
        assert!(ambiguous.contains("Hero 6 and Hero 7 go down"));
        assert!(!ambiguous.contains("killed"));
        b.players[0].kills = Some(3);
        b.players[1].kills = Some(0);
        // A further score increment without matching credit prevents pairing.
        b.radiant_score = Some(4);
        assert!(!text(&a, &b).contains("killed"));
        b.radiant_score = Some(3);
        b.players[6].deaths = Some(2);
        assert!(!text(&a, &b).contains("killed"));
    }

    #[test]
    fn explicit_kills_and_aegis_use_current_aliases_without_repeating_counter_summary() {
        let a = frame(100);
        let mut b = frame(115);
        heroes(&mut b);
        b.players[0].name = "Skywrath Mage".into();
        b.players[0].display_name = Some("jaso7".into());
        b.players[5].name = "Shadow Fiend".into();
        b.players[5].display_name = Some("pf".into());
        b.radiant_score = Some(1);
        b.events = vec![
            LiveEvent {
                id: "kill".into(),
                game_time: 110,
                kind: LiveEventKind::HeroKill {
                    killer: "Skywrath Mage".into(),
                    victim: "Shadow Fiend".into(),
                },
            },
            LiveEvent {
                id: "aegis".into(),
                game_time: 111,
                kind: LiveEventKind::Aegis {
                    hero: Some("Skywrath Mage".into()),
                },
            },
        ];
        let message = text(&a, &b);
        assert!(message.contains("jaso7's Skywrath Mage killed pf's Shadow Fiend!"));
        assert!(message.contains("Aegis on jaso7's Skywrath Mage"));
        assert!(!message.contains("add one to the board"));
    }

    #[test]
    fn delay_is_shown_once_per_match_even_after_memory_restore() {
        let a = frame(100);
        let mut b = frame(115);
        b.radiant_score = Some(1);
        let mut memory = AnnouncementMemory::default();
        let first = announcement_message_with_memory(Some(&a), &b, Some(120), &mut memory).unwrap();
        assert!(first.contains("feed 120s behind"));
        memory = serde_json::from_str(&serde_json::to_string(&memory).unwrap()).unwrap();
        let mut c = b.clone();
        c.game_time += 15;
        c.radiant_score = Some(2);
        let next = announcement_message_with_memory(Some(&b), &c, Some(120), &mut memory).unwrap();
        assert!(!next.contains("feed"));
        let mut d = c.clone();
        d.match_id = 2;
        d.game_time += 15;
        assert!(announcement_message_with_memory(Some(&c), &d, Some(120), &mut memory).is_none());
        let mut e = d.clone();
        e.game_time += 15;
        e.radiant_score = Some(3);
        assert!(
            announcement_message_with_memory(Some(&d), &e, Some(120), &mut memory)
                .unwrap()
                .contains("feed 120s behind")
        );
    }

    #[test]
    fn old_events_do_not_starve_current_events_beyond_batch_limit() {
        let a = frame(100);
        let mut b = frame(115);
        b.events = (0..100)
            .map(|id| LiveEvent {
                id: format!("old-{id}"),
                game_time: id,
                kind: LiveEventKind::Roshan { radiant: None },
            })
            .collect();
        b.events.push(LiveEvent {
            id: "current".into(),
            game_time: 110,
            kind: LiveEventKind::HeroKill {
                killer: "Oracle".into(),
                victim: "Enigma".into(),
            },
        });
        assert_eq!(announcements(Some(&a), &b), ["Oracle killed Enigma!"]);
    }

    #[test]
    fn lane_milestones_survive_restart_and_do_not_repeat_at_threshold() {
        let mut a = frame(300);
        a.radiant_net_worth = Some(20_950);
        let mut b = a.clone();
        b.game_time += 15;
        b.radiant_net_worth = Some(21_050);
        let mut memory = AnnouncementMemory::default();
        let first = announcement_message_with_memory(Some(&a), &b, Some(0), &mut memory).unwrap();
        assert!(first.contains("1.1k gold lead in lanes"));
        let saved = serde_json::to_string(&memory).unwrap();
        memory = serde_json::from_str(&saved).unwrap();
        a = b.clone();
        b.game_time += 15;
        b.radiant_net_worth = Some(20_950);
        assert!(announcement_message_with_memory(Some(&a), &b, Some(0), &mut memory).is_none());
        a = b.clone();
        b.game_time += 15;
        b.radiant_net_worth = Some(21_050);
        assert!(announcement_message_with_memory(Some(&a), &b, Some(0), &mut memory).is_none());
        a = b.clone();
        b.game_time += 15;
        b.radiant_net_worth = Some(22_010);
        assert!(
            announcement_message_with_memory(Some(&a), &b, Some(0), &mut memory)
                .unwrap()
                .contains("2k gold lead in lanes")
        );
    }

    #[test]
    fn mid_and_late_game_milestones_use_larger_steps() {
        for (time, old_lead, new_lead, expected) in [
            (900, 1900, 2100, true),
            (900, 2900, 3100, false),
            (1500, 3900, 4100, false),
            (1500, 4900, 5100, true),
        ] {
            let mut a = frame(time);
            a.radiant_net_worth = Some(20_000 + old_lead);
            let mut b = a.clone();
            b.game_time += 15;
            b.radiant_net_worth = Some(20_000 + new_lead);
            assert_eq!(!announcements(Some(&a), &b).is_empty(), expected);
        }
    }

    #[test]
    fn comeback_lead_flip_and_even_are_distinct() {
        let mut a = frame(100);
        a.radiant_net_worth = Some(30_000);
        let mut b = a.clone();
        b.game_time = 115;
        b.dire_net_worth = Some(24_000);
        assert!(text(&a, &b).contains("Dire claw back 4k gold. Still 6k behind"));
        b.dire_net_worth = Some(30_000);
        assert!(text(&a, &b).contains("Back to even"));
        b.dire_net_worth = Some(31_200);
        assert!(text(&a, &b).contains("Dire have flipped it — 1.2k gold ahead"));
        assert!(
            announcement_message(Some(&a), &b, Some(120))
                .unwrap()
                .contains("feed 120s behind")
        );
        assert!(
            announcement_message(Some(&a), &b, None)
                .unwrap()
                .contains("feed delay unknown")
        );
    }

    #[test]
    fn missing_and_invalid_economy_or_score_never_become_zero() {
        let a = frame(100);
        let mut b = frame(115);
        for invalid in [None, Some(-1), Some(i64::MAX)] {
            b.radiant_score = invalid;
            b.dire_score = Some(i64::MAX);
            assert!(announcements(Some(&a), &b).is_empty());
        }
        b.dire_score = None;
        b.radiant_net_worth = Some(30_000);
        b.dire_net_worth = None;
        assert!(announcements(Some(&a), &b).is_empty());
        b.dire_net_worth = Some(-1);
        assert!(announcements(Some(&a), &b).is_empty());
    }

    #[test]
    fn named_structures_require_same_identity_and_are_deduplicated() {
        let a = frame(100);
        let mut b = frame(115);
        b.buildings[0].destroyed = true;
        b.buildings.push(b.buildings[0].clone());
        assert_eq!(
            announcements(Some(&a), &b),
            ["Radiant's bottom tier 1 tower goes down."]
        );
        b.buildings[0].key = "new".into();
        b.buildings.pop();
        assert!(announcements(Some(&a), &b).is_empty());
        b.buildings[0].key = a.buildings[0].key.clone();
        b.buildings[0].radiant = false;
        assert!(announcements(Some(&a), &b).is_empty());
        b.buildings[0].radiant = true;
        b.buildings[0].name = None;
        assert_eq!(announcements(Some(&a), &b), ["Radiant lose a structure."]);
    }

    fn fight() -> LiveEvent {
        LiveEvent {
            id: "fight-1".into(),
            game_time: 110,
            kind: LiveEventKind::TeamFight {
                started_at: 90,
                radiant_kills: 0,
                dire_kills: 3,
                radiant_gold_delta: Some(100),
                dire_gold_delta: Some(1200),
            },
        }
    }

    #[test]
    fn explicit_fight_has_interval_kill_result_and_separately_labeled_gold() {
        let a = frame(100);
        let mut b = frame(115);
        b.dire_score = Some(3);
        b.events.push(fight());
        let lines = announcements(Some(&a), &b);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("Dire take that fight 3–0 on kills (1:30–1:50)"));
        assert!(lines[0].contains("Gold change favors Dire by 1.1k"));
        assert!(!lines[0].contains("gold lead"));
        let LiveEventKind::TeamFight { radiant_kills, .. } = &mut b.events[0].kind else {
            unreachable!()
        };
        *radiant_kills = 3;
        assert!(text(&a, &b).contains("That fight ends 3–3"));
    }

    #[test]
    fn explicit_events_deduplicate_and_must_be_in_current_interval() {
        let a = frame(100);
        let mut b = frame(115);
        b.events = vec![fight(), fight()];
        let mut memory = AnnouncementMemory::default();
        assert_eq!(lines_with_memory(Some(&a), &b, &mut memory).len(), 1);
        assert!(lines_with_memory(Some(&a), &b, &mut memory).is_empty());
        for time in [100, 116] {
            b.events[0].game_time = time;
            b.events[0].id = format!("event-{time}");
            assert!(lines_with_memory(Some(&a), &b, &mut memory).is_empty());
        }
    }

    #[test]
    fn confirmed_objectives_and_winner_render_once_and_sanitize_labels() {
        let a = frame(100);
        let mut b = frame(115);
        b.events = vec![
            LiveEvent {
                id: "rosh".into(),
                game_time: 110,
                kind: LiveEventKind::Roshan {
                    radiant: Some(false),
                },
            },
            LiveEvent {
                id: "aegis".into(),
                game_time: 111,
                kind: LiveEventKind::Aegis {
                    hero: Some("**Visage** @everyone".into()),
                },
            },
        ];
        b.radiant_win = Some(false);
        let mut memory = AnnouncementMemory::default();
        let message = announcement_message_with_memory(Some(&a), &b, Some(0), &mut memory).unwrap();
        assert!(message.contains("Roshan goes to Dire"));
        assert!(message.contains("Aegis on Visage everyone"));
        assert!(message.contains("Dire take the game. GG."));
        assert!(!message.contains('@'));
        assert!(announcement_message_with_memory(Some(&a), &b, Some(0), &mut memory).is_none());
    }

    #[test]
    fn authoritative_first_blood_uses_explicit_credit_once() {
        let a = frame(100);
        let mut b = frame(115);
        b.dire_score = Some(1);
        b.events.push(LiveEvent {
            id: "first-blood".into(),
            game_time: 110,
            kind: LiveEventKind::FirstBlood {
                radiant: false,
                killer: Some("Oracle".into()),
                victim: Some("Enigma".into()),
            },
        });
        let mut memory = AnnouncementMemory::default();
        assert_eq!(
            lines_with_memory(Some(&a), &b, &mut memory),
            ["First blood! Oracle gets Dire started — Enigma goes down."]
        );
        let mut c = b.clone();
        c.game_time += 15;
        c.events[0].game_time += 15;
        c.events[0].id = "renamed-first-blood".into();
        assert!(lines_with_memory(Some(&b), &c, &mut memory).is_empty());
    }

    #[test]
    fn old_serialized_frames_and_memory_remain_readable() {
        let f: LiveAnnouncementFrame = serde_json::from_str(r#"{"match_id":1,"game_time":100,"radiant_score":null,"dire_score":null,"radiant_net_worth":null,"dire_net_worth":null,"buildings":[{"key":"a","radiant":true,"destroyed":false}]}"#).unwrap();
        assert!(f.players.is_empty());
        assert!(f.events.is_empty());
        assert!(f.buildings[0].name.is_none());
        let memory: AnnouncementMemory = serde_json::from_str("{}").unwrap();
        assert!(memory.match_id.is_none());
    }

    #[test]
    fn match_change_and_stale_baseline_do_not_replay_old_milestones() {
        let a = frame(100);
        let mut b = frame(300);
        b.radiant_net_worth = Some(25_000);
        b.events.push(fight());
        let mut memory = AnnouncementMemory::default();
        assert!(lines_with_memory(Some(&a), &b, &mut memory).is_empty());
        let mut c = b.clone();
        c.game_time = 315;
        c.radiant_net_worth = Some(25_100);
        assert!(lines_with_memory(Some(&b), &c, &mut memory).is_empty());
        c.match_id = 2;
        c.radiant_net_worth = Some(21_100);
        assert!(lines_with_memory(Some(&b), &c, &mut memory).is_empty());
        assert_eq!(memory.match_id, Some(2));
        assert_eq!(memory.radiant_gold_milestone, 1000);
    }

    #[test]
    fn oversized_event_batch_remains_within_discord_limit() {
        let a = frame(100);
        let mut b = frame(115);
        for i in 0..64 {
            b.events.push(LiveEvent {
                id: format!("aegis-{i}"),
                game_time: 110,
                kind: LiveEventKind::Aegis {
                    hero: Some("𐐀".repeat(64)),
                },
            });
        }
        let message = text(&a, &b);
        assert!(message.encode_utf16().count() <= 2000);
        assert!(message.contains("details omitted"));
    }
}
