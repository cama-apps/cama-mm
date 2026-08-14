//! Typed Neon Degen/JOPA-T policies for the side-by-side Rust application.
//!
//! The module deliberately keeps Discord, SQLite, image encoding, and model
//! providers behind data contracts.  That lets the Rust service reproduce the
//! Python trigger, privacy, persistence, and upload policies without starting a
//! second bot process or becoming a second schema owner.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

use cama_domain::openskill::CamaOpenSkillSystem;
use gif::{Encoder, Frame, Repeat};

use crate::dig_neon::{BigWinFlavor, BigWinSource};
use crate::jopat_post_match::{
    JopatPostMatchContext, SeededJopatChoice, choose_protocol, render_fallback,
};
use crate::neon_bigwin_media::render_big_win;

const DISCORD_UPLOAD_LIMIT: usize = 4 * 1024 * 1024;
const DEFAULT_COOLDOWN_SECONDS: u64 = 60;

#[must_use]
pub fn ansi_block(text: &str) -> String {
    format!("```ansi\n{text}\n```")
}

#[must_use]
pub fn ascii_box(lines: &[&str], width: usize) -> String {
    let width = width.max(2);
    let border = format!("+{}+", "-".repeat(width - 2));
    let mut output = vec![border.clone()];
    for line in lines {
        let visible = strip_sgr(line).chars().count();
        output.push(format!(
            "|{line}{}|",
            " ".repeat(width.saturating_sub(2 + visible))
        ));
    }
    output.push(border);
    output.join("\n")
}

fn strip_sgr(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for code in chars.by_ref() {
                if code == 'm' {
                    break;
                }
            }
        } else {
            output.push(character);
        }
    }
    output
}

/// Small dependency-free entropy source. Runtime adapters should seed it from
/// operating-system entropy; tests can inject exact roll samples.
#[derive(Clone, Debug)]
pub struct RollSource {
    state: u64,
    queued: VecDeque<f64>,
    observed_chances: Vec<f64>,
}

impl RollSource {
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        Self {
            state: seed.max(1),
            queued: VecDeque::new(),
            observed_chances: Vec::new(),
        }
    }

    pub fn queue(&mut self, samples: impl IntoIterator<Item = f64>) {
        self.queued.extend(samples);
    }

    fn sample(&mut self) -> f64 {
        if let Some(sample) = self.queued.pop_front() {
            return sample.clamp(0.0, 1.0);
        }
        let mut state = self.state;
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.state = state;
        (state as f64) / (u64::MAX as f64)
    }

    fn roll(&mut self, chance: f64) -> bool {
        self.observed_chances.push(chance);
        self.sample() < chance
    }

    #[must_use]
    pub fn observed_chances(&self) -> &[f64] {
        &self.observed_chances
    }
}

#[must_use]
pub fn corrupt_text(text: &str, intensity: f64, random: &mut RollSource) -> String {
    const GLITCHES: &[char] = &[
        '@', '#', '$', '%', '&', '*', '!', '?', '~', '^', '|', '/', '\\', '<', '>', '{', '}', '[',
        ']',
    ];
    let intensity = intensity.clamp(0.0, 1.0);
    text.chars()
        .map(|character| {
            if character == ' ' || random.sample() >= intensity {
                character
            } else {
                let index = (random.sample() * GLITCHES.len() as f64) as usize;
                GLITCHES[index.min(GLITCHES.len() - 1)]
            }
        })
        .collect()
}

fn terminal_lines(lines: &[String]) -> String {
    ansi_block(&lines.join("\n"))
}

#[must_use]
pub fn render_balance_check(name: &str, balance: i64) -> String {
    terminal_lines(&[
        "[JOPA-T] CREDIT CHECK".into(),
        format!("Subject: {name}"),
        format!("Balance: {balance} JC"),
        "Risk profile: INADVISABLE".into(),
    ])
}

#[must_use]
pub fn render_bet_placed(amount: i64, team: &str, leverage: u8) -> String {
    terminal_lines(&[
        "[JOPA-T] WAGER ACCEPTED".into(),
        format!("Position: {amount} JC on {team}"),
        format!("Leverage: {leverage}x"),
    ])
}

#[must_use]
pub fn render_all_in_bet(name: &str, amount: i64, percentage: f64) -> String {
    terminal_lines(&[
        "ALL-IN DETECTED".into(),
        format!("Subject: {name}"),
        format!("Amount: {amount} JC"),
        format!("Portfolio exposure: {percentage:.0}%"),
        "Classification: TERMINAL".into(),
        "Risk assessment: VIOLENCE".into(),
        "Commitment level: MAXIMUM".into(),
        "Client chose violence.".into(),
    ])
}

#[must_use]
pub fn render_last_second_bet(name: &str, seconds_remaining: i64) -> String {
    terminal_lines(&[
        "BUZZER BEATER DETECTED".into(),
        format!("Subject: {name}"),
        format!("Time remaining: {seconds_remaining}s"),
        "Pattern: LAST_SECOND".into(),
        "Classification: INFORMATION OR DESPERATION".into(),
        "Probably desperation.".into(),
    ])
}

#[must_use]
pub fn render_first_leverage(name: &str, leverage: u8) -> String {
    terminal_lines(&[
        "[JOPA-T] MARGIN_ACCOUNT_OPENED".into(),
        format!("Client: {name}"),
        format!("First leverage: {leverage}x"),
        "Status: WELCOME TO THE DANGER ZONE".into(),
    ])
}

#[must_use]
pub fn render_bets_milestone(name: &str, total_bets: i64) -> String {
    terminal_lines(&[
        "BETTING CENTENNIAL".into(),
        format!("Subject: {name}"),
        format!("Total wagers: {total_bets}"),
        "Classification: COMMITTED".into(),
        format!("Milestone: {total_bets} bets"),
        "Status: TOO DEEP TO STOP".into(),
        "One hundred bets. Zero taken.".into(),
    ])
}

#[must_use]
pub fn render_tip(sender: &str, recipient: &str, amount: i64) -> String {
    terminal_lines(&[
        "[JOPA-T] TRANSFER OBSERVED".into(),
        format!("Sender: {sender}"),
        format!("Recipient: {recipient}"),
        format!("Amount: {amount} JC"),
    ])
}

#[must_use]
pub fn render_tip_surveillance(sender: &str, recipient: &str, amount: i64, fee: i64) -> String {
    terminal_lines(&[
        "SOCIAL SURVEILLANCE REPORT".into(),
        format!("Sender: {sender}"),
        format!("Recipient: {recipient}"),
        format!("Amount: {amount} JC"),
        format!("Reserve fee: {fee} JC"),
    ])
}

#[must_use]
pub fn render_loan_taken(amount: i64, total_owed: i64) -> String {
    terminal_lines(&[
        "[JOPA-T] CREDIT FACILITY".into(),
        format!("Principal: {amount} JC"),
        format!("Total owed: {total_owed} JC"),
    ])
}

#[must_use]
pub fn render_cooldown_hit(command: &str) -> String {
    terminal_lines(&[
        "[JOPA-T] REQUEST DENIED".into(),
        format!("Protocol {command} remains on cooldown."),
    ])
}

#[must_use]
pub fn render_match_recorded() -> String {
    terminal_lines(&[
        "[JOPA-T] MATCH DEBRIEF".into(),
        "Replay ledger accepted.".into(),
    ])
}

#[must_use]
pub fn render_bankruptcy_filing(name: &str, debt: i64, filing_number: u32) -> String {
    terminal_lines(&[
        "BANKRUPTCY FILING ACCEPTED".into(),
        format!("Client: {name}"),
        format!("Debt cleared: {debt} JC"),
        format!("Filing number: {filing_number}"),
    ])
}

#[must_use]
pub fn render_debt_collector(name: &str, debt: i64) -> String {
    terminal_lines(&[
        "DEBT COLLECTION PROTOCOL".into(),
        format!("Debtor: {name}"),
        format!("Exposure: {debt} JC"),
    ])
}

#[must_use]
pub fn render_system_breach(name: &str) -> String {
    terminal_lines(&[
        "SYSTEM BREACH".into(),
        format!("Client {name} reached the debt floor."),
    ])
}

#[must_use]
pub fn render_balance_zero(name: &str) -> String {
    terminal_lines(&[
        "BALANCE ZERO".into(),
        format!("Client {name} has no remaining collateral."),
    ])
}

#[must_use]
pub fn render_streak(name: &str, streak: u32, is_win: bool) -> String {
    let status = if is_win {
        "WIN STREAK HOT"
    } else {
        "LOSS STREAK ANOMALY"
    };
    terminal_lines(&[
        status.into(),
        format!("Client: {name}"),
        format!("Run: {streak}"),
    ])
}

#[must_use]
pub fn render_negative_loan(name: &str, amount: i64, new_debt: i64) -> String {
    terminal_lines(&[
        "RECURSIVE DEBT DETECTED".into(),
        format!("Client: {name}"),
        format!("Loan: {amount} JC. Debt: {new_debt} JC"),
    ])
}

#[must_use]
pub fn render_wheel_bankrupt(name: &str, loss: i64) -> String {
    terminal_lines(&[
        "WHEEL BANKRUPT OUTCOME".into(),
        format!("Client: {name}"),
        format!("Loss: {} JC", loss.abs()),
    ])
}

#[must_use]
pub fn render_lightning_bolt(total: i64, players: usize, overlay: bool) -> String {
    if overlay {
        terminal_lines(&[
            "LIGHTNING TAX ASSESSMENT".into(),
            format!("Accounts levied: {players}"),
            format!("Total extracted: {total} JC"),
            "Destination: NONPROFIT_FUND".into(),
            "The people suffer quietly. The fund grows.".into(),
        ])
    } else {
        terminal_lines(&[
            "[JOPA-T] LIGHTNING_EVENT processed.".into(),
            format!("{players} accounts taxed. {total} JC redistributed to nonprofit."),
        ])
    }
}

#[must_use]
pub fn render_don_win(name: &str, balance: i64) -> String {
    terminal_lines(&[
        "DOUBLE OR NOTHING: WIN".into(),
        format!("Client: {name}"),
        format!("Balance: {balance} JC"),
    ])
}

#[must_use]
pub fn render_don_lose(name: &str, risk: i64) -> String {
    terminal_lines(&[
        "DOUBLE OR NOTHING: LOSS".into(),
        format!("Client: {name}"),
        format!("Risk liquidated: {risk} JC"),
    ])
}

#[must_use]
pub fn render_don_loss_box(name: &str, risk: i64) -> String {
    terminal_lines(&[
        "DOUBLE OR NOTHING SETTLEMENT".into(),
        ascii_box(
            &[&format!("Client: {name}"), &format!("Loss: {risk} JC")],
            38,
        ),
    ])
}

#[must_use]
pub fn render_coinflip(winner: &str, loser: &str) -> String {
    terminal_lines(&[
        "DRAFT COINFLIP".into(),
        format!("Winner: {winner}"),
        format!("Loser: {loser}"),
    ])
}

#[must_use]
pub fn render_bomb_pot(pool: i64, contributors: i64) -> String {
    terminal_lines(&[
        "BOMB POT DETONATED".into(),
        "==============================".into(),
        "Type: MANDATORY CONTRIBUTION".into(),
        format!("Pool size: {pool} JC"),
        format!("Victims: {contributors}"),
        "==============================".into(),
        "Consent: NOT REQUIRED".into(),
        "Escape: IMPOSSIBLE".into(),
    ])
}

#[must_use]
pub fn render_captain_symmetry(captain1: &str, captain2: &str, diff: i64) -> String {
    terminal_lines(&[
        "CAPTAIN SYMMETRY".into(),
        format!("{captain1} vs {captain2}"),
        format!("Rating delta: {diff}"),
        "Outcome: uncertain".into(),
    ])
}

#[must_use]
pub fn render_registration(name: &str) -> String {
    terminal_lines(&["CLIENT REGISTRATION".into(), format!("New subject: {name}")])
}

#[must_use]
pub fn render_lobby_join(name: &str, queue_position: usize) -> String {
    terminal_lines(&[
        "QUEUE JOIN".into(),
        format!("Client: {name}"),
        format!("Position: {queue_position}"),
        "The countdown begins.".into(),
    ])
}

#[must_use]
pub fn render_gamba_spectator(name: &str) -> String {
    terminal_lines(&[
        "SPECTATOR MODE".into(),
        format!("Client: {name}"),
        "Not playing. Just watching.".into(),
    ])
}

#[must_use]
pub fn render_prediction_resolved(question: &str, outcome: &str, total_pool: i64) -> String {
    terminal_lines(&[
        "PREDICTION RESOLVED".into(),
        format!("Market: {question}"),
        format!("Outcome: {outcome}. Pool: {total_pool} JC"),
    ])
}

#[must_use]
pub fn render_prediction_market_crash(
    question: &str,
    total_pool: i64,
    outcome: &str,
    winners: u32,
    losers: u32,
) -> String {
    terminal_lines(&[
        "MARKET SETTLEMENT CRASH".into(),
        format!("Market: {question}"),
        format!("Outcome: {outcome}. Pool: {total_pool} JC"),
        format!("Winners: {winners}. Losers: {losers}."),
    ])
}

#[must_use]
pub fn render_soft_avoid(cost: i64, games: u32) -> String {
    terminal_lines(&[
        "SOFT AVOID PURCHASED".into(),
        format!("Anonymous cost: {cost} JC. Duration: {games} games."),
    ])
}

#[must_use]
pub fn render_soft_avoid_surveillance(cost: i64, games: u32) -> String {
    terminal_lines(&[
        "SOCIAL SURVEILLANCE".into(),
        format!("Avoid window: {games} games."),
        format!("Anonymous settlement: {cost} JC."),
    ])
}

#[must_use]
pub fn render_rivalry_detected(player1: &str, player2: &str, games: u32, winrate: f64) -> String {
    terminal_lines(&[
        "RIVALRY DETECTED".into(),
        format!("{player1} vs {player2}"),
        format!("Games against: {games}"),
        format!("Win rate: {winrate:.1}%"),
    ])
}

/// Encoded animation contract passed to the future Discord attachment adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GifAsset {
    pub kind: &'static str,
    pub bytes: Vec<u8>,
    pub frame_durations_ms: Vec<u32>,
    pub shared_palette: bool,
}

impl GifAsset {
    #[must_use]
    pub fn terminal_crash() -> Self {
        let mut durations = vec![120; 10];
        durations.extend(vec![145; 46]);
        durations.push(230);
        durations.push(60_000);
        render_neon_animation("terminal_crash", &durations, 0x7e7e, |frame, canvas| {
            if frame < 10 {
                canvas.text_centered("JOPA-T/v3.7", 70, COLOR_GREEN, 2);
                canvas.text_centered("SYSTEM FAILURE", 120, COLOR_RED, 2);
                canvas.text_centered("RECONCILING...", 170, COLOR_YELLOW, 1);
            } else if frame < 56 {
                canvas.text_centered(
                    if frame.is_multiple_of(3) {
                        "KERNEL PANIC"
                    } else if frame.is_multiple_of(2) {
                        "FATAL ERROR"
                    } else {
                        "TERMINAL UNRESPONSIVE"
                    },
                    120,
                    COLOR_RED,
                    2,
                );
                canvas.text_centered("BANKRUPTCY PROTOCOL", 165, COLOR_YELLOW, 1);
            } else {
                canvas.text_left("> JOPA-T/v3.7 REBOOTING...", 20, 45, COLOR_GREEN, 1);
                canvas.text_left("> LEDGER INTEGRITY... OK", 20, 75, COLOR_DIM_GREEN, 1);
                canvas.text_left("> SYSTEM ENDURES.", 20, 120, COLOR_GREEN, 1);
                canvas.text_left("> IT ALWAYS DOES.", 20, 145, COLOR_DIM_GREEN, 1);
            }
        })
    }

    #[must_use]
    pub fn upload_safe(&self) -> bool {
        self.bytes.starts_with(b"GIF")
            && !self.bytes.is_empty()
            && self.bytes.len() < DISCORD_UPLOAD_LIMIT
    }
}

const NEON_GIF_WIDTH: u16 = 400;
const NEON_GIF_HEIGHT: u16 = 300;
const NEON_GIF_PIXELS: usize = NEON_GIF_WIDTH as usize * NEON_GIF_HEIGHT as usize;
const COLOR_BLACK: u8 = 0;
const COLOR_GREEN: u8 = 1;
const COLOR_CYAN: u8 = 2;
const COLOR_RED: u8 = 4;
const COLOR_YELLOW: u8 = 5;
const COLOR_DIM_GREEN: u8 = 6;
const COLOR_DIM_CYAN: u8 = 7;
const COLOR_DARK_RED: u8 = 8;

// The Python generators draw a CRT-black terminal with a small fixed set of
// neon accents. Keeping one global palette makes these generated attachments
// seekable and compact, while still allowing the authored scenes to vary with
// every user/economy input.
const NEON_GIF_PALETTE: &[u8] = &[
    10, 10, 15, 0, 255, 65, 0, 255, 255, 255, 0, 128, 255, 30, 30, 255, 220, 0, 0, 120, 30, 0, 100,
    100, 80, 0, 0,
];

#[derive(Clone, Debug)]
struct NeonCanvas {
    pixels: Vec<u8>,
}

impl NeonCanvas {
    fn new() -> Self {
        Self {
            pixels: vec![COLOR_BLACK; NEON_GIF_PIXELS],
        }
    }

    fn fill(&mut self, color: u8) {
        self.pixels.fill(color);
    }

    fn pixel(&mut self, x: i32, y: i32, color: u8) {
        if (0..i32::from(NEON_GIF_WIDTH)).contains(&x)
            && (0..i32::from(NEON_GIF_HEIGHT)).contains(&y)
        {
            self.pixels[y as usize * usize::from(NEON_GIF_WIDTH) + x as usize] = color;
        }
    }

    fn line(&mut self, mut x0: i32, mut y0: i32, x1: i32, y1: i32, color: u8) {
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;
        loop {
            self.pixel(x0, y0, color);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let twice = 2 * error;
            if twice >= dy {
                error += dy;
                x0 += sx;
            }
            if twice <= dx {
                error += dx;
                y0 += sy;
            }
        }
    }

    fn rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: u8) {
        self.line(left, top, right, top, color);
        self.line(right, top, right, bottom, color);
        self.line(right, bottom, left, bottom, color);
        self.line(left, bottom, left, top, color);
    }

    fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: u8) {
        for y in top..=bottom {
            self.line(left, y, right, y, color);
        }
    }

    fn text_left(&mut self, text: &str, x: i32, y: i32, color: u8, scale: i32) {
        let mut cursor = x;
        for character in text.chars() {
            self.glyph(character, cursor, y, color, scale);
            cursor += 6 * scale;
        }
    }

    fn text_centered(&mut self, text: &str, y: i32, color: u8, scale: i32) {
        let width = text.chars().count() as i32 * 6 * scale;
        self.text_left(
            text,
            (i32::from(NEON_GIF_WIDTH) - width) / 2,
            y,
            color,
            scale,
        );
    }

    fn glyph(&mut self, character: char, left: i32, top: i32, color: u8, scale: i32) {
        let rows = glyph_rows(character);
        for (row, bits) in rows.iter().copied().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) != 0 {
                    self.fill_rect(
                        left + column * scale,
                        top + row as i32 * scale,
                        left + (column + 1) * scale - 1,
                        top + (row as i32 + 1) * scale - 1,
                        color,
                    );
                }
            }
        }
    }
}

fn glyph_rows(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0F, 0x10, 0x10, 0x10, 0x10, 0x10, 0x0F],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0F, 0x10, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x1F],
        'J' => [0x01, 0x01, 0x01, 0x01, 0x11, 0x11, 0x0E],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1E, 0x01, 0x01, 0x0E, 0x01, 0x01, 0x1E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x10, 0x1E, 0x01, 0x01, 0x1E],
        '6' => [0x0E, 0x10, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x01, 0x0E],
        ':' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '/' => [0x01, 0x02, 0x04, 0x08, 0x10, 0x00, 0x00],
        '#' => [0x0A, 0x1F, 0x0A, 0x0A, 0x1F, 0x0A, 0x00],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        '?' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
        _ => [0; 7],
    }
}

fn score_color(score: i64) -> u8 {
    if score < 60 {
        COLOR_GREEN
    } else if score < 80 {
        COLOR_YELLOW
    } else {
        COLOR_RED
    }
}

fn scene_seed<T: std::hash::Hash + ?Sized>(value: &T) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn render_neon_animation<F>(
    kind: &'static str,
    durations: &[u32],
    seed: u64,
    mut draw: F,
) -> GifAsset
where
    F: FnMut(usize, &mut NeonCanvas),
{
    let mut bytes = Vec::new();
    {
        let mut encoder = Encoder::new(
            &mut bytes,
            NEON_GIF_WIDTH,
            NEON_GIF_HEIGHT,
            NEON_GIF_PALETTE,
        )
        .expect("fixed Neon palette is valid");
        encoder
            .set_repeat(Repeat::Finite(1))
            .expect("Neon GIF repeat marker is valid");
        for (index, duration_ms) in durations.iter().copied().enumerate() {
            let mut canvas = NeonCanvas::new();
            draw(index, &mut canvas);
            // Deterministic phosphor speckles make otherwise identical scene
            // phases visibly animate without adding a second palette.
            if index % 3 == 0 {
                let mut state = seed.wrapping_add(index as u64 * 0x9e37_79b9);
                for _ in 0..8 {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    canvas.pixel(
                        (state % 390 + 5) as i32,
                        ((state >> 16) % 280 + 10) as i32,
                        COLOR_DIM_GREEN,
                    );
                }
            }
            let mut frame = Frame {
                width: NEON_GIF_WIDTH,
                height: NEON_GIF_HEIGHT,
                delay: u16::try_from(duration_ms / 10).expect("Neon GIF delay fits centiseconds"),
                buffer: Cow::Owned(canvas.pixels),
                ..Frame::default()
            };
            frame.dispose = gif::DisposalMethod::Keep;
            encoder
                .write_frame(&frame)
                .expect("Neon GIF frame is valid");
        }
    }
    GifAsset {
        kind,
        bytes,
        frame_durations_ms: durations.to_vec(),
        shared_palette: true,
    }
}

#[must_use]
pub fn create_void_welcome_gif(name: &str) -> GifAsset {
    let mut durations = vec![150; 10];
    durations.extend([400; 8]);
    durations.extend([200; 14]);
    durations.push(60_000);
    render_neon_animation(
        "void_welcome",
        &durations,
        scene_seed(name),
        |frame, canvas| {
            if frame < 10 {
                canvas.text_left("> _", 20, 140, COLOR_DIM_GREEN, 2);
                return;
            }
            if frame < 18 {
                let lines = [
                    "> INITIALIZING...",
                    "> NEW CLIENT DETECTED",
                    &format!("> IDENTITY: {name}"),
                    "> FIRST BANKRUPTCY RECORDED",
                    "> Welcome to the void.",
                    "> The system sees you now.",
                    "> There is no going back.",
                ];
                for (index, line) in lines.iter().enumerate().take(frame - 9) {
                    canvas.text_left(line, 20, 30 + index as i32 * 20, COLOR_DIM_GREEN, 1);
                }
                return;
            }
            canvas.text_left("> new client accepted", 20, 30, COLOR_DIM_GREEN, 1);
            canvas.text_left(&format!("> identity: {name}"), 20, 50, COLOR_CYAN, 1);
            canvas.text_centered("DEBTOR #1", 210, COLOR_GREEN, 3);
            canvas.text_centered("CLASSIFICATION: FRESH", 250, COLOR_DIM_GREEN, 1);
            if frame.is_multiple_of(2) {
                canvas.rect(18, 18, 382, 282, COLOR_DIM_GREEN);
            }
        },
    )
}

#[must_use]
pub fn create_debt_collector_gif(name: &str, debt: i64) -> GifAsset {
    let durations = (0..40)
        .map(|frame| if frame == 39 { 60_000 } else { 80 })
        .collect::<Vec<_>>();
    render_neon_animation(
        "debt_collector",
        &durations,
        scene_seed(&(name, debt)),
        |frame, canvas| {
            let scan_y = (frame as i32 * 300 / 40).min(299);
            canvas.line(0, scan_y, 399, scan_y, COLOR_RED);
            canvas.line(
                0,
                (scan_y - 2).max(0),
                399,
                (scan_y - 2).max(0),
                COLOR_DARK_RED,
            );
            if scan_y > 40 {
                canvas.text_centered("DEBT COLLECTION", 30, COLOR_RED, 2);
            }
            if scan_y > 70 {
                canvas.text_centered("==============================", 55, COLOR_RED, 1);
            }
            if scan_y > 100 {
                canvas.text_left(&format!("DEBTOR: {name}"), 20, 85, COLOR_RED, 1);
            }
            if scan_y > 130 {
                canvas.text_left(&format!("AMOUNT: {debt} JC"), 20, 105, COLOR_RED, 1);
            }
            if scan_y > 160 {
                canvas.text_left("STATUS: MAXIMUM DEBT", 20, 125, COLOR_RED, 1);
            }
            if scan_y > 190 {
                canvas.text_left("ACTION: GARNISHMENT", 20, 145, COLOR_RED, 1);
            }
            if scan_y > 245 {
                canvas.text_centered("ALL WINNINGS SEIZED", 200, COLOR_YELLOW, 2);
            }
        },
    )
}

#[must_use]
pub fn create_freefall_gif(name: &str, start_balance: i64, end_balance: i64) -> GifAsset {
    let durations = (0..45)
        .map(|frame| {
            if frame == 44 {
                60_000
            } else {
                let nominal = 60 + frame as u32 * 80 / 44;
                (nominal / 10) * 10
            }
        })
        .collect::<Vec<_>>();
    render_neon_animation(
        "freefall",
        &durations,
        scene_seed(&(name, start_balance, end_balance)),
        |frame, canvas| {
            let progress = frame as i64 * 1_000 / 44;
            let current = start_balance + (end_balance - start_balance) * progress / 1_000;
            canvas.text_centered("BALANCE UPDATE", 15, COLOR_RED, 1);
            canvas.text_left(&format!("CLIENT: {name}"), 20, 40, COLOR_DIM_GREEN, 1);
            let color = if progress < 500 {
                COLOR_GREEN
            } else {
                COLOR_RED
            };
            canvas.text_centered(&current.to_string(), 120, color, 4);
            canvas.text_centered("JC", 175, COLOR_DIM_GREEN, 2);
            for trail in 1..=frame.min(8) {
                let value = start_balance
                    + (end_balance - start_balance) * (progress - trail as i64 * 20).max(0) / 1_000;
                canvas.text_centered(
                    &value.to_string(),
                    175 + trail as i32 * 18,
                    COLOR_DARK_RED,
                    1,
                );
            }
            if progress > 800 {
                let status = if end_balance == 0 {
                    "FINAL: ZERO"
                } else {
                    "FINAL: DEBT"
                };
                canvas.text_centered(status, 260, COLOR_RED, 1);
            }
        },
    )
}

#[must_use]
pub fn create_degen_certificate_gif(name: &str, score: i64) -> GifAsset {
    let mut durations = vec![60; 25];
    durations.extend([200; 9]);
    durations.push(60_000);
    render_neon_animation(
        "degen_certificate",
        &durations,
        scene_seed(&(name, score)),
        |frame, canvas| {
            if frame < 20 {
                let shown = score.saturating_mul(frame as i64) / 19;
                canvas.text_centered("DEGEN SCORE ANALYSIS", 20, COLOR_DIM_GREEN, 1);
                canvas.text_centered(&format!("SUBJECT: {name}"), 45, COLOR_DIM_CYAN, 1);
                canvas.text_centered(&shown.to_string(), 100, score_color(shown), 5);
                canvas.text_centered("/ 100", 160, COLOR_DIM_GREEN, 1);
                canvas.rect(60, 205, 340 - 60, 220, COLOR_DIM_GREEN);
                canvas.fill_rect(
                    61,
                    206,
                    61 + (278 * shown.clamp(0, 100) / 100) as i32,
                    219,
                    score_color(shown),
                );
            } else if frame < 25 {
                canvas.fill(if frame.is_multiple_of(2) {
                    COLOR_RED
                } else {
                    COLOR_DARK_RED
                });
                canvas.text_centered("ACHIEVEMENT", 120, COLOR_YELLOW, 3);
            } else {
                canvas.rect(10, 10, 389, 289, COLOR_YELLOW);
                canvas.rect(15, 15, 384, 284, COLOR_DIM_GREEN);
                canvas.text_centered("ACHIEVEMENT UNLOCKED", 30, COLOR_YELLOW, 2);
                canvas.text_centered("LEGENDARY DEGEN", 85, COLOR_RED, 2);
                canvas.text_centered(&format!("SCORE: {score}"), 125, COLOR_YELLOW, 1);
                canvas.text_centered(&format!("CERTIFIED TO: {name}"), 160, COLOR_CYAN, 1);
                canvas.text_centered("FINANCIAL RUIN", 230, COLOR_GREEN, 1);
            }
        },
    )
}

#[must_use]
pub fn create_don_coin_flip_gif(name: &str, balance_lost: i64) -> GifAsset {
    let mut durations = vec![60; 15];
    durations.extend((0..15).map(|frame| 100 + frame * 30));
    durations.extend((0..20).map(|frame| {
        if frame == 19 {
            60_000
        } else {
            let nominal = 80 + frame * 60 / 19;
            (nominal / 10) * 10
        }
    }));
    render_neon_animation(
        "don_coin_flip",
        &durations,
        scene_seed(&(name, balance_lost)),
        |frame, canvas| {
            canvas.text_centered("DOUBLE OR NOTHING", 15, COLOR_YELLOW, 1);
            canvas.text_left(&format!("CLIENT: {name}"), 20, 40, COLOR_DIM_GREEN, 1);
            canvas.text_left(
                &format!("AT RISK: {balance_lost} JC"),
                20,
                58,
                COLOR_YELLOW,
                1,
            );
            if frame < 15 {
                let width = ((frame as i32 * 8) % 120 - 60).abs().max(10);
                canvas.rect(200 - width / 2, 105, 200 + width / 2, 155, COLOR_YELLOW);
                canvas.text_centered(
                    if frame.is_multiple_of(2) {
                        "DOUBLE"
                    } else {
                        "NOTHING"
                    },
                    118,
                    if frame.is_multiple_of(2) {
                        COLOR_GREEN
                    } else {
                        COLOR_RED
                    },
                    2,
                );
            } else if frame < 30 {
                canvas.text_centered("NOTHING", 118, COLOR_RED, 2);
                canvas.text_centered(
                    if frame < 27 {
                        "CALCULATING..."
                    } else {
                        "RESULT:"
                    },
                    170,
                    COLOR_YELLOW,
                    1,
                );
            } else {
                let progress = (frame - 30) as i64 * 1_000 / 19;
                let current = balance_lost.saturating_mul(1_000 - progress) / 1_000;
                canvas.text_centered("RESULT: NOTHING", 45, COLOR_RED, 1);
                canvas.text_centered(&current.to_string(), 115, COLOR_RED, 4);
                canvas.text_centered("JC", 170, COLOR_DIM_GREEN, 1);
                if frame == 49 {
                    canvas.text_centered("BALANCE: 0 JC", 245, COLOR_RED, 1);
                }
            }
        },
    )
}

#[must_use]
pub fn create_market_crash_gif(
    total_pool: i64,
    outcome: &str,
    winners: u32,
    losers: u32,
) -> GifAsset {
    let mut durations = vec![100; 15];
    durations.extend([80; 15]);
    durations.extend([200; 14]);
    durations.push(60_000);
    render_neon_animation(
        "market_crash",
        &durations,
        scene_seed(&(total_pool, outcome, winners, losers)),
        |frame, canvas| {
            if frame < 30 {
                let crash = frame >= 15;
                let progress = (frame % 15) as i32;
                let peak_x = 280;
                let end_x = if crash { 370 } else { 40 + progress * 20 };
                let end_y = if crash {
                    210 - progress * 8
                } else {
                    190 - progress * 6
                };
                canvas.text_centered(
                    "PREDICTION MARKET",
                    10,
                    if crash { COLOR_RED } else { COLOR_GREEN },
                    1,
                );
                canvas.text_centered(&format!("POOL: {total_pool} JC"), 32, COLOR_DIM_GREEN, 1);
                canvas.line(40, 220, 370, 220, COLOR_DIM_GREEN);
                canvas.line(40, 80, 40, 220, COLOR_DIM_GREEN);
                canvas.line(
                    40,
                    220,
                    peak_x,
                    100,
                    if crash { COLOR_RED } else { COLOR_GREEN },
                );
                canvas.line(
                    peak_x,
                    100,
                    end_x,
                    end_y,
                    if crash { COLOR_RED } else { COLOR_GREEN },
                );
                canvas.text_left(
                    if crash {
                        "STATUS: SETTLING"
                    } else {
                        "STATUS: ACTIVE"
                    },
                    20,
                    235,
                    COLOR_YELLOW,
                    1,
                );
                if crash && frame.is_multiple_of(2) {
                    canvas.text_centered("MARKET CRASH", 130, COLOR_RED, 2);
                }
            } else {
                canvas.text_centered("MARKET SETTLED", 20, COLOR_YELLOW, 2);
                canvas.text_centered(
                    &format!("OUTCOME: {}", outcome.to_ascii_uppercase()),
                    75,
                    COLOR_YELLOW,
                    1,
                );
                canvas.text_centered(&format!("TOTAL POOL: {total_pool} JC"), 105, COLOR_RED, 1);
                canvas.text_left(&format!("WINNERS: {winners}"), 60, 145, COLOR_GREEN, 1);
                canvas.text_left(&format!("LOSERS: {losers}"), 60, 170, COLOR_RED, 1);
                canvas.text_centered("WEALTH REDISTRIBUTED", 220, COLOR_GREEN, 1);
            }
        },
    )
}

#[must_use]
pub fn create_bomb_pot_gif(pool: i64, contributors: i64) -> GifAsset {
    let mut durations = vec![100; 15];
    durations.extend([60; 10]);
    durations.extend([200; 14]);
    durations.push(60_000);
    render_neon_animation(
        "bomb_pot",
        &durations,
        scene_seed(&(pool, contributors)),
        |frame, canvas| {
            if frame < 15 {
                let countdown = 15 - frame;
                canvas.text_centered("BOMB POT", 20, COLOR_RED, 2);
                canvas.text_centered("MANDATORY CONTRIBUTION", 50, COLOR_YELLOW, 1);
                canvas.text_centered(
                    &countdown.to_string(),
                    105,
                    if countdown > 5 {
                        COLOR_GREEN
                    } else {
                        COLOR_RED
                    },
                    5,
                );
                canvas.text_centered(
                    &format!("POOL: {} JC", pool * frame as i64 / 14),
                    175,
                    COLOR_DIM_GREEN,
                    1,
                );
                canvas.text_centered(
                    &format!("CONTRIBUTORS: {contributors}"),
                    200,
                    COLOR_DIM_GREEN,
                    1,
                );
            } else if frame < 25 {
                canvas.fill(if frame.is_multiple_of(2) {
                    COLOR_RED
                } else {
                    COLOR_YELLOW
                });
                canvas.text_centered("DETONATED", 120, COLOR_RED, 2);
                if frame > 18 {
                    canvas.text_centered(&format!("POOL: {pool} JC"), 170, COLOR_YELLOW, 2);
                }
            } else {
                canvas.text_centered("BOMB POT COMPLETE", 25, COLOR_RED, 2);
                canvas.text_centered(&format!("POOL: {pool} JC"), 110, COLOR_YELLOW, 2);
                canvas.text_centered(
                    &format!("CONTRIBUTORS: {contributors}"),
                    155,
                    COLOR_DIM_GREEN,
                    1,
                );
                canvas.text_centered("CONSENT: NOT REQUIRED", 215, COLOR_RED, 1);
                canvas.text_centered("ESCAPE: IMPOSSIBLE", 240, COLOR_RED, 1);
            }
        },
    )
}

/// Native counterpart of Python's `create_streak_record_gif`. The three
/// authored phases are kept explicit (count-up, record flash, certificate)
/// so the media contract remains seekable instead of collapsing to a static
/// fallback when the runtime has no font/image provider.
#[must_use]
pub fn create_streak_record_gif(name: &str, streak: i64) -> GifAsset {
    let mut durations = vec![60; 20];
    durations.extend([80; 10]);
    durations.extend([200; 14]);
    durations.push(60_000);
    render_neon_animation(
        "streak_record",
        &durations,
        scene_seed(&(name, streak)),
        |frame, canvas| {
            if frame < 20 {
                let shown = streak.saturating_mul(frame as i64) / 19;
                canvas.text_centered("WIN STREAK", 20, COLOR_GREEN, 2);
                canvas.text_centered(&format!("SUBJECT: {name}"), 50, COLOR_DIM_GREEN, 1);
                canvas.text_centered(&shown.to_string(), 95, COLOR_GREEN, 5);
                canvas.rect(60, 205, 340, 220, COLOR_DIM_GREEN);
                canvas.fill_rect(61, 206, 61 + (278 * frame as i32 / 19), 219, COLOR_GREEN);
            } else if frame < 30 {
                let flash = (150 - (frame - 20) * 15).clamp(0, 255);
                canvas.fill(if flash > 75 {
                    COLOR_DARK_RED
                } else {
                    COLOR_BLACK
                });
                canvas.text_centered("PERSONAL RECORD", 25, COLOR_YELLOW, 2);
                canvas.text_centered(&streak.to_string(), 95, COLOR_GREEN, 5);
                canvas.text_centered("CONSECUTIVE WINS", 175, COLOR_GREEN, 1);
                for sparkle in 0..(5 + (frame - 20).min(7)) {
                    let x = 35 + ((sparkle * 53 + frame * 17) % 330) as i32;
                    let y = 35 + ((sparkle * 29 + frame * 11) % 190) as i32;
                    canvas.pixel(
                        x,
                        y,
                        if sparkle.is_multiple_of(2) {
                            COLOR_CYAN
                        } else {
                            COLOR_YELLOW
                        },
                    );
                }
            } else {
                canvas.rect(10, 10, 389, 289, COLOR_GREEN);
                canvas.text_centered("ANOMALY DETECTED", 30, COLOR_GREEN, 2);
                canvas.text_centered(&format!("WIN X{streak}"), 115, COLOR_GREEN, 2);
                canvas.text_centered("STATUS: UNPRECEDENTED", 155, COLOR_YELLOW, 1);
                canvas.text_centered(&format!("SUBJECT: {name}"), 195, COLOR_DIM_CYAN, 1);
                canvas.text_centered("THE ALGORITHM ADJUSTS", 245, COLOR_DIM_GREEN, 1);
            }
        },
    )
}

/// Render the bounded text receipt used by the games-milestone hook.
#[must_use]
pub fn render_games_milestone(name: &str, total_games: i64) -> String {
    let tier = match total_games {
        10 => "BRONZE",
        50 => "SILVER",
        100 => "GOLD",
        200 => "DIAMOND",
        _ => "LEGENDARY",
    };
    terminal_lines(&[
        "GAMES MILESTONE".into(),
        "====================================".into(),
        format!("Subject: {name}"),
        format!("Games completed: {total_games}"),
        format!("Classification: {tier}"),
        "====================================".into(),
        "Milestone achieved.".into(),
        "Status: NOTED".into(),
        "You cannot leave.".into(),
        "None of them ever leave.".into(),
    ])
}

/// Render the bounded text receipt used by the personal win-streak hook.
#[must_use]
pub fn render_win_streak_record(name: &str, streak: i64) -> String {
    terminal_lines(&[
        "ANOMALY DETECTED".into(),
        "====================================".into(),
        format!("Subject: {name}"),
        format!("Pattern: WIN x{streak}"),
        "Status: UNPRECEDENTED".into(),
        "====================================".into(),
        "PERSONAL_RECORD_BROKEN".into(),
        format!("New streak: {streak}"),
        "Classification: HOT".into(),
        "The algorithm adjusts.".into(),
        "The system takes notice.".into(),
    ])
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PaletteFrame {
    pub colors: Vec<[u8; 3]>,
    pub palette: Vec<[u8; 3]>,
    pub indexed: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QuantizationStats {
    pub adaptive_searches: usize,
    pub palette_remaps: usize,
}

/// Select one animation-wide palette, then remap every full frame into it.
#[must_use]
pub fn quantize_frames_with_shared_palette(frames: &mut [PaletteFrame]) -> QuantizationStats {
    if frames.is_empty() {
        return QuantizationStats::default();
    }
    let mut palette = Vec::new();
    for color in frames.iter().flat_map(|frame| frame.colors.iter().copied()) {
        if !palette.contains(&color) && palette.len() < 256 {
            palette.push(color);
        }
    }
    for frame in &mut *frames {
        frame.palette.clone_from(&palette);
        frame.indexed = true;
    }
    QuantizationStats {
        adaptive_searches: 1,
        palette_remaps: frames.len(),
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EventKey {
    pub discord_id: u64,
    pub guild_id: u64,
    pub event_type: String,
}

impl EventKey {
    #[must_use]
    pub fn new(discord_id: u64, guild_id: Option<u64>, event_type: impl Into<String>) -> Self {
        Self {
            discord_id,
            guild_id: guild_id.unwrap_or(0),
            event_type: event_type.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreError(pub String);

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for StoreError {}

/// SQLite-backed implementations must preserve the Python repository's
/// `INSERT OR IGNORE` and write-error propagation semantics.
pub trait NeonEventPort: Send + Sync {
    fn load_one_time_events(&self) -> Result<Vec<EventKey>, StoreError>;
    fn check_one_time_event(&self, key: &EventKey) -> Result<bool, StoreError>;
    fn persist_one_time_event(&self, key: EventKey, layer: u8) -> Result<(), StoreError>;
}

#[derive(Debug, Default)]
struct MemoryEventState {
    events: HashMap<EventKey, u8>,
    fail_writes: bool,
}

#[derive(Clone, Debug, Default)]
pub struct MemoryNeonEventPort {
    state: Arc<Mutex<MemoryEventState>>,
}

impl MemoryNeonEventPort {
    pub fn fail_writes(&self, fail: bool) {
        self.state.lock().expect("memory event lock").fail_writes = fail;
    }
}

impl NeonEventPort for MemoryNeonEventPort {
    fn load_one_time_events(&self) -> Result<Vec<EventKey>, StoreError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| StoreError("event store lock poisoned".into()))?
            .events
            .keys()
            .cloned()
            .collect())
    }

    fn check_one_time_event(&self, key: &EventKey) -> Result<bool, StoreError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| StoreError("event store lock poisoned".into()))?
            .events
            .contains_key(key))
    }

    fn persist_one_time_event(&self, key: EventKey, layer: u8) -> Result<(), StoreError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| StoreError("event store lock poisoned".into()))?;
        if state.fail_writes {
            return Err(StoreError("database is locked".into()));
        }
        state.events.entry(key).or_insert(layer);
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NeonResult {
    pub layer: u8,
    pub text_block: Option<String>,
    pub gif_file: Option<GifAsset>,
    pub footer_text: Option<String>,
}

impl NeonResult {
    #[must_use]
    pub fn text(layer: u8, text_block: impl Into<String>) -> Self {
        Self {
            layer,
            text_block: Some(text_block.into()),
            gif_file: None,
            footer_text: None,
        }
    }

    fn with_gif(mut self, gif: GifAsset) -> Self {
        self.gif_file = Some(gif);
        self
    }
}

pub struct NeonDegenService {
    enabled: bool,
    cooldown_seconds: u64,
    max_debt: i64,
    bigwin_floor: f64,
    bigwin_full_payout: i64,
    bigwin_min_payout: i64,
    now_seconds: u64,
    cooldowns: HashMap<(u64, u64), u64>,
    one_time_seen: HashSet<EventKey>,
    event_port: Option<Arc<dyn NeonEventPort>>,
    pub rolls: RollSource,
}

impl fmt::Debug for NeonDegenService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NeonDegenService")
            .field("enabled", &self.enabled)
            .field("cooldown_seconds", &self.cooldown_seconds)
            .field("max_debt", &self.max_debt)
            .field("now_seconds", &self.now_seconds)
            .field("cooldowns", &self.cooldowns)
            .field("one_time_seen", &self.one_time_seen)
            .field("rolls", &self.rolls)
            .finish_non_exhaustive()
    }
}

impl NeonDegenService {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            enabled: true,
            cooldown_seconds: DEFAULT_COOLDOWN_SECONDS,
            max_debt: 500,
            bigwin_floor: 0.05,
            bigwin_full_payout: 5_000,
            bigwin_min_payout: 500,
            now_seconds: 10_000,
            cooldowns: HashMap::new(),
            one_time_seen: HashSet::new(),
            event_port: None,
            rolls: RollSource::seeded(seed),
        }
    }

    #[must_use]
    pub fn with_event_port(seed: u64, event_port: Arc<dyn NeonEventPort>) -> Self {
        let mut service = Self::new(seed);
        service.one_time_seen = event_port
            .load_one_time_events()
            .unwrap_or_default()
            .into_iter()
            .collect();
        service.event_port = Some(event_port);
        service
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Refresh the production clock used by the process-local Neon cooldown.
    /// Deterministic tests may keep using the constructor's fixed default.
    pub fn set_now_seconds(&mut self, now_seconds: u64) {
        self.now_seconds = now_seconds;
    }

    /// Apply the deployment's configured Neon cooldown.
    pub fn set_cooldown_seconds(&mut self, cooldown_seconds: u64) {
        self.cooldown_seconds = cooldown_seconds;
    }

    /// Apply the same debt-floor value used by the betting/economy service.
    /// Keeping this in the Neon policy avoids silently diverging when a
    /// deployment changes `MAX_DEBT` from its historical 500 JC default.
    pub fn set_max_debt(&mut self, max_debt: i64) {
        self.max_debt = max_debt.max(0);
    }

    /// Apply the deployment's Python-compatible scaled big-win policy.
    /// Invalid configuration is normalized to the same safe defaults used by
    /// the standalone Neon service.
    pub fn set_bigwin_policy(&mut self, floor: f64, full_payout: i64, min_payout: i64) {
        self.bigwin_floor = if floor.is_finite() {
            floor.clamp(0.0, 0.95)
        } else {
            0.05
        };
        self.bigwin_full_payout = full_payout.max(1);
        self.bigwin_min_payout = min_payout.max(0);
    }

    pub fn queue_rolls(&mut self, samples: impl IntoIterator<Item = f64>) {
        self.rolls.queue(samples);
    }

    fn normalized_guild(guild_id: Option<u64>) -> u64 {
        guild_id.unwrap_or(0)
    }

    fn check_cooldown(&self, discord_id: u64, guild_id: Option<u64>) -> bool {
        let key = (discord_id, Self::normalized_guild(guild_id));
        self.cooldowns
            .get(&key)
            .is_none_or(|last| self.now_seconds.saturating_sub(*last) >= self.cooldown_seconds)
    }

    fn set_cooldown(&mut self, discord_id: u64, guild_id: Option<u64>) {
        self.cooldowns.insert(
            (discord_id, Self::normalized_guild(guild_id)),
            self.now_seconds,
        );
    }

    fn unseen(&mut self, key: &EventKey) -> bool {
        if self.one_time_seen.contains(key) {
            return false;
        }
        let persisted = self
            .event_port
            .as_ref()
            .and_then(|port| port.check_one_time_event(key).ok())
            .unwrap_or(false);
        if persisted {
            self.one_time_seen.insert(key.clone());
            false
        } else {
            true
        }
    }

    fn mark_one_time(&mut self, key: EventKey, layer: u8) -> Result<(), StoreError> {
        if let Some(port) = &self.event_port {
            port.persist_one_time_event(key.clone(), layer)?;
        }
        self.one_time_seen.insert(key);
        Ok(())
    }

    #[must_use]
    pub fn check_one_time(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        event_type: &str,
    ) -> bool {
        self.unseen(&EventKey::new(discord_id, guild_id, event_type))
    }

    #[must_use]
    pub fn on_balance_check(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        balance: i64,
    ) -> Option<NeonResult> {
        if !self.enabled || !self.check_cooldown(discord_id, guild_id) || !self.rolls.roll(0.30) {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        Some(NeonResult::text(
            1,
            render_balance_check(&client_name(discord_id), balance),
        ))
    }

    /// Python-compatible process-local cooldown-hit flavor hook.
    #[must_use]
    pub fn on_cooldown_hit(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        cooldown_type: &str,
    ) -> Option<NeonResult> {
        if !self.enabled || !self.check_cooldown(discord_id, guild_id) || !self.rolls.roll(0.40) {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        Some(NeonResult::text(1, render_cooldown_hit(cooldown_type)))
    }

    /// Python-compatible `/gamba` result hook.  The betting runtime owns the
    /// settled amount and attachment delivery; this service only decides if a
    /// short terminal receipt should fire and applies the shared Neon
    /// cooldown.  Positive wins use the same chance gate as the wheel's
    /// ordinary Layer 1 easter egg, while bankrupt losses use the dedicated
    /// Layer 2 copy.
    #[must_use]
    pub fn on_wheel_result(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        result_value: i64,
        new_balance: i64,
    ) -> Option<NeonResult> {
        self.on_wheel_result_with_prior_balance(
            discord_id,
            guild_id,
            result_value,
            None,
            new_balance,
        )
    }

    /// Variant used by durable wheel settlement paths that know the balance
    /// immediately before the result.  Python's Freefall branch requires the
    /// player to have started with at least 100 JC; keeping that input at the
    /// call boundary prevents a negative result from being mistaken for a
    /// high-balance collapse after a restart or concurrent debit.
    #[must_use]
    pub fn on_wheel_result_with_prior_balance(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        result_value: i64,
        prior_balance: Option<i64>,
        new_balance: i64,
    ) -> Option<NeonResult> {
        if !self.enabled {
            return None;
        }
        if result_value > 0 {
            return self.on_big_win(
                discord_id,
                guild_id,
                BigWinSource::Gamba,
                result_value,
                BigWinFlavor::BigWin,
            );
        }
        if result_value < 0 && self.rolls.roll(0.30) {
            self.set_cooldown(discord_id, guild_id);
            return Some(NeonResult::text(
                2,
                render_wheel_bankrupt(&client_name(discord_id), result_value),
            ));
        }
        let freefall_eligible = prior_balance.is_some_and(|balance| balance >= 100)
            || (prior_balance.is_none() && new_balance <= 0);
        if result_value < 0 && freefall_eligible && new_balance <= 0 && self.rolls.roll(0.25) {
            self.set_cooldown(discord_id, guild_id);
            return Some(
                NeonResult {
                    layer: 3,
                    text_block: None,
                    gif_file: None,
                    footer_text: None,
                }
                .with_gif(create_freefall_gif(
                    &client_name(discord_id),
                    prior_balance.unwrap_or(100),
                    new_balance,
                )),
            );
        }
        None
    }

    /// Python-compatible big-win route shared by `/gamba`, prediction, and
    /// match settlement adapters.  The betting provider uses the Gamba
    /// source; keeping the policy here ensures the payout threshold, scaled
    /// chance, cooldown, and native attachment are one implementation.
    #[must_use]
    pub fn on_big_win(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        source: BigWinSource,
        payout: i64,
        flavor: BigWinFlavor,
    ) -> Option<NeonResult> {
        if !self.enabled
            || payout < self.bigwin_min_payout
            || !self.check_cooldown(discord_id, guild_id)
        {
            return None;
        }
        let fraction = (payout as f64 / self.bigwin_full_payout as f64).clamp(0.0, 1.0);
        let chance = self.bigwin_floor + (0.95 - self.bigwin_floor) * fraction;
        if !self.rolls.roll(chance) {
            return None;
        }
        let gif = render_big_win(
            &format!("Client-{}", discord_id % 10_000),
            payout,
            source,
            flavor,
        )
        .ok()?;
        self.set_cooldown(discord_id, guild_id);
        Some(
            NeonResult::text(
                3,
                ansi_block(&format!(
                    "[LEDGER] +{payout} JC settled. The house keeps receipts on winners too."
                )),
            )
            .with_gif(gif),
        )
    }

    /// Python-compatible post-settlement debt/zero-balance hook.  This hook
    /// intentionally does not consult the ordinary Neon cooldown: Match
    /// chooses at most one settlement candidate, and Python still evaluates
    /// the high-impact event even when a prior low-layer event consumed the
    /// player's cooldown.
    #[must_use]
    pub fn on_bet_settled(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        won: bool,
        new_balance: i64,
    ) -> Option<NeonResult> {
        if !self.enabled {
            return None;
        }
        if new_balance <= -self.max_debt && self.rolls.roll(0.90) {
            self.set_cooldown(discord_id, guild_id);
            return Some(NeonResult::text(
                2,
                render_system_breach(&client_name(discord_id)),
            ));
        }
        if new_balance == 0 && !won && self.rolls.roll(0.70) {
            self.set_cooldown(discord_id, guild_id);
            return Some(NeonResult::text(
                2,
                render_balance_zero(&client_name(discord_id)),
            ));
        }
        None
    }

    /// Python-compatible leveraged-loss hook.  A successful layer-2 debt
    /// collector consumes the shared cooldown, and the optional layer-3 GIF
    /// roll follows the Python ordering after the text event is selected.
    #[must_use]
    pub fn on_leverage_loss(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        amount: i64,
        leverage: i64,
        new_balance: i64,
    ) -> Option<NeonResult> {
        if !self.enabled || leverage < 5 || new_balance >= 0 || !self.rolls.roll(0.80) {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        let debt = new_balance.saturating_abs();
        let text = render_debt_collector(&client_name(discord_id), debt);
        if leverage >= 5 && new_balance <= -self.max_debt && self.rolls.roll(0.30) {
            return Some(
                NeonResult::text(3, text)
                    .with_gif(create_debt_collector_gif(&client_name(discord_id), debt)),
            );
        }
        let _ = amount;
        Some(NeonResult::text(2, text))
    }

    /// Python-compatible Lightning Bolt server-wide tax hook: one 20% Neon
    /// chance, with the Layer 2 assessment for totals of 500 JC or more.
    #[must_use]
    pub fn on_lightning_bolt(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        total_taxed: i64,
        players_hit: usize,
    ) -> Option<NeonResult> {
        if !self.enabled || !self.check_cooldown(discord_id, guild_id) || !self.rolls.roll(0.20) {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        Some(NeonResult::text(
            if total_taxed >= 500 { 2 } else { 1 },
            render_lightning_bolt(total_taxed, players_hit, total_taxed >= 500),
        ))
    }

    #[must_use]
    pub fn on_bankruptcy(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        debt_cleared: i64,
        filing_number: u32,
    ) -> Option<NeonResult> {
        if !self.enabled {
            return None;
        }
        let text = render_bankruptcy_filing(&client_name(discord_id), debt_cleared, filing_number);
        self.set_cooldown(discord_id, guild_id);
        let result = if filing_number == 1 {
            NeonResult::text(3, text).with_gif(create_void_welcome_gif(&client_name(discord_id)))
        } else if filing_number >= 3 {
            NeonResult::text(3, text).with_gif(GifAsset::terminal_crash())
        } else {
            NeonResult::text(2, text)
        };
        Some(result)
    }

    #[must_use]
    pub fn on_bet_placed(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        amount: i64,
        leverage: u8,
        team: &str,
    ) -> Option<NeonResult> {
        if !self.enabled || !self.check_cooldown(discord_id, guild_id) {
            return None;
        }
        let chance = if leverage == 1 { 0.10 } else { 0.20 };
        if !self.rolls.roll(chance) {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        Some(NeonResult::text(
            1,
            render_bet_placed(amount, team, leverage),
        ))
    }

    /// Python-compatible all-in candidate: a base stake consuming at least
    /// 90% of the pre-wager balance has a 35% Layer 2 chance.
    #[must_use]
    pub fn on_all_in_bet(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        amount: i64,
        balance_before: i64,
    ) -> Option<NeonResult> {
        if !self.enabled || balance_before <= 0 {
            return None;
        }
        let percentage = amount as f64 / balance_before as f64 * 100.0;
        if percentage < 90.0 || !self.check_cooldown(discord_id, guild_id) || !self.rolls.roll(0.35)
        {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        Some(NeonResult::text(
            2,
            render_all_in_bet(&client_name(discord_id), amount, percentage),
        ))
    }

    /// Python-compatible final-window candidate.  The command layer only
    /// queues positive values, while the service keeps the original
    /// `seconds_remaining <= 60` policy for direct callers.
    #[must_use]
    pub fn on_last_second_bet(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        seconds_remaining: i64,
    ) -> Option<NeonResult> {
        if !self.enabled
            || seconds_remaining > 60
            || !self.check_cooldown(discord_id, guild_id)
            || !self.rolls.roll(0.05)
        {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        Some(NeonResult::text(
            2,
            render_last_second_bet(&client_name(discord_id), seconds_remaining),
        ))
    }

    /// Python-compatible first-ever leveraged wager candidate.  The marker
    /// is persisted before the result is returned, exactly like the Python
    /// helper's successful-event callback and its existing one-time store.
    #[must_use]
    pub fn on_first_leverage_bet(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        leverage: u8,
    ) -> Option<NeonResult> {
        if !self.enabled || leverage < 2 {
            return None;
        }
        let key = EventKey::new(discord_id, guild_id, "first_leverage");
        if !self.unseen(&key) || !self.rolls.roll(0.80) || self.mark_one_time(key, 1).is_err() {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        Some(NeonResult::text(
            1,
            render_first_leverage(&client_name(discord_id), leverage),
        ))
    }

    /// Python-compatible one-time 100-bets milestone candidate.
    #[must_use]
    pub fn on_100_bets_milestone(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        total_bets: i64,
    ) -> Option<NeonResult> {
        if !self.enabled || total_bets != 100 {
            return None;
        }
        let key = EventKey::new(discord_id, guild_id, "100_bets");
        if !self.unseen(&key) || !self.rolls.roll(0.50) || self.mark_one_time(key, 2).is_err() {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        Some(NeonResult::text(
            2,
            render_bets_milestone(&client_name(discord_id), total_bets),
        ))
    }

    /// Python-compatible `/economy tip` hook: try the Layer 2 surveillance
    /// report first, then the Layer 1 transfer notice.  Both branches share
    /// the normal per-user/guild Neon cooldown and consume one cooldown only
    /// when a message actually fires.
    #[must_use]
    pub fn on_tip(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        sender: &str,
        recipient: &str,
        amount: i64,
        fee: i64,
    ) -> Option<NeonResult> {
        if !self.enabled || !self.check_cooldown(discord_id, guild_id) {
            return None;
        }
        if self.rolls.roll(0.05) {
            self.set_cooldown(discord_id, guild_id);
            return Some(NeonResult::text(
                2,
                render_tip_surveillance(sender, recipient, amount, fee),
            ));
        }
        if self.rolls.roll(0.20) {
            self.set_cooldown(discord_id, guild_id);
            return Some(NeonResult::text(1, render_tip(sender, recipient, amount)));
        }
        None
    }

    #[must_use]
    pub fn on_loan(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        amount: i64,
        total_owed: i64,
        is_negative: bool,
    ) -> Option<NeonResult> {
        if !self.enabled {
            return None;
        }
        let text = if is_negative {
            if !self.rolls.roll(0.80) {
                return None;
            }
            NeonResult::text(
                2,
                render_negative_loan(&client_name(discord_id), amount, -total_owed.abs()),
            )
        } else {
            if !self.check_cooldown(discord_id, guild_id) || !self.rolls.roll(0.50) {
                return None;
            }
            NeonResult::text(1, render_loan_taken(amount, total_owed))
        };
        self.set_cooldown(discord_id, guild_id);
        Some(text)
    }

    #[must_use]
    pub fn on_match_recorded(&mut self) -> Option<NeonResult> {
        if !self.enabled || !self.rolls.roll(0.35) {
            return None;
        }
        Some(NeonResult::text(2, render_match_recorded()))
    }

    /// Python-compatible post-match debrief route. Match supplies only
    /// committed, typed facts; this method performs the same base/notable/
    /// extreme chance gate and renders the bounded JOPA-T fallback without
    /// querying or mutating Match-owned state.
    #[must_use]
    pub fn on_post_match_debrief(&mut self, context: &JopatPostMatchContext) -> Option<NeonResult> {
        if !self.enabled {
            return None;
        }
        let extreme = context.payout >= 750
            || context.loss >= 500
            || context.leverage >= 5
            || context.bankruptcy_count >= 2
            || context
                .degen_score
                .as_ref()
                .and_then(|value| value.as_str().parse::<f64>().ok())
                .is_some_and(|score| score >= 95.0)
            || context
                .rating_change
                .as_ref()
                .and_then(|value| value.as_str().parse::<f64>().ok())
                .is_some_and(|change| change.abs() >= 40.0)
            || context
                .expected_win_prob
                .as_ref()
                .and_then(|value| value.as_str().parse::<f64>().ok())
                .is_some_and(|probability| probability <= 0.30)
            || context.streak.abs() >= 7;
        let notable = context.payout >= 500
            || context.loss >= 200
            || context.leverage >= 3
            || context.bankruptcy_count > 0
            || context
                .degen_score
                .as_ref()
                .and_then(|value| value.as_str().parse::<f64>().ok())
                .is_some_and(|score| score >= 80.0)
            || context
                .rating_change
                .as_ref()
                .and_then(|value| value.as_str().parse::<f64>().ok())
                .is_some_and(|change| change.abs() >= 20.0)
            || context
                .expected_win_prob
                .as_ref()
                .and_then(|value| value.as_str().parse::<f64>().ok())
                .is_some_and(|probability| probability < 0.45)
            || context.streak.abs() >= 4;
        let chance = if extreme {
            0.75
        } else if notable {
            0.55
        } else {
            0.35
        };
        if !self.rolls.roll(chance) {
            return None;
        }
        let seed = u64::try_from(context.payout.max(0))
            .unwrap_or_default()
            .wrapping_mul(31)
            .wrapping_add(u64::try_from(context.loss.max(0)).unwrap_or_default())
            .wrapping_add(u64::try_from(context.streak.saturating_abs()).unwrap_or_default());
        let mut choice = SeededJopatChoice::new(seed);
        let protocol = choose_protocol(context, &mut choice);
        let fallback = render_fallback(protocol, context, &mut choice);
        Some(NeonResult::text(2, fallback))
    }

    #[must_use]
    pub fn on_degen_milestone(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        degen_score: u8,
    ) -> Option<NeonResult> {
        if !self.enabled || degen_score < 90 {
            return None;
        }
        let key = EventKey::new(discord_id, guild_id, "degen_90");
        if !self.unseen(&key) || self.mark_one_time(key, 3).is_err() {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        Some(
            NeonResult::text(
                3,
                terminal_lines(&[
                    "ACHIEVEMENT UNLOCKED".into(),
                    format!("Degen Score: {degen_score}"),
                    "Classification: LEGENDARY".into(),
                ]),
            )
            .with_gif(create_degen_certificate_gif(
                &client_name(discord_id),
                i64::from(degen_score),
            )),
        )
    }

    /// Python-compatible games milestone hook. The match recorder supplies
    /// committed totals; this service owns only the exact milestone set,
    /// cooldown/chance gate, and the 100+ animated certificate promotion.
    #[must_use]
    pub fn on_games_milestone(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        total_games: i64,
    ) -> Option<NeonResult> {
        if !self.enabled
            || !matches!(total_games, 10 | 50 | 100 | 200 | 500)
            || !self.check_cooldown(discord_id, guild_id)
            || !self.rolls.roll(0.10)
        {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        let name = client_name(discord_id);
        let text = render_games_milestone(&name, total_games);
        if total_games >= 100 {
            Some(
                NeonResult::text(3, text)
                    .with_gif(create_degen_certificate_gif(&name, total_games)),
            )
        } else {
            Some(NeonResult::text(2, text))
        }
    }

    /// Python-compatible personal win-streak record hook. Match recording
    /// performs the durable previous-best update and passes both values here;
    /// a GIF is selected only for an 8+ streak, matching the Python layer
    /// split and its 20% chance gate.
    #[must_use]
    pub fn on_win_streak_record(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        current_streak: i64,
        previous_best: i64,
    ) -> Option<NeonResult> {
        if !self.enabled
            || current_streak < 5
            || current_streak <= previous_best
            || !self.check_cooldown(discord_id, guild_id)
            || !self.rolls.roll(0.20)
        {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        let name = client_name(discord_id);
        let text = render_win_streak_record(&name, current_streak);
        if current_streak >= 8 {
            Some(
                NeonResult::text(3, text).with_gif(create_streak_record_gif(&name, current_streak)),
            )
        } else {
            Some(NeonResult::text(2, text))
        }
    }

    #[must_use]
    pub fn on_double_or_nothing(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        won: bool,
        balance_at_risk: i64,
        final_balance: i64,
    ) -> Option<NeonResult> {
        if !self.enabled {
            return None;
        }
        let name = client_name(discord_id);
        let result = if won {
            NeonResult::text(1, render_don_win(&name, final_balance))
        } else if balance_at_risk > 100 && self.rolls.roll(0.18) {
            NeonResult::text(3, render_don_loss_box(&name, balance_at_risk))
                .with_gif(create_don_coin_flip_gif(&name, balance_at_risk))
        } else if balance_at_risk > 50 && self.rolls.roll(0.80) {
            NeonResult::text(2, render_don_loss_box(&name, balance_at_risk))
        } else {
            NeonResult::text(1, render_don_lose(&name, balance_at_risk))
        };
        self.set_cooldown(discord_id, guild_id);
        Some(result)
    }

    #[must_use]
    pub fn on_draft_coinflip(
        &mut self,
        guild_id: Option<u64>,
        winner_id: u64,
        loser_id: u64,
    ) -> Option<NeonResult> {
        if !self.enabled || !self.rolls.roll(0.40) {
            return None;
        }
        let _ = guild_id;
        Some(NeonResult::text(
            1,
            render_coinflip(&client_name(winner_id), &client_name(loser_id)),
        ))
    }

    /// Python-compatible captain-symmetry hook: a 20% Layer 1 event for
    /// captains within fifty rating points.
    #[must_use]
    pub fn on_captain_symmetry(
        &mut self,
        guild_id: Option<u64>,
        captain1_id: u64,
        captain2_id: u64,
        rating_diff: i64,
    ) -> Option<NeonResult> {
        let _ = guild_id;
        let diff = rating_diff.saturating_abs();
        if !self.enabled || diff > 50 || !self.rolls.roll(0.20) {
            return None;
        }
        Some(NeonResult::text(
            1,
            render_captain_symmetry(&client_name(captain1_id), &client_name(captain2_id), diff),
        ))
    }

    /// Python-compatible bomb-pot hook: a 25% Layer 3 event with an
    /// upload-safe GIF attachment and a text fallback represented by the
    /// same typed result.
    #[must_use]
    pub fn on_bomb_pot(
        &mut self,
        guild_id: Option<u64>,
        pool_amount: i64,
        contributor_count: i64,
    ) -> Option<NeonResult> {
        let _ = guild_id;
        if !self.enabled || !self.rolls.roll(0.25) {
            return None;
        }
        Some(
            NeonResult::text(3, render_bomb_pot(pool_amount, contributor_count))
                .with_gif(create_bomb_pot_gif(pool_amount, contributor_count)),
        )
    }

    #[must_use]
    pub fn on_registration(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        player_name: &str,
    ) -> Option<NeonResult> {
        if !self.enabled {
            return None;
        }
        let key = EventKey::new(discord_id, guild_id, "registration");
        if !self.unseen(&key) || !self.rolls.roll(0.50) {
            return None;
        }
        if self.mark_one_time(key, 1).is_err() {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        Some(NeonResult::text(1, render_registration(player_name)))
    }

    /// Python `on_lobby_join`: explicit `/join` only, 3% and per-user/guild
    /// cooldown guarded.
    #[must_use]
    pub fn on_lobby_join(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        player_name: &str,
        queue_position: usize,
    ) -> Option<NeonResult> {
        if !self.enabled || !self.check_cooldown(discord_id, guild_id) || !self.rolls.roll(0.03) {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        Some(NeonResult::text(
            1,
            render_lobby_join(player_name, queue_position),
        ))
    }

    /// Python jopacoin lobby reaction hook: 5% and per-user/guild cooldown
    /// guarded. The runtime adapter owns the 60-second Discord cleanup.
    #[must_use]
    pub fn on_gamba_spectator(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        display_name: &str,
    ) -> Option<NeonResult> {
        if !self.enabled || !self.check_cooldown(discord_id, guild_id) || !self.rolls.roll(0.05) {
            return None;
        }
        self.set_cooldown(discord_id, guild_id);
        Some(NeonResult::text(1, render_gamba_spectator(display_name)))
    }

    #[must_use]
    pub fn on_prediction_resolved(
        &mut self,
        question: &str,
        outcome: &str,
        total_pool: i64,
        winner_count: u32,
        loser_count: u32,
    ) -> Option<NeonResult> {
        if !self.enabled {
            return None;
        }
        if total_pool >= 500 && self.rolls.roll(0.30) {
            return Some(
                NeonResult::text(
                    3,
                    render_prediction_market_crash(
                        question,
                        total_pool,
                        outcome,
                        winner_count,
                        loser_count,
                    ),
                )
                .with_gif(create_market_crash_gif(
                    total_pool,
                    outcome,
                    winner_count,
                    loser_count,
                )),
            );
        }
        if total_pool >= 200 && self.rolls.roll(0.70) {
            return Some(NeonResult::text(
                2,
                render_prediction_market_crash(
                    question,
                    total_pool,
                    outcome,
                    winner_count,
                    loser_count,
                ),
            ));
        }
        self.rolls
            .roll(0.30)
            .then(|| NeonResult::text(1, render_prediction_resolved(question, outcome, total_pool)))
    }

    #[must_use]
    pub fn on_soft_avoid(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        cost: i64,
        games: u32,
    ) -> Option<NeonResult> {
        if !self.enabled || !self.check_cooldown(discord_id, guild_id) {
            return None;
        }
        let result = if self.rolls.roll(0.10) {
            NeonResult::text(2, render_soft_avoid_surveillance(cost, games))
        } else if self.rolls.roll(0.25) {
            NeonResult::text(1, render_soft_avoid(cost, games))
        } else {
            return None;
        };
        self.set_cooldown(discord_id, guild_id);
        Some(result)
    }

    #[must_use]
    pub fn on_match_enriched(
        &mut self,
        winners: &[MatchTelemetry],
        mvp_chance: f64,
    ) -> Vec<NeonResult> {
        if !self.enabled {
            return Vec::new();
        }
        let enriched = winners
            .iter()
            .filter(|winner| winner.has_enriched_telemetry())
            .collect::<Vec<_>>();
        if enriched.is_empty() || !self.rolls.roll(mvp_chance) {
            return Vec::new();
        }
        let Some(target) = enriched.into_iter().max_by(|left, right| {
            left.selection_key()
                .partial_cmp(&right.selection_key())
                .unwrap_or(std::cmp::Ordering::Equal)
        }) else {
            return Vec::new();
        };
        let mention = target
            .discord_id
            .map_or_else(String::new, |discord_id| format!("<@{discord_id}>\n"));
        vec![NeonResult::text(
            2,
            format!(
                "{mention}{}",
                terminal_lines(&[
                    "JOPA-T MVP TELEMETRY".into(),
                    format!(
                        "K/D/A: {}/{}/{}",
                        target.kills, target.deaths, target.assists
                    ),
                    format!("GPM: {}", target.gpm),
                ])
            ),
        )]
    }
}

fn client_name(discord_id: u64) -> String {
    format!("Client-{}", discord_id % 10_000)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MatchTelemetry {
    pub discord_id: Option<u64>,
    pub hero_id: Option<u16>,
    pub kills: i32,
    pub deaths: i32,
    pub assists: i32,
    pub gpm: i32,
    pub xpm: Option<i32>,
    pub fantasy_points: Option<f64>,
}

/// Durable rating facts supplied by Match after settlement.  The Neon layer
/// does not query Match's repositories itself; this value object keeps the
/// Python `_get_notable_winner` selection policy reusable by either runtime.
#[derive(Clone, Debug, PartialEq)]
pub struct RatingHistorySnapshot {
    pub discord_id: i64,
    pub rating_before: Option<f64>,
    pub rating_after: Option<f64>,
    pub openskill_mu_before: Option<f64>,
    pub openskill_mu_after: Option<f64>,
    pub expected_team_win_prob: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotableWinnerDetails {
    pub rating_change: Option<f64>,
    pub rating_change_system: Option<&'static str>,
    pub glicko_rating_change: Option<f64>,
    pub openskill_rating_change: Option<i64>,
    pub expected_win_prob: Option<f64>,
    pub is_underdog: bool,
    pub is_big_gainer: bool,
}

fn rating_changes(
    entry: &RatingHistorySnapshot,
    openskill: &CamaOpenSkillSystem,
) -> (Option<f64>, Option<i64>) {
    let glicko = entry
        .rating_before
        .zip(entry.rating_after)
        .map(|(before, after)| after - before);
    let openskill =
        entry
            .openskill_mu_before
            .zip(entry.openskill_mu_after)
            .map(|(before, after)| {
                i64::from(openskill.mu_to_display(after) - openskill.mu_to_display(before))
            });
    (glicko, openskill)
}

fn notable_details(
    entry: &RatingHistorySnapshot,
    openskill: &CamaOpenSkillSystem,
    is_underdog: bool,
    is_big_gainer: bool,
) -> NotableWinnerDetails {
    let (glicko, openskill_change) = rating_changes(entry, openskill);
    let (rating_change, rating_change_system) = match (glicko, openskill_change) {
        (None, None) => (None, None),
        (Some(change), None) => (Some(change), Some("glicko")),
        (None, Some(change)) => (Some(change as f64), Some("openskill")),
        (Some(glicko), Some(openskill)) if glicko >= openskill as f64 => {
            (Some(glicko), Some("glicko"))
        }
        (Some(_), Some(openskill)) => (Some(openskill as f64), Some("openskill")),
    };
    NotableWinnerDetails {
        rating_change,
        rating_change_system,
        glicko_rating_change: glicko,
        openskill_rating_change: openskill_change,
        expected_win_prob: entry.expected_team_win_prob,
        is_underdog,
        is_big_gainer,
    }
}

/// Pick the Python-compatible notable winner: lowest-probability winner below
/// 45% first, otherwise the winner with the largest available display-point
/// gain across Glicko and OpenSkill.  Ties retain input order like Python's
/// stable `min`/`max`; an empty history returns `None` for Match's fallback.
#[must_use]
pub fn choose_notable_winner(
    history: &[RatingHistorySnapshot],
    winning_ids: &[i64],
    openskill: &CamaOpenSkillSystem,
) -> Option<(i64, NotableWinnerDetails)> {
    let winners = history
        .iter()
        .filter(|entry| winning_ids.contains(&entry.discord_id))
        .collect::<Vec<_>>();
    // Keep Python's stable ``min``/``max`` behavior explicitly.  Rust's
    // iterator ``max_by`` is allowed to select the last equal item, while
    // the Python implementation keeps the first winner on a tie.
    let underdog = winners.iter().copied().fold(None, |best, candidate| {
        let Some(best) = best else {
            return Some(candidate);
        };
        let candidate_probability = candidate.expected_team_win_prob.unwrap_or(0.5);
        let best_probability = best.expected_team_win_prob.unwrap_or(0.5);
        (candidate_probability < best_probability)
            .then_some(candidate)
            .or(Some(best))
    });
    if let Some(underdog) = underdog {
        let probability = underdog.expected_team_win_prob.unwrap_or(0.5);
        if probability < 0.45 {
            return Some((
                underdog.discord_id,
                notable_details(underdog, openskill, true, false),
            ));
        }
    }
    let best = winners.into_iter().fold(None, |best, candidate| {
        let candidate_changes = rating_changes(candidate, openskill);
        let candidate_score = candidate_changes
            .0
            .into_iter()
            .chain(candidate_changes.1.map(|change| change as f64))
            .fold(f64::NEG_INFINITY, f64::max);
        let Some((best, best_score)) = best else {
            return Some((candidate, candidate_score));
        };
        (candidate_score > best_score)
            .then_some((candidate, candidate_score))
            .or(Some((best, best_score)))
    });
    best.map(|(winner, _)| {
        (
            winner.discord_id,
            notable_details(winner, openskill, false, true),
        )
    })
}

impl MatchTelemetry {
    fn has_enriched_telemetry(&self) -> bool {
        self.hero_id.is_some()
            || self.kills != 0
            || self.deaths != 0
            || self.assists != 0
            || self.gpm != 0
            || self.xpm.is_some()
            || self.fantasy_points.is_some()
    }

    fn selection_key(&self) -> (OrderedFloat, i32, i32) {
        (
            OrderedFloat(self.fantasy_points.unwrap_or(0.0)),
            self.kills + self.assists,
            self.gpm,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OrderedFloat(f64);

impl Eq for OrderedFloat {}

impl PartialOrd for OrderedFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContextValue {
    Text(String),
    Integer(i64),
    Number(f64),
    Bool(bool),
}

impl From<&str> for ContextValue {
    fn from(value: &str) -> Self {
        Self::Text(value.into())
    }
}

impl From<i64> for ContextValue {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

pub type PlayerContext = BTreeMap<String, ContextValue>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiRequest {
    pub prompt: String,
    pub system_prompt: String,
    pub feature: &'static str,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GenerationOptions {
    pub anonymous: bool,
    pub validate_facts: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedText {
    pub text: String,
    pub request: Option<AiRequest>,
}

const SYSTEM_PROMPT: &str = "You are JOPA-T/v3.7. Use verified facts only.";
const STRICT_FACT_KEYS: &[&str] = &[
    "winner_name",
    "loser_name",
    "hero",
    "kills",
    "deaths",
    "assists",
    "gpm",
    "xpm",
    "rating_change",
    "expected_win_prob",
    "payout",
    "loss",
    "leverage",
    "bankruptcy_count",
    "degen_score",
    "streak",
];

#[must_use]
pub fn build_player_context(player: Option<&PlayerSnapshot>) -> PlayerContext {
    let mut context = PlayerContext::new();
    if let Some(player) = player {
        context.insert("name".into(), ContextValue::Text(player.name.clone()));
        context.insert("balance".into(), ContextValue::Integer(player.balance));
        if let Some(lowest) = player.lowest_balance {
            context.insert("lowest_balance".into(), ContextValue::Integer(lowest));
        }
    }
    context
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerSnapshot {
    pub discord_id: u64,
    pub name: String,
    pub balance: i64,
    pub lowest_balance: Option<i64>,
}

#[must_use]
pub fn generate_text(
    ai_enabled: bool,
    event_description: &str,
    player_context: &PlayerContext,
    fallback: &str,
    model_output: Option<&str>,
    options: GenerationOptions,
) -> GeneratedText {
    if !ai_enabled {
        return GeneratedText {
            text: fallback.into(),
            request: None,
        };
    }
    if options.validate_facts
        && player_context
            .keys()
            .any(|key| !STRICT_FACT_KEYS.contains(&key.as_str()))
    {
        return GeneratedText {
            text: fallback.into(),
            request: None,
        };
    }

    let effective_context = if options.anonymous {
        PlayerContext::new()
    } else {
        player_context.clone()
    };
    let prompt_context = if options.validate_facts {
        redact_strict_context(&effective_context)
    } else {
        effective_context.clone()
    };
    let context_json = serialize_context(&prompt_context);
    let (prompt, system_prompt, feature) = if options.validate_facts {
        let facts = structured_fact_lines(&effective_context);
        (
            format!(
                "Event: {event_description}\nPlayer context:\n{context_json}\n\nReturn exactly one JSON object. fact must be one of {:?}.",
                facts.keys().collect::<Vec<_>>()
            ),
            format!(
                "{SYSTEM_PROMPT}\n\nCRITICAL: Select approved components only. Return exactly the requested JSON object."
            ),
            "neon.post_match",
        )
    } else if options.anonymous {
        (
            format!(
                "Event: {event_description}\nPlayer context:\n{context_json}\n\nExample output:\n{fallback}\n\nDo NOT reference any player-specific stats."
            ),
            format!(
                "{SYSTEM_PROMPT}\n\nCRITICAL: This is an ANONYMOUS event. DO NOT include any player names, usernames, balances, statistics, or identifying information."
            ),
            "neon.event",
        )
    } else {
        (
            format!(
                "Event: {event_description}\nPlayer context:\n{context_json}\n\nExample output:\n{fallback}\n\nUse only the supplied context fields."
            ),
            SYSTEM_PROMPT.into(),
            "neon.event",
        )
    };
    let request = AiRequest {
        prompt,
        system_prompt,
        feature,
    };

    let Some(model_output) = model_output else {
        return GeneratedText {
            text: fallback.into(),
            request: Some(request),
        };
    };
    let text = if options.validate_facts {
        render_structured_selection(model_output, &effective_context)
            .map_or_else(|| fallback.into(), |selected| ansi_block(&selected))
    } else {
        let stripped = strip_markdown_fence(model_output);
        let sanitized = sanitize_llm_terminal_text(&stripped, 1800);
        if sanitized.is_empty() {
            fallback.into()
        } else {
            ansi_block(&sanitized)
        }
    };
    GeneratedText {
        text,
        request: Some(request),
    }
}

fn redact_strict_context(context: &PlayerContext) -> PlayerContext {
    let mut redacted = context.clone();
    if redacted.contains_key("winner_name") {
        redacted.insert(
            "winner_name".into(),
            ContextValue::Text("[verified winner client]".into()),
        );
    }
    if redacted.contains_key("loser_name") {
        redacted.insert(
            "loser_name".into(),
            ContextValue::Text("[verified loser client]".into()),
        );
    }
    redacted
}

fn serialize_context(context: &PlayerContext) -> String {
    let entries = context
        .iter()
        .map(|(key, value)| {
            let value = match value {
                ContextValue::Text(value) => format!("\"{}\"", json_escape(value)),
                ContextValue::Integer(value) => value.to_string(),
                ContextValue::Number(value) if value.is_finite() => value.to_string(),
                ContextValue::Number(_) => "null".into(),
                ContextValue::Bool(value) => value.to_string(),
            };
            format!("\"{}\": {value}", json_escape(key))
        })
        .collect::<Vec<_>>();
    format!("{{{}}}", entries.join(", "))
}

fn json_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn strip_markdown_fence(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return trimmed.into();
    }
    let without_open = trimmed
        .find('\n')
        .map_or("", |newline| &trimmed[newline + 1..]);
    without_open
        .strip_suffix("```")
        .unwrap_or(without_open)
        .trim()
        .into()
}

#[must_use]
pub fn sanitize_llm_terminal_text(text: &str, max_chars: usize) -> String {
    let mut cleaned = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\u{1b}' => match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    skip_control_sequence(&mut chars);
                }
                Some(']') => {
                    chars.next();
                    skip_osc_sequence(&mut chars);
                }
                _ => {}
            },
            '\u{009b}' => skip_control_sequence(&mut chars),
            '`' => cleaned.push('\''),
            '\n' | '\t' => cleaned.push(character),
            character if forbidden_format_character(character) || character.is_control() => {}
            character => cleaned.push(character),
        }
    }
    let compact = cleaned
        .lines()
        .filter_map(|line| {
            let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
            (!line.is_empty()).then_some(line)
        })
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    let protected = protect_mentions(&compact);
    truncate_chars(&protected, max_chars)
}

fn skip_control_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for character in chars.by_ref() {
        if ('@'..='~').contains(&character) {
            break;
        }
    }
}

fn skip_osc_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(character) = chars.next() {
        if character == '\u{7}' {
            break;
        }
        if character == '\u{1b}' && chars.peek() == Some(&'\\') {
            chars.next();
            break;
        }
    }
}

fn forbidden_format_character(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
    )
}

fn protect_mentions(text: &str) -> String {
    let mut protected = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let remainder = &text[index..];
        let lower = remainder.to_ascii_lowercase();
        if lower.starts_with("@everyone") {
            protected.push_str("@\u{200b}");
            protected.push_str(&remainder[1..9]);
            index += 9;
        } else if lower.starts_with("@here") {
            protected.push_str("@\u{200b}");
            protected.push_str(&remainder[1..5]);
            index += 5;
        } else if let Some(after) = remainder.strip_prefix("<@") {
            let mention_len = after
                .char_indices()
                .find_map(|(offset, character)| (character == '>').then_some(offset + 1));
            if let Some(mention_len) = mention_len {
                let body = &after[..mention_len];
                let numeric = body
                    .strip_suffix('>')
                    .unwrap_or(body)
                    .trim_start_matches(['!', '&']);
                if !numeric.is_empty()
                    && numeric.chars().all(|character| character.is_ascii_digit())
                {
                    protected.push_str("<@\u{200b}");
                    protected.push_str(body);
                    index += 2 + mention_len;
                    continue;
                }
            }
            protected.push_str("<@");
            index += 2;
        } else {
            let character = remainder.chars().next().expect("non-empty remainder");
            protected.push(character);
            index += character.len_utf8();
        }
    }
    protected
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.into();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let mut output = text.chars().take(max_chars - 3).collect::<String>();
    while output.ends_with(char::is_whitespace) {
        output.pop();
    }
    output.push_str("...");
    output
}

fn structured_fact_lines(context: &PlayerContext) -> BTreeMap<&'static str, String> {
    let mut lines = BTreeMap::new();
    if let Some(payout) = positive_integer(context, "payout") {
        lines.insert("payout", format!("SETTLEMENT: {payout} JC paid."));
    }
    if let Some(loss) = positive_integer(context, "loss") {
        lines.insert("loss", format!("SETTLEMENT: {loss} JC lost."));
    }
    if let Some(kills) = integer(context, "kills")
        && let (Some(deaths), Some(assists)) =
            (integer(context, "deaths"), integer(context, "assists"))
    {
        lines.insert(
            "kda",
            format!("COMBAT LEDGER: {kills}/{deaths}/{assists} K/D/A."),
        );
    }
    if context.contains_key("winner_name") && !context.contains_key("loser_name") {
        lines.insert("outcome", "OUTCOME: Verified client victory.".into());
    } else if context.contains_key("loser_name") && !context.contains_key("winner_name") {
        lines.insert("outcome", "OUTCOME: Verified client loss.".into());
    }
    if lines.is_empty() {
        lines.insert(
            "telemetry",
            "TELEMETRY: Verified post-match sample archived.".into(),
        );
    }
    lines
}

fn integer(context: &PlayerContext, key: &str) -> Option<i64> {
    match context.get(key) {
        Some(ContextValue::Integer(value)) => Some(*value),
        _ => None,
    }
}

fn positive_integer(context: &PlayerContext, key: &str) -> Option<i64> {
    integer(context, key).filter(|value| *value > 0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StructuredSelection<'a> {
    lead: usize,
    fact: &'a str,
    closer: usize,
}

fn parse_structured_selection(raw: &str) -> Option<StructuredSelection<'_>> {
    let object = raw.trim().strip_prefix('{')?.strip_suffix('}')?;
    let mut lead = None;
    let mut fact = None;
    let mut closer = None;
    let mut count = 0;
    for field in object.split(',') {
        count += 1;
        let (key, value) = field.split_once(':')?;
        let key = key.trim().strip_prefix('"')?.strip_suffix('"')?;
        let value = value.trim();
        match key {
            "lead" if lead.is_none() => lead = value.parse().ok(),
            "fact" if fact.is_none() => fact = Some(value.strip_prefix('"')?.strip_suffix('"')?),
            "closer" if closer.is_none() => closer = value.parse().ok(),
            _ => return None,
        }
    }
    (count == 3).then_some(StructuredSelection {
        lead: lead?,
        fact: fact?,
        closer: closer?,
    })
}

fn render_structured_selection(raw: &str, context: &PlayerContext) -> Option<String> {
    const HYPE_LEADS: &[&str] = &[
        "STATUS: ODDS DEFEATED",
        "RISK ENGINE: CLIENT ASCENDING",
        "MARKET ALERT: ANCIENT UNDER NEW MANAGEMENT",
        "CASTER PROTOCOL: VOLUME APPROVED",
    ];
    const HYPE_CLOSERS: &[&str] = &[
        "The house spreadsheet has entered the trees.",
        "Risk controls have been asked to queue support.",
        "The confidence index has achieved escape velocity.",
        "JOPA-T upgrades this result from variance to cinema.",
    ];
    const ROAST_LEADS: &[&str] = &[
        "STATUS: BUYBACK DENIED",
        "RISK ENGINE: POSITION LIQUIDATED",
        "COMPLIANCE: MMR COLLATERAL IMPAIRED",
        "REPLAY AUDIT: EXPLANATIONS DEPRECIATED",
    ];
    const ROAST_CLOSERS: &[&str] = &[
        "The replay has been classified as unsecured debt.",
        "Buyback remains unavailable in both client and risk model.",
        "The lane requested a responsible adult. Compliance sent a parlay.",
        "JOPA-T marked the performance to market. The market objected.",
    ];
    const NEUTRAL_LEADS: &[&str] = &[
        "STATUS: TELEMETRY INGESTED",
        "MATCH LEDGER: SAMPLE ACCEPTED",
        "REPLAY PARSER: CONCLUSIONS PENDING",
        "RISK ENGINE: OUTCOME ARCHIVED",
    ];
    const NEUTRAL_CLOSERS: &[&str] = &[
        "The system has kept the receipt.",
        "Further confidence remains chance-gated.",
        "No Ancient was consulted during this filing.",
        "The queue may now resume pretending this was planned.",
    ];

    let selection = parse_structured_selection(raw)?;
    let winner = context.contains_key("winner_name")
        || positive_integer(context, "payout").is_some()
        || integer(context, "rating_change").is_some_and(|value| value > 0);
    let loser = context.contains_key("loser_name")
        || positive_integer(context, "loss").is_some()
        || integer(context, "rating_change").is_some_and(|value| value < 0);
    let (leads, closers) = if winner && !loser {
        (HYPE_LEADS, HYPE_CLOSERS)
    } else if loser && !winner {
        (ROAST_LEADS, ROAST_CLOSERS)
    } else {
        (NEUTRAL_LEADS, NEUTRAL_CLOSERS)
    };
    let fact_lines = structured_fact_lines(context);
    Some(format!(
        "[12:00:00.000] {}\n{}\n{}",
        leads.get(selection.lead)?,
        fact_lines.get(selection.fact)?,
        closers.get(selection.closer)?
    ))
}

#[cfg(test)]
#[path = "neon_degen/tests.rs"]
mod tests;
