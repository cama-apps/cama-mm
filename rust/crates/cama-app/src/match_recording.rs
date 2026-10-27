//! Record shapes for the post-match easter eggs the runtime announces.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WinStreakRecord {
    pub discord_id: i64,
    pub current_streak: i64,
    pub previous_best: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RivalryRecord {
    pub player1_id: i64,
    pub player2_id: i64,
    pub games_against: i64,
    pub winrate_vs: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GamesMilestone {
    pub discord_id: i64,
    pub games_played: i64,
}
