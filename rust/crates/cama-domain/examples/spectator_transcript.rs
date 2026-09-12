//! Offline historical preview through the production announcement formatter.
//! No network, Steam, Discord, database, betting or recording side effects.
//! cargo run --locked -p cama-domain --example spectator_transcript -- HISTORY.json
use cama_domain::live_announcements::{
    ANNOUNCEMENT_INTERVAL_SECONDS, AnnouncementMemory, LiveAnnouncementFrame, LiveBuilding,
    LiveEvent, LiveEventKind, LiveHero, announcement_message_with_memory,
};
use serde::Deserialize;
use std::{collections::BTreeSet, fs::File, io::Read};

#[derive(Deserialize)]
struct History {
    match_id: u64,
    duration: i64,
    radiant_win: bool,
    radiant_score: i64,
    dire_score: i64,
    first_blood_time: i64,
    players: Vec<HistoricalPlayer>,
    team_fights: Vec<HistoricalFight>,
    objective_events: Vec<LiveEvent>,
    score_events: Vec<ScoreEvent>,
    building_events: Vec<BuildingEvent>,
    net_worth_samples: Vec<WorthSample>,
    notes: Vec<String>,
    sources: Vec<Source>,
}
#[derive(Deserialize)]
struct ScoreEvent {
    time: i64,
    radiant: bool,
}
#[derive(Deserialize)]
struct BuildingEvent {
    name: String,
    time: i64,
    key: String,
    radiant: bool,
}
#[derive(Deserialize)]
struct WorthSample {
    time: i64,
    radiant: i64,
    dire: i64,
}
#[derive(Deserialize)]
struct Source {
    name: String,
    url: String,
    sha256: String,
}

#[derive(Deserialize)]
struct HistoricalPlayer {
    account_id: u64,
    hero_id: u32,
    name: String,
    radiant: bool,
    player_slot: u16,
    final_kills: i64,
    final_deaths: i64,
    kill_events: Vec<HeroKill>,
    death_times: Vec<i64>,
}
#[derive(Deserialize)]
struct HeroKill {
    time: i64,
    victim_hero_id: u32,
}
#[derive(Deserialize)]
struct HistoricalFight {
    id: String,
    started_at: i64,
    ended_at: i64,
    players: Vec<FightPlayer>,
}
#[derive(Deserialize)]
struct FightPlayer {
    hero_id: u32,
    deaths: i64,
    kills: i64,
    gold_delta: i64,
}

impl History {
    fn validate(&self) -> Result<(), String> {
        if self.match_id == 0 || !(1..=21_600).contains(&self.duration) {
            return Err("expected a match ID and duration of at most six hours".into());
        }
        if self.score_events.len() > 10_000 || self.building_events.len() > 64 {
            return Err("historical event count exceeds preview limits".into());
        }
        if self
            .score_events
            .iter()
            .any(|e| !(0..=self.duration).contains(&e.time))
            || self
                .building_events
                .iter()
                .any(|e| !(0..=self.duration).contains(&e.time))
        {
            return Err("event lies outside the match timeline".into());
        }
        let radiant = self.score_events.iter().filter(|e| e.radiant).count() as i64;
        if radiant != self.radiant_score
            || self.score_events.len() as i64 - radiant != self.dire_score
        {
            return Err("event counts do not reconcile with the final scoreboard".into());
        }
        let mut keys = BTreeSet::new();
        if self
            .building_events
            .iter()
            .any(|e| e.key.is_empty() || e.name.is_empty() || !keys.insert(&e.key))
        {
            return Err("building identity is missing or repeated".into());
        }
        if self
            .net_worth_samples
            .iter()
            .any(|s| !(0..=self.duration).contains(&s.time) || s.radiant < 0 || s.dire < 0)
            || self
                .net_worth_samples
                .windows(2)
                .any(|s| s[0].time >= s[1].time)
        {
            return Err("net-worth samples must be nonnegative and strictly ordered".into());
        }
        if self.players.len() != 10
            || self.players.iter().filter(|p| p.radiant).count() != 5
            || !(0..=self.duration).contains(&self.first_blood_time)
        {
            return Err(
                "expected ten players with five per side and a valid first-blood clock".into(),
            );
        }
        let mut accounts = BTreeSet::new();
        let mut heroes = BTreeSet::new();
        let mut slots = BTreeSet::new();
        for p in &self.players {
            if p.account_id == 0
                || p.account_id > u64::from(u32::MAX)
                || p.hero_id == 0
                || p.name.is_empty()
                || !accounts.insert(p.account_id)
                || !heroes.insert(p.hero_id)
                || !slots.insert(p.player_slot)
                || (p.radiant && p.player_slot > 4)
                || (!p.radiant && !(128..=132).contains(&p.player_slot))
                || p.final_kills != p.kill_events.len() as i64
                || p.final_deaths != p.death_times.len() as i64
                || p.kill_events.iter().any(|k| {
                    !(0..=self.duration).contains(&k.time)
                        || !self
                            .players
                            .iter()
                            .any(|v| v.hero_id == k.victim_hero_id && v.radiant != p.radiant)
                })
                || p.death_times
                    .iter()
                    .any(|t| !(0..=self.duration).contains(t))
            {
                return Err("player identity, side or historical K/D is inconsistent".into());
            }
        }
        let mut deaths: Vec<_> = self
            .players
            .iter()
            .flat_map(|p| p.death_times.iter().map(move |t| (*t, !p.radiant)))
            .collect();
        let mut scores: Vec<_> = self
            .score_events
            .iter()
            .map(|e| (e.time, e.radiant))
            .collect();
        deaths.sort_unstable();
        scores.sort_unstable();
        if deaths != scores {
            return Err("score events do not match the recorded opponent deaths".into());
        }
        let mut event_ids = BTreeSet::new();
        if self.team_fights.len() > 1_000 || self.objective_events.len() > 1_000 {
            return Err("too many parsed fight/objective events".into());
        }
        for f in &self.team_fights {
            if f.id.is_empty()
                || !event_ids.insert(f.id.as_str())
                || f.started_at < 0
                || f.started_at >= f.ended_at
                || f.ended_at > self.duration
                || f.players.len() != 10
            {
                return Err("invalid parsed fight identity or interval".into());
            }
            let mut fight_heroes = BTreeSet::new();
            let mut totals = [(0i64, 0i64); 2];
            for p in &f.players {
                let Some(hero) = self.players.iter().find(|h| h.hero_id == p.hero_id) else {
                    return Err("fight player is missing from the roster".into());
                };
                if !fight_heroes.insert(p.hero_id)
                    || p.deaths < 0
                    || p.kills < 0
                    || p.gold_delta.unsigned_abs() > 1_000_000_000
                    || p.kills
                        != hero
                            .kill_events
                            .iter()
                            .filter(|e| e.time >= f.started_at && e.time <= f.ended_at)
                            .count() as i64
                    || p.deaths
                        != hero
                            .death_times
                            .iter()
                            .filter(|t| **t >= f.started_at && **t <= f.ended_at)
                            .count() as i64
                {
                    return Err("fight counts do not reconcile with player event logs".into());
                }
                let side = usize::from(!hero.radiant);
                totals[side].0 += p.kills;
                totals[side].1 += p.deaths;
            }
            if totals[0].0 != totals[1].1 || totals[1].0 != totals[0].1 {
                return Err("parsed fight kill credits and opposing deaths disagree".into());
            }
        }
        for e in &self.objective_events {
            if e.id.is_empty()
                || !event_ids.insert(e.id.as_str())
                || !(0..=self.duration).contains(&e.game_time)
            {
                return Err("invalid objective event identity or time".into());
            }
            match &e.kind {
                LiveEventKind::Roshan { .. } => {}
                LiveEventKind::Aegis { hero: Some(name) }
                    if self.players.iter().any(|p| p.name == *name) => {}
                _ => return Err("expected a sourced Roshan or known Aegis recipient".into()),
            }
        }
        Ok(())
    }

    fn frame(&self, time: i64) -> LiveAnnouncementFrame {
        // Never interpolate economy or carry an old value forward as a new
        // measurement. Only exact-time samples may supply net worth.
        let worth = self.net_worth_samples.iter().find(|s| s.time == time);
        let mut frame = LiveAnnouncementFrame {
            match_id: self.match_id,
            game_time: time,
            radiant_score: Some(
                self.score_events
                    .iter()
                    .filter(|e| e.radiant && e.time <= time)
                    .count() as i64,
            ),
            dire_score: Some(
                self.score_events
                    .iter()
                    .filter(|e| !e.radiant && e.time <= time)
                    .count() as i64,
            ),
            radiant_net_worth: worth.map(|s| s.radiant),
            dire_net_worth: worth.map(|s| s.dire),
            buildings: self
                .building_events
                .iter()
                .map(|e| LiveBuilding {
                    key: e.key.clone(),
                    radiant: e.radiant,
                    destroyed: e.time <= time,
                    name: Some(e.name.clone()),
                })
                .collect(),
            players: self
                .players
                .iter()
                .map(|p| LiveHero {
                    hero_id: p.hero_id,
                    account_id: u32::try_from(p.account_id).ok(),
                    display_name: None,
                    name: p.name.clone(),
                    radiant: p.radiant,
                    kills: Some(p.kill_events.iter().filter(|k| k.time <= time).count() as i64),
                    deaths: Some(p.death_times.iter().filter(|t| **t <= time).count() as i64),
                })
                .collect(),
            events: self
                .objective_events
                .iter()
                .filter(|e| e.game_time <= time)
                .cloned()
                .chain(
                    self.team_fights
                        .iter()
                        .filter(|f| f.ended_at <= time)
                        .map(|f| {
                            let mut kills = [0i64; 2];
                            let mut gold = [0i64; 2];
                            for player in &f.players {
                                let hero = self
                                    .players
                                    .iter()
                                    .find(|p| p.hero_id == player.hero_id)
                                    .expect("validated fight roster");
                                let side = usize::from(!hero.radiant);
                                kills[side] += player.kills;
                                gold[side] += player.gold_delta;
                            }
                            LiveEvent {
                                id: f.id.clone(),
                                game_time: f.ended_at,
                                kind: LiveEventKind::TeamFight {
                                    started_at: f.started_at,
                                    radiant_kills: kills[0],
                                    dire_kills: kills[1],
                                    radiant_gold_delta: Some(gold[0]),
                                    dire_gold_delta: Some(gold[1]),
                                },
                            }
                        }),
                )
                .collect(),
            radiant_win: (time == self.duration).then_some(self.radiant_win),
        };
        for player in &self.players {
            for (index, kill) in player
                .kill_events
                .iter()
                .enumerate()
                .filter(|(_, k)| k.time <= time)
            {
                let victim = self
                    .players
                    .iter()
                    .find(|p| p.hero_id == kill.victim_hero_id)
                    .expect("validated victim roster");
                frame.events.push(LiveEvent {
                    id: format!("kill-{}-{index}", player.hero_id),
                    game_time: kill.time,
                    kind: LiveEventKind::HeroKill {
                        killer: player.name.clone(),
                        victim: victim.name.clone(),
                    },
                });
            }
        }
        frame.events.sort_by_key(|e| e.game_time);
        frame
    }

    fn messages(&self) -> Vec<String> {
        let times = (0..=self.duration)
            .step_by(ANNOUNCEMENT_INTERVAL_SECONDS as usize)
            .chain(std::iter::once(self.duration));
        let mut previous = None;
        let mut memory = AnnouncementMemory::default();
        let mut messages = Vec::new();
        for time in times {
            let frame = self.frame(time);
            if let Some(message) =
                announcement_message_with_memory(previous.as_ref(), &frame, Some(0), &mut memory)
            {
                messages.push(message);
            }
            previous = Some(frame);
        }
        messages
    }

    fn transcript(&self) -> String {
        let messages = self.messages();
        let mut text = format!(
            "# Historical spectator simulation — match {}\n\n**Local preview only.** Duration {}:{:02}. {} updates, sampled every 15 seconds.\n\nReconstructed from parsed postgame data; fight and objective details here exceed current live-feed coverage.\n\n## Announcement transcript\n\n",
            self.match_id,
            self.duration / 60,
            self.duration % 60,
            messages.len()
        );
        for message in messages {
            text.push_str(&message);
            text.push_str("\n\n---\n\n");
        }
        text.push_str("## Reconstruction limits\n\nProduction formatting, no simulated feed delay. No Discord messages or Steam connection. The preview does not model Valve availability, wall-clock delivery retries, Discord permissions or betting state.\n\n");
        for note in &self.notes {
            text.push_str(&format!("- {note}\n"));
        }
        text.push('\n');
        text.push_str("## Source provenance\n\n");
        for source in &self.sources {
            text.push_str(&format!(
                "- [{}]({}) — cached response SHA-256 `{}`\n",
                source.name, source.url, source.sha256
            ));
        }
        text
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .ok_or("usage: spectator_transcript HISTORY.json")?;
    if args.next().is_some() {
        return Err("usage: spectator_transcript HISTORY.json".into());
    }
    const LIMIT: u64 = 4 * 1024 * 1024;
    let mut bytes = Vec::new();
    File::open(path)?.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err("historical fixture exceeds 4 MiB".into());
    }
    let history: History = serde_json::from_slice(&bytes)?;
    history.validate()?;
    print!("{}", history.transcript());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn history() -> History {
        serde_json::from_str(include_str!("../tests/fixtures/spectator-8991226826.json")).unwrap()
    }
    #[test]
    fn real_match_reconciles_scores_and_keeps_missing_economy_absent() {
        let h = history();
        h.validate().unwrap();
        let final_frame = h.frame(h.duration);
        assert_eq!(
            (final_frame.radiant_score, final_frame.dire_score),
            (Some(11), Some(44))
        );
        assert!(final_frame.radiant_net_worth.is_none());
        assert!(final_frame.dire_net_worth.is_none());
        assert_eq!(
            final_frame
                .players
                .iter()
                .filter(|p| p.radiant)
                .map(|p| p.kills.unwrap())
                .sum::<i64>(),
            9
        );
    }
    #[test]
    fn objectives_appear_only_after_the_recorded_destruction() {
        let h = history();
        let before = h.frame(756);
        let after = h.frame(757);
        assert!(before.buildings.iter().all(|b| !b.destroyed));
        let message = announcement_message_with_memory(
            Some(&before),
            &after,
            Some(0),
            &mut AnnouncementMemory::default(),
        )
        .unwrap();
        assert!(
            message
                .to_lowercase()
                .contains("radiant's bottom tier 1 tower")
        );
        assert!(!message.to_lowercase().contains("dire's top tier 1 tower"));
    }
    #[test]
    fn mismatched_totals_and_out_of_range_events_are_rejected() {
        let mut h = history();
        h.score_events.pop();
        assert!(h.validate().is_err());
        let mut h = history();
        h.building_events[0].time = h.duration + 1;
        assert!(h.validate().is_err());
    }
    #[test]
    fn old_economy_samples_are_not_presented_as_fresh_measurements() {
        let mut h = history();
        h.net_worth_samples.push(WorthSample {
            time: 60,
            radiant: 1000,
            dire: 1100,
        });
        assert_eq!(h.frame(60).radiant_net_worth, Some(1000));
        assert_eq!(h.frame(90).radiant_net_worth, None);
    }
    #[test]
    fn complete_roster_matches_both_sources_and_preserves_kill_credit_difference() {
        let h = history();
        h.validate().unwrap();
        let expected = [
            (69576061, 101, true),
            (154390881, 54, true),
            (862668582, 2, true),
            (123736773, 33, true),
            (156404497, 5, true),
            (33293249, 21, false),
            (177837390, 92, false),
            (11758567, 96, false),
            (87584299, 111, false),
            (87626955, 67, false),
        ];
        assert_eq!(
            h.players
                .iter()
                .map(|p| (p.account_id, p.hero_id, p.radiant))
                .collect::<Vec<_>>(),
            expected
        );
        let early = h.frame(157);
        assert_eq!(early.radiant_score, Some(1));
        assert!(early.players.iter().all(|p| p.kills == Some(0)));
        let first_credit = h.frame(158);
        assert_eq!(
            first_credit
                .players
                .iter()
                .filter(|p| p.kills == Some(1))
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["Oracle"]
        );
        assert_eq!(h.first_blood_time, 157);
        assert!(
            !h.frame(h.duration)
                .events
                .iter()
                .any(|e| matches!(e.kind, LiveEventKind::FirstBlood { .. }))
        );
    }

    #[test]
    fn every_credited_kill_preserves_its_source_victim_and_time() {
        let h = history();
        let frame = h.frame(h.duration);
        let kills = frame
            .events
            .iter()
            .filter(|e| matches!(e.kind, LiveEventKind::HeroKill { .. }))
            .count();
        assert_eq!(
            kills,
            h.players.iter().map(|p| p.kill_events.len()).sum::<usize>()
        );
        assert_eq!(kills, 53);
        assert!(
            frame
                .events
                .windows(2)
                .all(|events| events[0].game_time <= events[1].game_time)
        );
        let transcript = h.messages().join("\n");
        for player in &h.players {
            for (index, kill) in player.kill_events.iter().enumerate() {
                let victim = h
                    .players
                    .iter()
                    .find(|p| p.hero_id == kill.victim_hero_id)
                    .unwrap();
                let event_id = format!("kill-{}-{index}", player.hero_id);
                assert!(
                    !h.frame(kill.time - 1)
                        .events
                        .iter()
                        .any(|e| e.id == event_id)
                );
                let event = frame.events.iter().find(|e| e.id == event_id).unwrap();
                assert_eq!(event.game_time, kill.time);
                assert_eq!(
                    event.kind,
                    LiveEventKind::HeroKill {
                        killer: player.name.clone(),
                        victim: victim.name.clone(),
                    }
                );
                assert!(transcript.contains(&format!("{} killed {}!", player.name, victim.name)));
            }
        }
    }

    #[test]
    fn every_parsed_fight_appears_at_completion_with_measured_kills_and_segment_gold() {
        let h = history();
        let expected = [
            (228, 277, 0, 3, 347, 1479),
            (277, 329, 1, 2, 741, 1150),
            (927, 964, 1, 2, 1237, 1544),
            (1135, 1171, 0, 4, -333, 2650),
            (1345, 1384, 0, 3, 5, 2289),
            (1512, 1548, 0, 3, 775, 1790),
            (1902, 1954, 0, 5, -1144, 5931),
        ];
        assert_eq!(h.team_fights.len(), expected.len());
        for (fight, (start, end, rk, dk, rg, dg)) in h.team_fights.iter().zip(expected) {
            assert!(!h.frame(end - 1).events.iter().any(|e| e.id == fight.id));
            let frame = h.frame(end);
            let event = frame.events.iter().find(|e| e.id == fight.id).unwrap();
            assert_eq!(
                event.kind,
                LiveEventKind::TeamFight {
                    started_at: start,
                    radiant_kills: rk,
                    dire_kills: dk,
                    radiant_gold_delta: Some(rg),
                    dire_gold_delta: Some(dg),
                }
            );
        }
    }

    #[test]
    fn explicit_roshan_and_aegis_and_terminal_result_do_not_leak_early() {
        let h = history();
        assert!(
            !h.frame(1743)
                .events
                .iter()
                .any(|e| matches!(e.kind, LiveEventKind::Roshan { .. }))
        );
        assert!(h.frame(1744).events.iter().any(|e| matches!(
            e.kind,
            LiveEventKind::Roshan {
                radiant: Some(false)
            }
        )));
        assert!(h.frame(1745).events.iter().any(
            |e| matches!(&e.kind, LiveEventKind::Aegis { hero: Some(name) } if name == "Visage")
        ));
        for t in 0..h.duration {
            assert_eq!(h.frame(t).radiant_win, None);
        }
        assert_eq!(h.frame(h.duration).radiant_win, Some(false));
    }

    #[test]
    fn corrupt_identity_and_fight_credits_are_rejected() {
        let mut h = history();
        h.players[0].radiant = false;
        assert!(h.validate().is_err());
        let mut h = history();
        h.team_fights[0].players[0].kills = 1;
        assert!(h.validate().is_err());
        let mut h = history();
        h.team_fights[0].players[0].hero_id = 9999;
        assert!(h.validate().is_err());
    }
    #[test]
    fn fifteen_second_transcript_includes_every_parsed_fight_and_one_terminal_winner() {
        let h = history();
        let messages = h.messages();
        for fight in &h.team_fights {
            let interval = format!(
                "{}:{:02}–{}:{:02}",
                fight.started_at / 60,
                fight.started_at % 60,
                fight.ended_at / 60,
                fight.ended_at % 60
            );
            assert_eq!(messages.iter().filter(|m| m.contains(&interval)).count(), 1);
        }
        assert!(messages[0].starts_with("**2:45**"));
        assert!(messages[0].contains("Oracle killed Enigma!"));
        assert!(
            messages
                .iter()
                .all(|m| !m.to_lowercase().contains("first blood"))
        );
        assert!(messages.iter().any(|m| m.contains("Aegis on Visage")));
        assert!(messages.iter().any(|m| m.contains("Roshan goes to Dire")));
        let last = messages.last().unwrap();
        assert!(last.starts_with("**32:41**"));
        assert!(last.contains("Dire take the game"));
        assert!(
            messages[..messages.len() - 1]
                .iter()
                .all(|m| !m.contains("take the game"))
        );
    }
}
