//! Prewritten lines posted when no model text is available.
//!
//! Slots are written as `{slot}` and filled with [`fill`]. A line is only
//! picked when every slot it uses can be filled.

use crate::ai_services::FlavorEvent;

/// Lobby plea. Slots: `{lobby}`, `{in}`, `{needed}`.
pub const PLEA_LINES: &[&str] = &[
    "We are not asking. We are begging. The first game of the day is right there.",
    "Zero games played today. Roshan is getting bored and starting a book club.",
    "One click stands between this server and its first throw of the day.",
    "The lobby is {needed} short and the silence is getting loud.",
    "Somewhere a courier is walking alone because you have not queued.",
    "Your MMR is not going to ruin itself. Get in here.",
    "{in} people believed in this lobby. Do not make them look stupid.",
    "The ancients are unguarded and nobody is even here to throw.",
    "You opened Discord for a reason. This is the reason.",
    "No game today yet. The fountain is cold. The shopkeeper is weeping.",
    "It is not a real day until someone feeds first blood. Help us make it real.",
    "Lurkers. We can see you. The lobby can see you. Join.",
    "This lobby needs {needed} more and we are prepared to grovel.",
    "Think of the bettors. They have jopacoin and nothing to lose it on.",
    "Skipping the first game is how losing streaks start. Probably. Join anyway.",
    "The creeps spawn in spirit only until this lobby fills.",
    "Be the hero this lobby needs, or at least the position five it deserves.",
    "Every minute this lobby stays open, a support buys their own wards and cries.",
    "You have time for one game. You always have time for one game.",
    "We promise nobody will blame you. That is a lie, but join anyway.",
    "{needed} more and we go. Do not let the dream die in the queue.",
    "First game of the day energy is unmatched. Nobody is tilted yet. Yet.",
    "The lobby is almost full and entirely desperate.",
    "Imagine {in} grown adults staring at a lobby. Now fix it.",
    "Your chair is right there. Your mouse is right there. The lobby is right here.",
    "Nobody has thrown a single game today. This is your chance to be first.",
    "The betting window is gathering dust. The gamblers are restless.",
    "Whoever joins next gets our eternal gratitude and absolutely nothing else.",
    "Do it for the content. Do it for the clips. Do it because we asked nicely.",
    "This is a cry for help with a lobby attached.",
    "The day's first game is one or two brave souls away.",
    "Roshan respawned three times waiting for this lobby.",
    "Please. The mid lane is empty and it is making everyone uncomfortable.",
    "Your daily dose of regret is ready, just {needed} short of serving.",
    "We have carries. We have supports. We do not have enough of either.",
    "You are going to queue eventually. Save us all the suspense.",
    "Fresh day, fresh lobby, same old throws. Join up.",
    "A lobby this close to full is basically a moral obligation.",
    "The shuffler is warmed up and has nothing to shuffle.",
    "Nobody has been flamed today. The balance of nature is off.",
    "Pause whatever you are doing. It is less important than this lobby.",
    "{in} in, {needed} to go, zero dignity left in this message.",
    "The first game sets the tone. Right now the tone is crickets.",
    "Come lose with friends. Or win. Mostly it is about the friends.",
    "We would send a courier to fetch you if we could.",
    "This lobby has been almost ready for what feels like a full Techies game.",
    "Your team needs you. They do not know who you are yet, but they need you.",
    "The observer wards are down and so is our morale.",
    "One more game cannot hurt. This is the first one, so technically zero games can hurt.",
    "Abandon your responsibilities, not this lobby.",
    "Every great comeback starts with someone joining the lobby.",
    "We are {needed} short of a game and long on excuses. Bring yours.",
    "Dinner can wait. The laundry can wait. The lobby cannot.",
    "No inhouse yet today. Somewhere a rat is not being ratted.",
    "The lobby is lonely and so is your match history.",
    "Tip the scales. Fill the slot. Be someone's reason to blame the draft.",
    "Clock is ticking. People are leaving their keyboards to make snacks. Hurry.",
    "It only takes one of you to stop the suffering. Maybe two.",
    "We checked. You are online. Do not make this weird.",
    "A full lobby is a happy lobby. This lobby is not happy.",
    "The first game of the day is free of grudges. Enjoy it while it lasts.",
    "Get in before somebody suggests a different game.",
    "This plea was written by a bot. Even the bot wants you to queue.",
    "You could be farming right now. Instead the jungle sits untouched.",
    "{needed} more. That is all. We have never asked for anything else. Today.",
    "Stop scrolling. Start queuing.",
    "The team captains of fate are waiting on you specifically.",
    "If this lobby dies, it dies knowing you could have saved it.",
    "Help. We have {in} players and no game. This is how villain arcs begin.",
    "Buybacks are ready. Egos are ready. The lobby is not.",
    "You do not have to play well. You just have to show up.",
    "First blood has been waiting all day for a volunteer.",
    "Nobody is ahead in net worth yet. This is as fair as it gets.",
    "The longer you wait, the more someone practices their excuses.",
    "Somebody out there is one click from being today's main character.",
    "We ran the numbers. The numbers say join.",
    "Low effort request: sit in a lobby. High effort reward: bragging rights.",
    "Your rival is probably already in here. Just saying.",
    "The lobby is at the door with flowers. Do not leave it standing there.",
    "It has been zero games since the last game. Unacceptable.",
    "Queue now and you can still claim you were the reason it fired.",
    "The neutrals are stacked and nobody is here to take them.",
    "Give the people what they want. The people want {needed} more.",
    "This is the easiest good deed you will do all day.",
    "Smoke is ready. Gank is planned. Target acquired: your free evening.",
    "An empty slot is just a missed opportunity to be flamed by friends.",
    "We have enough players for a very sad {in}-person game. Fix it.",
    "Ready check dreams are made of this. Almost.",
    "The day is young and winless. Change that. Or lose trying.",
    "A hero you have not played in months is calling. Answer in this lobby.",
    "Every lobby that fires started as a lobby that begged.",
    "You were going to play later anyway. Later is now.",
    "Slots are limited. Dignity is not, clearly.",
    "The game cannot start without you. Literally. That is how numbers work.",
    "Think about how good it feels to hit a creep. Now go do it.",
    "Half the server is waiting on the other half. Pick a half.",
    "{lobby} has more hope than players. Even it out.",
    "Join and nobody mentions the last game you threw. For now.",
    "The ancient will not defend itself. Historically it does not anyway.",
    "Last plea, we swear: {needed} more and the day finally begins.",
];

/// Post-game bait blaming one loser. Slot: `{loser}`.
pub const BAIT_BLAME_LOSER: &[&str] = &[
    "The replay is in. It was {loser}. It was always {loser}.",
    "{loser} is typing a very long explanation. Nobody asked.",
    "Somebody check on {loser}. Actually, check their item build first.",
    "{loser} has entered the excuse phase. Next comes the rematch phase.",
    "Historians will study what {loser} did in that game. Not kindly.",
    "{loser} played their heart out. Unfortunately the heart was not enough.",
    "One more like that and {loser} gets their own highlight reel. Of deaths.",
    "{loser}, the enemy team would like to thank you for your service.",
    "Petition to let {loser} pick the next draft so there is someone to blame.",
    "{loser} says it was the team. The team says it was {loser}. Discuss.",
    "Bold strategy from {loser}. Shame about the result.",
    "{loser} owes four people an apology and one person a rematch.",
    "Nobody is blaming {loser}. Out loud.",
    "{loser} was definitely in that game. That is the nicest thing we can say.",
    "Scientists confirm {loser} could have bought a ward at any time.",
    "{loser} is one bad game from blaming lag. Stay tuned.",
    "Does {loser} want to run that back, or sit with it for a while?",
    "{loser} fought bravely. Mostly alone. Mostly under the enemy tower.",
    "Is {loser} going to take that? Really?",
    "The fountain misses {loser} already. They spent so much time together.",
];

/// Post-game bait crowning one winner. Slot: `{winner}`.
pub const BAIT_CROWN_WINNER: &[&str] = &[
    "{winner} carried that so hard their back needs a buyback.",
    "All hail {winner}, at least until the next game humbles them.",
    "{winner} is insufferable right now and has earned it.",
    "Somebody stop {winner} before they start believing they are good.",
    "{winner} made that look easy. Who is going to make it hard?",
    "{winner} is feeling themselves. Someone do something about it.",
    "That was the {winner} show and the rest of us were extras.",
    "{winner} just raised the bounty on their own head.",
    "{winner} will be talking about that one for a week unless someone ends it tonight.",
    "{winner} peaked. Prove us wrong.",
    "Is anyone going to challenge {winner}, or is this just their server now?",
    "{winner} played like the rent was due.",
    "Nobody tell {winner} it was the draft. Let them have this.",
    "{winner} did that with a straight face. Disgusting. Again.",
    "The crown fits {winner} suspiciously well. Someone knock it off.",
];

/// Post-game bait pitting a winner against a loser. Slots: `{winner}`, `{loser}`.
pub const BAIT_VERSUS: &[&str] = &[
    "{winner} lives in {loser}'s head rent free now.",
    "{loser}, are you going to let {winner} talk like that?",
    "{winner} over {loser}. The record books will not be gentle.",
    "{loser} wants a rematch with {winner}. They have not said it, but we know.",
    "{winner} and {loser} need to settle this properly. That was only round one.",
    "Sources say {loser} is already plotting revenge on {winner}.",
    "{winner} says {loser} was free. Their words. Probably.",
    "This {winner} versus {loser} thing is becoming a storyline.",
    "{loser} had {winner} right where they wanted them. Then the game started.",
    "Somewhere {winner} is smiling and {loser} can feel it.",
    "{winner} farmed creeps, heroes, and {loser}'s patience.",
    "{loser} can fix this. All it takes is beating {winner} once.",
    "{winner} did not even look at {loser}'s lane. That is the insult.",
    "Next game {loser} gets {winner} on their team and has to say thank you.",
    "{winner} took that personally. {loser} should too.",
];

/// Post-game bait about the biggest lost bet. Slots: `{bettor}`, `{amount}`.
pub const BAIT_BET_LOST: &[&str] = &[
    "{bettor} just donated {amount} jopacoin to the cause. Very generous.",
    "A moment of silence for {bettor}'s {amount} jopacoin.",
    "{bettor} bet {amount} on the wrong team and would like it back. No.",
    "{amount} jopacoin gone. {bettor} is one bad bet from a villain arc.",
    "{bettor} trusted the wrong five people with {amount} jopacoin.",
    "The house thanks {bettor} for the {amount}. Come again.",
    "{bettor} is down {amount} and looking for someone to win it back from.",
    "{bettor} read the odds, ignored them, and paid {amount} for the lesson.",
    "{amount} jopacoin says {bettor} bets the other side next time.",
    "{bettor} had a feeling. The feeling cost {amount}.",
    "Financial advice from {bettor} is now {amount} jopacoin less credible.",
    "{bettor} can win that {amount} back. Any game now.",
];

/// Post-game bait about an upset. Slots: `{side}`, `{chance}`.
pub const BAIT_UPSET: &[&str] = &[
    "{side} had a {chance}% chance and took it personally.",
    "The odds said {chance}%. {side} said watch this.",
    "A {chance}% shot and {side} still took the throne. Who let that happen?",
    "{side} at {chance}% was supposed to be a formality. It was a robbery.",
    "Somebody tell the shuffler {side} was not meant to win at {chance}%.",
    "Upset alert: {side} won a game the math gave {chance}%.",
    "{chance}%. That is what {side} was given. Look what they did with it.",
    "The favorites just lost to a {chance}% underdog. Explain yourselves.",
    "{side} beat {chance}% odds, which means someone on the other side has questions to answer.",
    "Every bettor who faded {side} at {chance}% is very quiet right now.",
    "{side} turned {chance}% into a win. The other team turned a lead into a lesson.",
    "That was a {chance}% miracle for {side} or a collapse for the other side. Vote now.",
];

/// Post-game bait about a win streak. Slots: `{player}`, `{streak}`.
pub const BAIT_STREAK: &[&str] = &[
    "{player} has won {streak} in a row. Who is ending it?",
    "{streak} straight for {player}. This is getting out of hand.",
    "{player} is on {streak} wins and nobody has done anything about it.",
    "Bounty on {player}: {streak} wins and counting.",
    "{player} at {streak} in a row. Somebody be the villain.",
    "{streak} wins running. {player} thinks this is normal now.",
    "Is {player} unbeatable, or has nobody tried hard enough? {streak} and counting.",
    "{player} just set a personal best of {streak} straight. Ruin it.",
    "A {streak} game heater for {player}. The shuffler is taking notes.",
    "{player} on {streak} in a row is everyone's problem now.",
];

/// Post-game bait that needs no player facts. Slot: `{side}`.
pub const BAIT_GENERIC: &[&str] = &[
    "GG. Winners, prove it was not a fluke. Losers, prove it was.",
    "{side} took that one. The other side is already typing excuses.",
    "Nobody is logging off on that. The blame is still warm.",
    "That game answered nothing.",
    "{side} wins. The losing side insists it was the draft.",
    "One game proves nothing. Two games proves slightly more.",
    "Was that skill or luck? There is only one way to find out.",
    "{side} is celebrating. Give it ten minutes.",
    "Somebody is leaving that game with a grudge. Good.",
    "The scoreboard is closed. The argument is wide open.",
    "{side} victory. Rematch demands are being accepted, reluctantly.",
    "That one is going to come up in conversation for days.",
    "Half of you are happy. The other half are planning something.",
    "A clean win for {side}, or a dirty loss for the other team. Depends who you ask.",
    "Losers queue is a myth. Probably.",
    "That is one in the books. The night is not over.",
];

/// Bankruptcy roast. Slots: `{name}`, `{count}`.
pub const BANKRUPTCY_ROAST_LINES: &[&str] = &[
    "Financial reset complete. Shame meter: still maxed.",
    "{name} has declared bankruptcy. The jopacoin never stood a chance.",
    "Bankruptcy number {count}. At this point it is a subscription.",
    "The debt is gone. The decisions that caused it remain.",
    "{name} hit the reset button like it owed them money. It did.",
    "Clean slate, dirty history.",
    "The bank would like to speak to {name}. The bank is crying.",
    "Filing number {count}. The clerk knows {name} by voice now.",
    "A fresh start for {name}, sponsored by everyone who paid their debts.",
    "Wiped clean. Like the lanes {name} was supposed to be defending.",
    "Bankruptcy: because paying it back was never really on the table.",
    "{name} turned debt into a lifestyle and then into paperwork.",
    "The jopacoin gods have forgiven {name}. The leaderboard has not.",
    "That is {count} bankruptcies. Some people collect stamps.",
    "Debt cleared. Dignity sold separately.",
    "{name} is financially reborn and no wiser.",
    "The penalty games will remember this even if {name} does not.",
    "Another glorious chapter in {name}'s ongoing fiscal tragedy.",
    "Nobody loses money like {name}. It is almost a talent.",
    "The ledger has been burned. The bettors remember.",
    "{name} went all in on being broke and it finally paid off.",
    "Bankruptcy filed. Somewhere a loan shark sheds a single tear.",
    "This is filing {count}. The form now comes pre-filled.",
    "{name} speedran poverty and is going for a new record.",
    "Zero balance, infinite confidence.",
    "Debts erased. The next ones are already forming.",
    "{name} would like everyone to know this was strategic.",
    "The house always wins. {name} always helps.",
    "A moment of silence for the jopacoin {name} set on fire.",
    "Reset complete. Expect the same result, faster.",
    "{name} declared bankruptcy with the calm of someone who has done it before.",
    "Paperwork approved. The judge did not even look up.",
    "Economists will study {name}. Carefully, from a distance.",
    "{name} is free of debt and full of bad ideas.",
    "Bankruptcy {count}: now with even less remorse.",
    "The balance is zero and somehow that is an improvement.",
    "{name} lost it all, filed for it all, and will bet it all again.",
    "Starting over is easy when you do it this often.",
    "Creditors hate this one weird trick. {name} has used it {count} times.",
    "The slate is clean. Give it a week.",
];

/// Loan roast. Slots: `{name}`, `{amount}`.
pub const LOAN_ROAST_LINES: &[&str] = &[
    "Your financial advisor just felt a disturbance in the force.",
    "{name} borrowed {amount} jopacoin. The repayment plan is vibes.",
    "A loan of {amount}. The bank calls it risk. We call it comedy.",
    "{name} sees a loan as free money. The fee disagrees.",
    "Borrowed today, bet tonight, broke by tomorrow.",
    "{amount} jopacoin on credit. What could possibly go right?",
    "The loan officer approved {name} without making eye contact.",
    "{name} is investing in themselves. Historically a poor investment.",
    "Interest is temporary. Regret compounds.",
    "{name} took out a loan the way other people take out the trash: often.",
    "That {amount} has a very short life expectancy.",
    "The bank believes in {name}. The bank is new here.",
    "Borrowing {amount} to feel something. Relatable. Unwise.",
    "{name} has a plan for that loan. The plan is a bet.",
    "Money in hand, sense nowhere to be found.",
    "{amount} jopacoin, fresh from someone else's pocket.",
    "{name} just signed for {amount} and did not read a single word.",
    "A loan is just a bet with extra steps and a fee.",
    "The fee alone should have been a warning. It was not.",
    "{name} is now rich in debt and poor in everything else.",
    "Somewhere a betting window just lit up in anticipation.",
    "This loan will be remembered fondly, right up until it is due.",
    "{name} asked for {amount} and the universe sighed.",
    "Liquidity acquired. Judgment still pending.",
    "The nonprofit fund thanks {name} for the fee.",
    "{name} is one loan closer to a very familiar form.",
    "Borrow responsibly. Or do whatever this is.",
    "{amount} borrowed. The countdown to an all-in begins.",
    "{name} financed their next mistake in advance.",
    "Banks hate being paid back late. {name} hates paying them back at all.",
    "The ink is wet and the jopacoin is already nervous.",
    "{name} now owes more than they did a minute ago. Progress.",
    "Credit extended. Expectations lowered.",
    "A bold loan from someone with {name}'s track record.",
    "That is {amount} reasons to win the next bet.",
];

/// Roast for a loan taken while already in debt. Slots: `{name}`, `{amount}`.
pub const NEGATIVE_LOAN_ROAST_LINES: &[&str] = &[
    "Borrowing while broke is financial performance art.",
    "{name} was already in debt and thought: more.",
    "A loan to pay for the loan. The circle of debt.",
    "{name} dug a hole, then borrowed {amount} for a bigger shovel.",
    "This is not a loan. This is a cry for help with a fee.",
    "Legendary degen move. The bank is taking notes for a documentary.",
    "{name} looked at a negative balance and saw opportunity.",
    "Debt on top of debt. {name} is building a tower.",
    "{amount} more on the pile. The pile says thank you.",
    "The bank asked if {name} was sure. {name} was not listening.",
    "Rock bottom had a basement and {name} just took out a mortgage on it.",
    "Most people stop digging. {name} finances the excavation.",
    "Owing money to borrow money is a kind of genius. The wrong kind.",
    "{name} now owes the bank, the fee, and an explanation.",
    "A negative balance and a positive attitude. Dangerous combination.",
    "{name} is leveraging debt against future debt.",
    "The loan shark is impressed, and a little concerned.",
    "This is how villain origin stories are funded.",
    "{name} borrowed {amount} while underwater. Bubbles were seen.",
    "The debt is not shrinking. {name} is just getting used to it.",
    "At this point the bank is just along for the ride.",
    "{name} treats the red number as a high score.",
    "Another {amount} against a balance that was already begging for mercy.",
    "Financially speaking, {name} is feeding.",
    "Deeper in debt, higher in spirit. Unclear how.",
];

/// A well-mixed value derived from `seed`, so nearby seeds pick unrelated lines.
#[must_use]
pub const fn mix(seed: u64) -> u64 {
    let mut value = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// Replace every `{key}` in `template` with its value.
#[must_use]
pub fn fill(template: &str, slots: &[(&str, &str)]) -> String {
    slots
        .iter()
        .fold(template.to_owned(), |line, (key, value)| {
            line.replace(&format!("{{{key}}}"), value)
        })
}

/// Pick a line from `pool` and fill it, skipping lines that need a slot not in `slots`.
#[must_use]
pub fn pick_line(pool: &[&str], seed: u64, slots: &[(&str, &str)]) -> Option<String> {
    if pool.is_empty() {
        return None;
    }
    let start = (mix(seed) % pool.len() as u64) as usize;
    (0..pool.len())
        .map(|offset| fill(pool[(start + offset) % pool.len()], slots))
        .find(|line| !line.contains('{'))
}

/// A prewritten roast for a bankruptcy or loan, or `None` for other events.
///
/// `name` must already be safe to post. Lines needing an absent name, a
/// non-positive count or a non-positive amount are skipped.
#[must_use]
pub fn roast_line(
    event: FlavorEvent,
    name: Option<&str>,
    count: i64,
    amount: i64,
    seed: u64,
) -> Option<String> {
    let pool = match event {
        FlavorEvent::BankruptcyDeclared => BANKRUPTCY_ROAST_LINES,
        FlavorEvent::LoanTaken => LOAN_ROAST_LINES,
        FlavorEvent::NegativeLoan => NEGATIVE_LOAN_ROAST_LINES,
        _ => return None,
    };
    let count_text = count.to_string();
    let amount_text = amount.to_string();
    let mut slots = Vec::new();
    if let Some(name) = name {
        slots.push(("name", name));
    }
    if count > 0 {
        slots.push(("count", count_text.as_str()));
    }
    if amount > 0 {
        slots.push(("amount", amount_text.as_str()));
    }
    pick_line(pool, seed, &slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const POOLS: &[(&str, &[&str], &[&str], usize)] = &[
        ("plea", PLEA_LINES, &["lobby", "in", "needed"], 100),
        ("blame", BAIT_BLAME_LOSER, &["loser"], 20),
        ("crown", BAIT_CROWN_WINNER, &["winner"], 15),
        ("versus", BAIT_VERSUS, &["winner", "loser"], 15),
        ("bet", BAIT_BET_LOST, &["bettor", "amount"], 12),
        ("upset", BAIT_UPSET, &["side", "chance"], 12),
        ("streak", BAIT_STREAK, &["player", "streak"], 10),
        ("generic", BAIT_GENERIC, &["side"], 16),
        ("bankruptcy", BANKRUPTCY_ROAST_LINES, &["name", "count"], 40),
        ("loan", LOAN_ROAST_LINES, &["name", "amount"], 35),
        (
            "negative loan",
            NEGATIVE_LOAN_ROAST_LINES,
            &["name", "amount"],
            25,
        ),
    ];

    #[test]
    fn pools_are_large_unique_and_only_use_their_own_slots() {
        let mut seen = BTreeSet::new();
        for (label, pool, slots, minimum) in POOLS {
            assert!(pool.len() >= *minimum, "{label} pool shrank");
            let longest = "W".repeat(32);
            let filled_slots = slots
                .iter()
                .map(|slot| (*slot, longest.as_str()))
                .collect::<Vec<_>>();
            for line in *pool {
                assert!(seen.insert(*line), "duplicate line: {line}");
                let filled = fill(line, &filled_slots);
                assert!(!filled.contains(['{', '}']), "unknown slot in {line}");
                assert!(filled.chars().count() <= 240, "{label} line too long");
                assert!(
                    !line.contains(['<', '@', '`', '/']),
                    "{label} line carries markup or a command: {line}"
                );
            }
        }
        let bait = POOLS[1..8].iter().map(|pool| pool.1.len()).sum::<usize>();
        let roasts = POOLS[8..].iter().map(|pool| pool.1.len()).sum::<usize>();
        assert!(bait >= 100 && roasts >= 100);
    }

    #[test]
    fn pick_line_skips_lines_whose_slots_are_missing() {
        for seed in 0..500 {
            let line = pick_line(PLEA_LINES, seed, &[("needed", "2")]).expect("slot-free line");
            assert!(!line.contains('{'));
        }
        assert_eq!(pick_line(BAIT_BLAME_LOSER, 3, &[]), None);
        assert_eq!(pick_line(&[], 3, &[]), None);
    }

    #[test]
    fn pick_line_rotates_with_the_seed() {
        let picked = (0..200)
            .filter_map(|seed| pick_line(BAIT_GENERIC, seed, &[("side", "Radiant")]))
            .collect::<BTreeSet<_>>();
        assert_eq!(picked.len(), BAIT_GENERIC.len());
    }

    #[test]
    fn roast_lines_cover_bankruptcy_and_loans_only() {
        let line = roast_line(FlavorEvent::BankruptcyDeclared, Some("Alice"), 3, 0, 7)
            .expect("bankruptcy roast");
        assert!(!line.contains('{'));
        for seed in 0..200 {
            let anonymous = roast_line(FlavorEvent::LoanTaken, None, 0, 0, seed).expect("loan");
            assert!(LOAN_ROAST_LINES.contains(&anonymous.as_str()));
        }
        assert!(roast_line(FlavorEvent::NegativeLoan, Some("Bob"), 0, 50, 1).is_some());
        assert_eq!(roast_line(FlavorEvent::BetWon, Some("Bob"), 1, 1, 1), None);
    }
}
