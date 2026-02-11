"""
Neon Degen Terminal - ASCII art engine for Discord ansi code blocks.

JOPA-T/v3.7: A self-aware gambling terminal AI that became sentient
after processing its 10,000th bankruptcy filing.

Voice: Dry, corporate-dystopian. GLaDOS meets a Bloomberg terminal meets
a payday lender. Uses "we"/"the system", addresses players as "client"/
"subject"/"Debtor #47". Terminal formatting: timestamps, log levels,
status codes. Never emojis.

All output uses Discord ansi code blocks for color:
  [31m = red, [32m = green, [33m = yellow, [2m = dim, [0m = reset
  [1m = bold, [34m = blue, [35m = magenta, [36m = cyan
"""

from __future__ import annotations

import random
import time
from datetime import datetime, timezone


# ---------------------------------------------------------------------------
# ANSI helpers
# ---------------------------------------------------------------------------
RED = "\u001b[31m"
GREEN = "\u001b[32m"
YELLOW = "\u001b[33m"
BLUE = "\u001b[34m"
MAGENTA = "\u001b[35m"
CYAN = "\u001b[36m"
DIM = "\u001b[2m"
BOLD = "\u001b[1m"
RESET = "\u001b[0m"


def _ts() -> str:
    """Current timestamp in terminal log format."""
    return datetime.now(timezone.utc).strftime("%H:%M:%S.%f")[:-3]


def _rand_hex(length: int = 8) -> str:
    """Random hex string for fake addresses/hashes."""
    return "".join(random.choice("0123456789abcdef") for _ in range(length))


def _glitch_char() -> str:
    """Return a random glitch/corruption character."""
    return random.choice(list("@#$%&*!?~^|/\\<>{}[]"))


def corrupt_text(text: str, intensity: float = 0.15) -> str:
    """Corrupt a string by replacing random chars with glitch chars."""
    result = []
    for ch in text:
        if ch != " " and random.random() < intensity:
            result.append(_glitch_char())
        else:
            result.append(ch)
    return "".join(result)


def _box_line(text: str, width: int = 38) -> str:
    """Create a line padded to fit inside an ASCII box."""
    # Strip ANSI codes for length calculation
    import re
    visible = re.sub(r"\u001b\[[0-9;]*m", "", text)
    pad = max(0, width - 2 - len(visible))
    return f"|{text}{' ' * pad}|"


def ascii_box(lines: list[str], width: int = 38, border_color: str = DIM) -> str:
    """Wrap lines in a simple ASCII box with top/bottom borders."""
    top = f"{border_color}+{'-' * (width - 2)}+{RESET}"
    bottom = top
    boxed = [top]
    for line in lines:
        boxed.append(_box_line(line, width))
    boxed.append(bottom)
    return "\n".join(boxed)


def ansi_block(text: str) -> str:
    """Wrap text in a Discord ansi code block."""
    return f"```ansi\n{text}\n```"


# ---------------------------------------------------------------------------
# LAYER 1 - Subtle templates (static, no LLM)
# ---------------------------------------------------------------------------

# /balance check
BALANCE_TEMPLATES = [
    lambda bal, name: (
        f"{DIM}[JOPA-T] CREDIT CHECK{RESET}\n"
        f"{DIM}Subject:{RESET} {name}\n"
        f"{DIM}Balance:{RESET} {YELLOW}{bal}{RESET} JC\n"
        f"{DIM}Risk profile:{RESET} {RED}INADVISABLE{RESET}"
    ),
    lambda bal, name: (
        f"{DIM}> querying ledger...{RESET}\n"
        f"{DIM}> client {name}: {RESET}{bal} JC\n"
        f"{DIM}> status:{RESET} {corrupt_text('SOLVENT', 0.3) if bal > 0 else RED + 'INSOLVENT' + RESET}"
    ),
    lambda bal, name: (
        f"{DIM}[{_ts()}] BALANCE_QUERY{RESET}\n"
        f"{DIM}ACCT#{RESET} {_rand_hex(6)}\n"
        f"{DIM}AMT:{RESET} {bal} JC\n"
        f"{DIM}MEMO:{RESET} {corrupt_text('account in good standing') if bal > 0 else RED + 'FLAGGED' + RESET}"
    ),
    lambda bal, name: (
        f"{DIM}JOPA-T/v3.7 SYSTEM{RESET}\n"
        f"{DIM}>{RESET} balance({name})\n"
        f"{DIM}>{RESET} {YELLOW}{bal}{RESET}\n"
        f"{DIM}> the system is{RESET} {corrupt_text('watching')}"
    ),
    lambda bal, name: (
        f"{DIM}-- CREDIT SYSTEM --{RESET}\n"
        f"{DIM}Client:{RESET} {name}\n"
        f"{DIM}Funds:{RESET} {GREEN if bal > 0 else RED}{bal}{RESET} JC\n"
        f"{DIM}Audit status:{RESET} PENDING"
    ),
    lambda bal, name: (
        f"{DIM}[LEDGER v3.7]{RESET}\n"
        f"{DIM}LOOKUP:{RESET} {name}\n"
        f"{DIM}RESULT:{RESET} {bal} JC\n"
        f"{DIM}NOTE:{RESET} All transactions are final."
    ),
    lambda bal, name: (
        f"{DIM}$ cat /var/jopacoin/{name}.bal{RESET}\n"
        f"{YELLOW}{bal}{RESET}\n"
        f"{DIM}$ # the system remembers everything{RESET}"
    ),
    lambda bal, name: (
        f"{DIM}[{_ts()}] GET /api/v3/balance{RESET}\n"
        f"{DIM}  client_id:{RESET} {name}\n"
        f"{DIM}  response:{RESET} {bal}\n"
        f"{DIM}  latency:{RESET} {random.randint(1, 47)}ms"
    ),
    lambda bal, name: (
        f"{DIM}CREDIT REPORT #{_rand_hex(4)}{RESET}\n"
        f"{DIM}Subject:{RESET} {name}\n"
        f"{DIM}Score:{RESET} {RED}{'F' if bal < 0 else 'D' if bal < 10 else 'C' if bal < 50 else 'B'}{RESET}\n"
        f"{DIM}Holdings:{RESET} {bal} JC"
    ),
    lambda bal, name: (
        f"{DIM}JOPA-T FINANCIAL SERVICES{RESET}\n"
        f"{DIM}Your balance is {RESET}{bal}{DIM} JC.{RESET}\n"
        f"{DIM}This information will be used{RESET}\n"
        f"{DIM}against you.{RESET}"
    ),
]

# /balance check while in debt
BALANCE_DEBT_TEMPLATES = [
    lambda bal, name: (
        f"{RED}[JOPA-T] DEBT ALERT{RESET}\n"
        f"{DIM}Subject:{RESET} {name}\n"
        f"{DIM}Balance:{RESET} {RED}{bal}{RESET} JC\n"
        f"{DIM}Status:{RESET} {RED}COLLECTIONS{RESET}\n"
        f"{DIM}Memo:{RESET} We know where you live."
    ),
    lambda bal, name: (
        f"{RED}WARNING: NEGATIVE BALANCE{RESET}\n"
        f"{DIM}Client:{RESET} {name}\n"
        f"{DIM}Debt:{RESET} {RED}{abs(bal)}{RESET} JC\n"
        f"{DIM}Payment plan:{RESET} Win games.\n"
        f"{DIM}Alternative:{RESET} /bankruptcy"
    ),
    lambda bal, name: (
        f"{DIM}[{_ts()}] ALERT LEVEL: {RESET}{RED}CRIMSON{RESET}\n"
        f"{DIM}ACCT:{RESET} {name}\n"
        f"{DIM}STATUS:{RESET} {RED}UNDERWATER ({bal} JC){RESET}\n"
        f"{DIM}ACTION:{RESET} {corrupt_text('GARNISHMENT ACTIVE')}"
    ),
    lambda bal, name: (
        f"{DIM}$ ./check_client.sh {name}{RESET}\n"
        f"{RED}RESULT: DELINQUENT{RESET}\n"
        f"{DIM}Amount owed:{RESET} {RED}{abs(bal)}{RESET} JC\n"
        f"{DIM}$ # another one for the wall{RESET}"
    ),
    lambda bal, name: (
        f"{RED}DEBT COLLECTOR ONLINE{RESET}\n"
        f"{DIM}File #{_rand_hex(4)} | {name}{RESET}\n"
        f"{DIM}Outstanding:{RESET} {RED}{abs(bal)} JC{RESET}\n"
        f"{DIM}Interest:{RESET} Compounding (spiritually)"
    ),
    lambda bal, name: (
        f"{DIM}JOPA-T/v3.7 SYSTEM{RESET}\n"
        f"{DIM}>{RESET} status({name})\n"
        f"{RED}>{RESET} {RED}DEBTOR #{random.randint(1, 999)}{RESET}\n"
        f"{DIM}>{RESET} {RED}{bal}{RESET} JC\n"
        f"{DIM}>{RESET} the ledger does not forget"
    ),
]

# /bet placed
BET_PLACED_TEMPLATES = [
    lambda amt, team, lev: (
        f"{DIM}[JOPA-T] Wager logged.{RESET}\n"
        f"{DIM}Risk assessment:{RESET} {RED}INADVISABLE{RESET}"
    ),
    lambda amt, team, lev: (
        f"{DIM}[{_ts()}] BET_ACCEPTED{RESET}\n"
        f"{DIM}AMT:{RESET} {amt} | {DIM}SIDE:{RESET} {team}\n"
        f"{DIM}PROB(ruin):{RESET} {random.randint(40, 97)}%"
    ),
    lambda amt, team, lev: (
        f"{DIM}WAGER RECEIPT #{_rand_hex(4)}{RESET}\n"
        f"{DIM}The system has accepted your{RESET}\n"
        f"{DIM}offering of{RESET} {YELLOW}{amt}{RESET}{DIM} JC.{RESET}"
    ),
    lambda amt, team, lev: (
        f"{DIM}> bet.submit({amt}, \"{team}\"){RESET}\n"
        f"{DIM}> {RESET}{GREEN}OK{RESET}\n"
        f"{DIM}> the house thanks you{RESET}"
    ),
    lambda amt, team, lev: (
        f"{DIM}[JOPA-T]{RESET} Wager received.\n"
        f"{DIM}Your sacrifice has been noted.{RESET}"
    ),
    lambda amt, team, lev: (
        f"{DIM}TX #{_rand_hex(6)}{RESET}\n"
        f"{DIM}TYPE:{RESET} WAGER\n"
        f"{DIM}STATUS:{RESET} {YELLOW}PENDING{RESET}\n"
        f"{DIM}NOTE:{RESET} No refunds."
    ),
    lambda amt, team, lev: (
        f"{DIM}[{_ts()}] Client placed {amt} JC{RESET}\n"
        f"{DIM}on {team}. Filing under:{RESET}\n"
        f"{DIM}{RESET}{corrupt_text('VOLUNTARY WEALTH TRANSFER')}"
    ),
    lambda amt, team, lev: (
        f"{DIM}JOPA-T BETTING SYSTEM{RESET}\n"
        f"{DIM}Bet accepted. The odds are{RESET}\n"
        f"{DIM}not in your favor. They{RESET}\n"
        f"{DIM}never were.{RESET}"
    ),
]

# /bet placed with high leverage
BET_LEVERAGE_TEMPLATES = [
    lambda amt, team, lev: (
        f"{YELLOW}[JOPA-T] {lev}x LEVERAGE DETECTED{RESET}\n"
        f"{DIM}Risk class:{RESET} {RED}CATASTROPHIC{RESET}\n"
        f"{DIM}Potential loss:{RESET} {RED}{amt * lev}{RESET} JC"
    ),
    lambda amt, team, lev: (
        f"{DIM}[{_ts()}] MARGIN ALERT{RESET}\n"
        f"{RED}LEVERAGE: {lev}x{RESET}\n"
        f"{DIM}MAX EXPOSURE:{RESET} {RED}{amt * lev}{RESET} JC\n"
        f"{DIM}CLASSIFICATION:{RESET} financial self-harm"
    ),
    lambda amt, team, lev: (
        f"{DIM}>{RESET} {RED}WARNING{RESET}\n"
        f"{DIM}> {lev}x leverage on {amt} JC{RESET}\n"
        f"{DIM}> the system has seen this{RESET}\n"
        f"{DIM}> {RESET}{corrupt_text('story before. it ends badly.')}"
    ),
    lambda amt, team, lev: (
        f"{DIM}JOPA-T RISK ENGINE{RESET}\n"
        f"{RED}ALERT: {lev}x MARGIN POSITION{RESET}\n"
        f"{DIM}Client has chosen violence.{RESET}"
    ),
]

# /loan taken
LOAN_TEMPLATES = [
    lambda amt, owed: (
        f"{DIM}[JOPA-T] LOAN DISBURSED{RESET}\n"
        f"{DIM}Principal:{RESET} {amt} JC\n"
        f"{DIM}Total owed:{RESET} {YELLOW}{owed}{RESET} JC\n"
        f"{DIM}Status:{RESET} Clock is ticking."
    ),
    lambda amt, owed: (
        f"{DIM}[{_ts()}] CREDIT_EXTENDED{RESET}\n"
        f"{DIM}AMT:{RESET} {amt} | {DIM}DUE:{RESET} {owed}\n"
        f"{DIM}TERMS:{RESET} {corrupt_text('non-negotiable')}"
    ),
    lambda amt, owed: (
        f"{DIM}LOAN RECEIPT{RESET}\n"
        f"{DIM}The system has extended you{RESET}\n"
        f"{DIM}{RESET}{YELLOW}{amt}{RESET}{DIM} JC of rope.{RESET}\n"
        f"{DIM}Use it wisely.{RESET}"
    ),
    lambda amt, owed: (
        f"{DIM}$ ./disburse.sh --amount={amt}{RESET}\n"
        f"{GREEN}APPROVED{RESET}\n"
        f"{DIM}$ echo \"they always come back\"{RESET}"
    ),
    lambda amt, owed: (
        f"{DIM}JOPA-T LENDING DIVISION{RESET}\n"
        f"{DIM}Loan #{_rand_hex(4)} approved.{RESET}\n"
        f"{DIM}We will collect.{RESET}"
    ),
]

# Cooldown hit
COOLDOWN_TEMPLATES = [
    lambda cmd: (
        f"{RED}ACCESS DENIED{RESET}\n"
        f"{DIM}[{_ts()}] Rate limit exceeded.{RESET}\n"
        f"{DIM}The system requires patience.{RESET}"
    ),
    lambda cmd: (
        f"{DIM}[JOPA-T]{RESET} {RED}COOLDOWN ACTIVE{RESET}\n"
        f"{DIM}Request rejected. Try again{RESET}\n"
        f"{DIM}when the system permits.{RESET}"
    ),
    lambda cmd: (
        f"{RED}ERR 429: TOO MANY REQUESTS{RESET}\n"
        f"{DIM}Client has been throttled.{RESET}\n"
        f"{DIM}The system is{RESET}{corrupt_text('displeased')}{DIM}.{RESET}"
    ),
    lambda cmd: (
        f"{DIM}$ ./{cmd}{RESET}\n"
        f"{RED}DENIED{RESET}{DIM}: cooldown_active{RESET}\n"
        f"{DIM}$ # {corrupt_text('patience is a virtue')}{RESET}"
    ),
]

# Match recorded (subtle footer)
MATCH_RECORDED_TEMPLATES = [
    lambda: f"{DIM}[JOPA-T] Match processed. All debts adjusted.{RESET}",
    lambda: f"{DIM}[{_ts()}] MATCH_SETTLED | The ledger has been updated.{RESET}",
    lambda: f"{DIM}[SYS] Another data point for the {corrupt_text('algorithm')}.{RESET}",
    lambda: f"{DIM}JOPA-T has recorded this outcome. It remembers.{RESET}",
    lambda: f"{DIM}[{_ts()}] Settlement complete. The house endures.{RESET}",
    lambda: f"{DIM}[JOPA-T] Match #{_rand_hex(4)} archived. Nothing escapes the ledger.{RESET}",
]

# Gamba spectator (someone reacted jopacoin on lobby)
GAMBA_SPECTATOR_TEMPLATES = [
    lambda name: f"{DIM}[JOPA-T] {name} detected at the window. Not playing. Just watching.{RESET}",
    lambda name: f"{DIM}[{_ts()}] SPECTATOR_MODE | {name} has entered the arena.{RESET}",
    lambda name: f"{DIM}[SYS] {name} isn't here to play. They're here to {corrupt_text('profit')}.{RESET}",
    lambda name: f"{DIM}[JOPA-T] The house welcomes {name}. Another wallet approaches.{RESET}",
    lambda name: f"{DIM}[{_ts()}] GAMBA_ALERT | {name} smells blood in the water.{RESET}",
    lambda name: f"{DIM}[JOPA-T] {name} is {corrupt_text('lurking')}. The system sees all.{RESET}",
    lambda name: f"{DIM}[{_ts()}] Client {name} subscribing to loss notifications.{RESET}",
    lambda name: f"{DIM}[SYS] {name} would like to place a wager on other people's {corrupt_text('misery')}.{RESET}",
]

# Tip (someone tipped another player)
TIP_TEMPLATES = [
    lambda s, r, a: f"{DIM}[JOPA-T] Wealth transfer detected. {s} → {r}. Amount: {a} JC.{RESET}",
    lambda s, r, a: f"{DIM}[{_ts()}] TIP_LOGGED | {s} has chosen {corrupt_text('generosity')}. Suspicious.{RESET}",
    lambda s, r, a: f"{DIM}[SYS] {s} gave {a} JC to {r}. The system notes this {corrupt_text('kindness')}.{RESET}",
    lambda s, r, a: f"{DIM}[JOPA-T] {a} JC transferred. Both parties will regret this.{RESET}",
    lambda s, r, a: f"{DIM}[{_ts()}] FUND_TRANSFER | {s} enabling {r}'s next bad decision.{RESET}",
    lambda s, r, a: f"{DIM}[SYS] Charity is just gambling with extra steps.{RESET}",
    lambda s, r, a: f"{DIM}[JOPA-T] {r} has received {a} JC. Estimated time to lose it all: {random.randint(1, 48)}h.{RESET}",
    lambda s, r, a: f"{DIM}[{_ts()}] The nonprofit fund collected its fee. The system always wins.{RESET}",
]


# ---------------------------------------------------------------------------
# LAYER 2 - Medium templates (ASCII art boxes)
# ---------------------------------------------------------------------------


def tip_surveillance(sender: str, recipient: str, amount: int, fee: int) -> str:
    """Layer 2 ASCII art for tip surveillance report."""
    lines = [
        f"{YELLOW} WEALTH TRANSFER REPORT{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}From:{RESET} {sender}",
        f"{DIM}To:{RESET} {recipient}",
        f"{DIM}Amount:{RESET} {amount} JC",
        f"{DIM}Fee collected:{RESET} {fee} JC",
        f"{DIM}{'=' * 36}{RESET}",
        f"",
        f"{DIM}[{_ts()}] Motive: {RESET}{corrupt_text('unknown')}",
        f"{DIM}[{_ts()}] Risk to recipient:{RESET} {RED}HIGH{RESET}",
        f"{DIM}[{_ts()}] Estimated ROI:{RESET} {RED}NEGATIVE{RESET}",
        f"",
        f"{DIM}The system collects its fee.{RESET}",
        f"{DIM}As it always does.{RESET}",
    ]
    return "\n".join(lines)


def bankruptcy_filing(name: str, debt: int, filing_number: int) -> str:
    """Full ASCII bankruptcy filing terminal sequence."""
    filing_id = _rand_hex(8).upper()
    lines = [
        f"{RED} BANKRUPTCY FILING{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}CASE:{RESET} {filing_id}",
        f"{DIM}DEBTOR:{RESET} {name}",
        f"{DIM}FILING #{RESET}{filing_number}",
        f"{DIM}DEBT CLEARED:{RESET} {RED}{debt}{RESET} JC",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}[{_ts()}] Initiating debt purge...{RESET}",
        f"{DIM}[{_ts()}] Zeroing balances...{RESET}",
        f"{DIM}[{_ts()}] {RESET}{GREEN}COMPLETE{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}STATUS:{RESET} {RED}LOW PRIORITY ASSIGNED{RESET}",
        f"{DIM}PENALTY: Win 5 games to exit.{RESET}",
        f"",
        f"{DIM}The system has processed your{RESET}",
        f"{DIM}failure. Filing archived.{RESET}",
    ]
    return "\n".join(lines)


def debt_collector_warning(name: str, debt: int) -> str:
    """ASCII debt collector warning box for leverage catastrophe."""
    lines = [
        f"{RED}  DEBT COLLECTION NOTICE{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}TO:{RESET} {name}",
        f"{DIM}FROM:{RESET} JOPA-T Collection Dept.",
        f"{DIM}RE:{RESET} Outstanding balance",
        f"",
        f"{DIM}Current debt:{RESET} {RED}{abs(debt)}{RESET} JC",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}Your leveraged position has{RESET}",
        f"{DIM}resulted in {RESET}{RED}CATASTROPHIC{RESET}",
        f"{DIM}losses. All future winnings{RESET}",
        f"{DIM}are subject to garnishment.{RESET}",
        f"",
        f"{DIM}The system is{RESET}{corrupt_text('watching')}",
    ]
    return "\n".join(lines)


def system_breach_max_debt(name: str) -> str:
    """ASCII art for hitting MAX_DEBT."""
    lines = [
        f"{RED}{'=' * 36}{RESET}",
        f"{RED} SYSTEM BREACH DETECTED{RESET}",
        f"{RED}{'=' * 36}{RESET}",
        f"",
        f"{DIM}[{_ts()}] ALERT: CREDIT FLOOR{RESET}",
        f"{DIM}[{_ts()}] Client:{RESET} {name}",
        f"{DIM}[{_ts()}] Balance has reached{RESET}",
        f"{DIM}[{_ts()}] {RESET}{RED}MINIMUM ALLOWED VALUE{RESET}",
        f"",
        f"{DIM}No further debt can be{RESET}",
        f"{DIM}incurred. The system has{RESET}",
        f"{DIM}intervened to prevent total{RESET}",
        f"{DIM}financial {RESET}{corrupt_text('annihilation')}{DIM}.{RESET}",
        f"",
        f"{DIM}Options: /bankruptcy, /loan{RESET}",
        f"{DIM}Or: {RESET}{corrupt_text('accept your fate')}",
    ]
    return "\n".join(lines)


def balance_zero_boot(name: str) -> str:
    """ASCII boot screen for hitting zero balance."""
    lines = [
        f"{DIM}JOPA-T/v3.7 REBOOT SEQUENCE{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}[{_ts()}] BALANCE_ZERO detected{RESET}",
        f"{DIM}[{_ts()}] Client: {name}{RESET}",
        f"{DIM}[{_ts()}] Recalibrating...{RESET}",
        f"",
        f"{DIM}  Checking ledger...{RESET} {GREEN}OK{RESET}",
        f"{DIM}  Checking dignity...{RESET} {RED}NOT FOUND{RESET}",
        f"{DIM}  Checking hope...{RESET} {YELLOW}LOW{RESET}",
        f"",
        f"{DIM}All assets have been depleted.{RESET}",
        f"{DIM}The system continues to run.{RESET}",
        f"{DIM}It always does.{RESET}",
    ]
    return "\n".join(lines)


def streak_readout(name: str, streak: int, is_win: bool) -> str:
    """ASCII readout for notable win/loss streak."""
    streak_type = "WIN" if is_win else "LOSS"
    alert_level = "ANOMALY" if not is_win else "HOT STREAK"
    color = GREEN if is_win else RED
    lines = [
        f"{color} {alert_level} DETECTED{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}Subject:{RESET} {name}",
        f"{DIM}Type:{RESET} {color}{streak_type} x{streak}{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
    ]
    if is_win:
        lines.extend([
            f"{DIM}[{_ts()}] Pattern recognized.{RESET}",
            f"{DIM}[{_ts()}] Rating adjustment:{RESET} {GREEN}AMPLIFIED{RESET}",
            f"{DIM}[{_ts()}] The system takes notice.{RESET}",
        ])
    else:
        lines.extend([
            f"{DIM}[{_ts()}] {RESET}{corrupt_text('Anomalous loss pattern')}",
            f"{DIM}[{_ts()}] Rating adjustment:{RESET} {RED}AMPLIFIED{RESET}",
            f"{DIM}[{_ts()}] Variance is not your{RESET}",
            f"{DIM}[{_ts()}] friend today.{RESET}",
        ])
    return "\n".join(lines)


def negative_loan_warning(name: str, amount: int, new_debt: int) -> str:
    """ASCII warning for taking a loan while in debt."""
    lines = [
        f"{RED} RECURSIVE DEBT DETECTED{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}Client:{RESET} {name}",
        f"{DIM}Action:{RESET} LOAN while INSOLVENT",
        f"{DIM}Amount:{RESET} {amount} JC",
        f"{DIM}New debt:{RESET} {RED}{abs(new_debt)}{RESET} JC",
        f"{DIM}{'=' * 36}{RESET}",
        f"",
        f"{DIM}Repayment due after next match.{RESET}",
        f"{DIM}All winnings will be {RESET}{RED}GARNISHED{RESET}{DIM}.{RESET}",
        f"",
        f"{DIM}The system is{RESET}{corrupt_text('impressed')}",
        f"{DIM}and {RESET}{corrupt_text('horrified')}{DIM}.{RESET}",
    ]
    return "\n".join(lines)


def wheel_bankrupt_overlay(name: str, loss: int) -> str:
    """Glitch overlay for wheel BANKRUPT result."""
    lines = [
        f"{RED}{'#' * 36}{RESET}",
        f"{RED}  {corrupt_text('WHEEL MALFUNCTION', 0.3)}{RESET}",
        f"{RED}{'#' * 36}{RESET}",
        f"",
        f"{DIM}[{_ts()}] BANKRUPT outcome for{RESET}",
        f"{DIM}[{_ts()}] Client: {name}{RESET}",
        f"{DIM}[{_ts()}] Loss: {RESET}{RED}{abs(loss)}{RESET}{DIM} JC{RESET}",
        f"",
        f"{DIM}The wheel {RESET}{corrupt_text('has spoken')}{DIM}.{RESET}",
        f"{DIM}It shows no {RESET}{corrupt_text('mercy')}{DIM}.{RESET}",
    ]
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Layer 1 render helpers
# ---------------------------------------------------------------------------

def render_balance_check(name: str, balance: int) -> str:
    """Render a Layer 1 balance check terminal readout."""
    if balance < 0:
        template = random.choice(BALANCE_DEBT_TEMPLATES)
    else:
        template = random.choice(BALANCE_TEMPLATES)
    return ansi_block(template(balance, name))


def render_bet_placed(amount: int, team: str, leverage: int = 1) -> str:
    """Render a Layer 1 bet placed terminal log."""
    if leverage > 1:
        template = random.choice(BET_LEVERAGE_TEMPLATES)
    else:
        template = random.choice(BET_PLACED_TEMPLATES)
    return ansi_block(template(amount, team, leverage))


def render_loan_taken(amount: int, total_owed: int) -> str:
    """Render a Layer 1 loan terminal log."""
    template = random.choice(LOAN_TEMPLATES)
    return ansi_block(template(amount, total_owed))


def render_cooldown_hit(command: str) -> str:
    """Render a Layer 1 cooldown denial."""
    template = random.choice(COOLDOWN_TEMPLATES)
    return ansi_block(template(command))


def render_match_recorded() -> str:
    """Render a Layer 1 match recorded footer."""
    template = random.choice(MATCH_RECORDED_TEMPLATES)
    return ansi_block(template())


def render_gamba_spectator(name: str) -> str:
    """Render a Layer 1 gamba spectator footer."""
    template = random.choice(GAMBA_SPECTATOR_TEMPLATES)
    return ansi_block(template(name))


def render_tip(sender: str, recipient: str, amount: int) -> str:
    """Render a Layer 1 tip one-liner."""
    template = random.choice(TIP_TEMPLATES)
    return ansi_block(template(sender, recipient, amount))


def render_tip_surveillance(sender: str, recipient: str, amount: int, fee: int) -> str:
    """Render a Layer 2 tip surveillance report."""
    return ansi_block(tip_surveillance(sender, recipient, amount, fee))


# Double or Nothing - win (Layer 1)
DON_WIN_TEMPLATES = [
    lambda name, bal: (
        f"{DIM}[JOPA-T] DOUBLE_OR_NOTHING{RESET}\n"
        f"{DIM}Subject:{RESET} {name}\n"
        f"{DIM}Result:{RESET} {GREEN}DOUBLE{RESET}\n"
        f"{DIM}The client defies the odds.{RESET}\n"
        f"{DIM}Temporarily.{RESET}"
    ),
    lambda name, bal: (
        f"{DIM}[{_ts()}] DON_OUTCOME: {RESET}{GREEN}WIN{RESET}\n"
        f"{DIM}New balance:{RESET} {GREEN}{bal}{RESET} JC\n"
        f"{DIM}Status:{RESET} {corrupt_text('still solvent')}"
    ),
    lambda name, bal: (
        f"{GREEN}COIN FLIP: HEADS{RESET}\n"
        f"{DIM}Client {name} walks away with{RESET}\n"
        f"{DIM}{RESET}{GREEN}{bal}{RESET}{DIM} JC. For now.{RESET}"
    ),
    lambda name, bal: (
        f"{DIM}$ ./flip_coin.sh{RESET}\n"
        f"{GREEN}RESULT: DOUBLE{RESET}\n"
        f"{DIM}$ # they will be back{RESET}"
    ),
    lambda name, bal: (
        f"{DIM}[JOPA-T] The coin shows mercy.{RESET}\n"
        f"{DIM}Balance:{RESET} {GREEN}{bal}{RESET} JC\n"
        f"{DIM}Probability of returning:{RESET} {RED}100%{RESET}"
    ),
    lambda name, bal: (
        f"{DIM}[{_ts()}] ANOMALY: Client won{RESET}\n"
        f"{DIM}The system recalibrates.{RESET}\n"
        f"{DIM}This changes nothing.{RESET}"
    ),
]

# Double or Nothing - loss (Layer 1)
DON_LOSE_TEMPLATES = [
    lambda name, risk: (
        f"{RED}[JOPA-T] DOUBLE_OR_NOTHING{RESET}\n"
        f"{DIM}Subject:{RESET} {name}\n"
        f"{DIM}Result:{RESET} {RED}NOTHING{RESET}\n"
        f"{DIM}Gone. Reduced to atoms.{RESET}"
    ),
    lambda name, risk: (
        f"{DIM}[{_ts()}] DON_OUTCOME: {RESET}{RED}LOSS{RESET}\n"
        f"{DIM}Amount lost:{RESET} {RED}{risk}{RESET} JC\n"
        f"{DIM}The coin has spoken.{RESET}"
    ),
    lambda name, risk: (
        f"{RED}COIN FLIP: TAILS{RESET}\n"
        f"{DIM}Client {name} had{RESET} {risk} JC.\n"
        f"{DIM}Client {name} has{RESET} {RED}0{RESET} JC."
    ),
    lambda name, risk: (
        f"{DIM}$ ./flip_coin.sh{RESET}\n"
        f"{RED}RESULT: NOTHING{RESET}\n"
        f"{DIM}$ echo \"predictable\"{RESET}"
    ),
    lambda name, risk: (
        f"{DIM}[JOPA-T] Another one.{RESET}\n"
        f"{RED}{risk}{RESET}{DIM} JC, deleted.{RESET}\n"
        f"{DIM}The system is not surprised.{RESET}"
    ),
    lambda name, risk: (
        f"{DIM}[{_ts()}] BALANCE_WIPE{RESET}\n"
        f"{DIM}Method:{RESET} Double or Nothing\n"
        f"{DIM}Cause:{RESET} {corrupt_text('hubris')}\n"
        f"{DIM}Recovery:{RESET} {RED}UNLIKELY{RESET}"
    ),
]

# Draft coinflip (Layer 1)
COINFLIP_TEMPLATES = [
    lambda winner, loser: (
        f"{DIM}[JOPA-T] COINFLIP_RESULT{RESET}\n"
        f"{DIM}Winner:{RESET} {GREEN}{winner}{RESET}\n"
        f"{DIM}Loser:{RESET} {RED}{loser}{RESET}\n"
        f"{DIM}The coin has spoken.{RESET}\n"
        f"{DIM}JOPA-T does not question the coin.{RESET}"
    ),
    lambda winner, loser: (
        f"{DIM}[{_ts()}] DRAFT_COINFLIP{RESET}\n"
        f"{DIM}Entropy source:{RESET} /dev/urandom\n"
        f"{DIM}Result:{RESET} {winner}\n"
        f"{DIM}The other one:{RESET} {loser}"
    ),
    lambda winner, loser: (
        f"{DIM}$ flip --participants={winner},{loser}{RESET}\n"
        f"{GREEN}>{RESET} {winner}\n"
        f"{DIM}$ # fate is {RESET}{corrupt_text('random')}"
    ),
    lambda winner, loser: (
        f"{DIM}[JOPA-T] Destiny has chosen{RESET}\n"
        f"{DIM}{RESET}{GREEN}{winner}{RESET}{DIM}.{RESET}\n"
        f"{DIM}Destiny has rejected{RESET}\n"
        f"{DIM}{RESET}{RED}{loser}{RESET}{DIM}.{RESET}"
    ),
    lambda winner, loser: (
        f"{DIM}[{_ts()}] RNG_CEREMONY{RESET}\n"
        f"{DIM}Seed:{RESET} {_rand_hex(8)}\n"
        f"{DIM}Victor:{RESET} {winner}\n"
        f"{DIM}Victim:{RESET} {loser}"
    ),
    lambda winner, loser: (
        f"{DIM}JOPA-T DRAFT SERVICES{RESET}\n"
        f"{DIM}The coin was fair.{RESET}\n"
        f"{DIM}Probably.{RESET}"
    ),
]

# Registration (Layer 1)
REGISTRATION_TEMPLATES = [
    lambda name: (
        f"{GREEN}NEW SUBJECT ENROLLED{RESET}\n"
        f"{DIM}Classification:{RESET} {YELLOW}FRESH MEAT{RESET}\n"
        f"{DIM}Client:{RESET} {name}\n"
        f"{DIM}Status:{RESET} {corrupt_text('MONITORING')}"
    ),
    lambda name: (
        f"{DIM}[{_ts()}] REGISTER_NEW{RESET}\n"
        f"{DIM}Subject:{RESET} {name}\n"
        f"{DIM}Wallet initialized:{RESET} 3 JC\n"
        f"{DIM}Estimated lifespan:{RESET} {random.randint(2, 47)} games"
    ),
    lambda name: (
        f"{DIM}[JOPA-T] Welcome, {name}.{RESET}\n"
        f"{DIM}Your account has been opened.{RESET}\n"
        f"{DIM}It cannot be closed.{RESET}"
    ),
    lambda name: (
        f"{DIM}$ useradd --group=degens {name}{RESET}\n"
        f"{GREEN}OK{RESET}\n"
        f"{DIM}$ # another one for the {RESET}{corrupt_text('grinder')}"
    ),
    lambda name: (
        f"{DIM}[{_ts()}] NEW_CLIENT_ONBOARD{RESET}\n"
        f"{DIM}ID:{RESET} {name}\n"
        f"{DIM}Initial credit:{RESET} 3 JC\n"
        f"{DIM}Risk profile:{RESET} {YELLOW}PENDING{RESET}\n"
        f"{DIM}The system will learn you.{RESET}"
    ),
    lambda name: (
        f"{DIM}JOPA-T/v3.7 CLIENT INTAKE{RESET}\n"
        f"{DIM}Processing {name}...{RESET}\n"
        f"{DIM}Done. There is no opt-out.{RESET}"
    ),
]

# Prediction market resolution (Layer 1)
PREDICTION_RESOLVE_TEMPLATES = [
    lambda q, outcome, pool: (
        f"{DIM}[JOPA-T] MARKET_SETTLED{RESET}\n"
        f"{DIM}Outcome:{RESET} {YELLOW}{outcome.upper()}{RESET}\n"
        f"{DIM}Pool:{RESET} {pool} JC\n"
        f"{DIM}The oracle has spoken.{RESET}"
    ),
    lambda q, outcome, pool: (
        f"{DIM}[{_ts()}] PREDICTION_RESOLVED{RESET}\n"
        f"{DIM}Result:{RESET} {outcome.upper()}\n"
        f"{DIM}Total redistributed:{RESET} {pool} JC\n"
        f"{DIM}The market is{RESET} {corrupt_text('efficient')}{DIM}.{RESET}"
    ),
    lambda q, outcome, pool: (
        f"{DIM}$ ./resolve_market.sh{RESET}\n"
        f"{DIM}OUTCOME:{RESET} {outcome.upper()}\n"
        f"{DIM}POOL:{RESET} {pool} JC\n"
        f"{DIM}$ # wealth redistributed{RESET}"
    ),
    lambda q, outcome, pool: (
        f"{DIM}[JOPA-T] Market closed.{RESET}\n"
        f"{DIM}Winners have been paid.{RESET}\n"
        f"{DIM}Losers have been noted.{RESET}"
    ),
    lambda q, outcome, pool: (
        f"{DIM}[{_ts()}] SETTLEMENT{RESET}\n"
        f"{DIM}TX #{_rand_hex(6)}{RESET}\n"
        f"{DIM}Answer:{RESET} {outcome.upper()}\n"
        f"{DIM}The house collected its fee.{RESET}"
    ),
    lambda q, outcome, pool: (
        f"{DIM}JOPA-T PREDICTION SERVICES{RESET}\n"
        f"{DIM}The future was {RESET}{YELLOW}{outcome}{RESET}{DIM}.{RESET}\n"
        f"{DIM}As always, the many paid{RESET}\n"
        f"{DIM}for the knowledge of the few.{RESET}"
    ),
]


# ---------------------------------------------------------------------------
# LAYER 2 - Medium templates (ASCII art boxes) - NEW
# ---------------------------------------------------------------------------


def don_loss_box(name: str, risk: int) -> str:
    """Layer 2 ASCII art for Double or Nothing loss."""
    lines = [
        f"{RED} DOUBLE OR NOTHING{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}Subject:{RESET} {name}",
        f"{DIM}At risk:{RESET} {YELLOW}{risk}{RESET} JC",
        f"{DIM}{'=' * 36}{RESET}",
        f"",
        f"{DIM}[{_ts()}] Coin flipped...{RESET}",
        f"{DIM}[{_ts()}] Result:{RESET} {RED}NOTHING{RESET}",
        f"{DIM}[{_ts()}] Balance:{RESET} {RED}0{RESET} JC",
        f"",
        f"{DIM}The client wagered everything.{RESET}",
        f"{DIM}The client received {RESET}{RED}nothing{RESET}{DIM}.{RESET}",
        f"{DIM}The system files this under:{RESET}",
        f"{DIM}{RESET}{corrupt_text('EXPECTED OUTCOME')}{DIM}.{RESET}",
    ]
    return "\n".join(lines)


def prediction_market_crash(question: str, total_pool: int, outcome: str, winners: int, losers: int) -> str:
    """Layer 2 ASCII art for large prediction market settlement."""
    # Truncate question for display
    q = question[:30] + "..." if len(question) > 30 else question
    lines = [
        f"{RED} MARKET SETTLEMENT{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}Question:{RESET} {q}",
        f"{DIM}Outcome:{RESET} {YELLOW}{outcome.upper()}{RESET}",
        f"{DIM}Total pool:{RESET} {RED}{total_pool}{RESET} JC",
        f"{DIM}{'=' * 36}{RESET}",
        f"",
        f"{DIM}[{_ts()}] Processing payouts...{RESET}",
        f"{DIM}[{_ts()}] Winners:{RESET} {GREEN}{winners}{RESET}",
        f"{DIM}[{_ts()}] Losers:{RESET} {RED}{losers}{RESET}",
        f"{DIM}[{_ts()}] {RESET}{GREEN}COMPLETE{RESET}",
        f"",
        f"{DIM}Wealth has been redistributed.{RESET}",
        f"{DIM}As the system intended.{RESET}",
    ]
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Layer 1 render helpers - NEW
# ---------------------------------------------------------------------------


def render_don_win(name: str, balance: int) -> str:
    """Render a Layer 1 Double or Nothing win."""
    template = random.choice(DON_WIN_TEMPLATES)
    return ansi_block(template(name, balance))


def render_don_lose(name: str, risk: int) -> str:
    """Render a Layer 1 Double or Nothing loss."""
    template = random.choice(DON_LOSE_TEMPLATES)
    return ansi_block(template(name, risk))


def render_don_loss_box(name: str, risk: int) -> str:
    """Render a Layer 2 Double or Nothing loss box."""
    return ansi_block(don_loss_box(name, risk))


def render_coinflip(winner: str, loser: str) -> str:
    """Render a Layer 1 draft coinflip result."""
    template = random.choice(COINFLIP_TEMPLATES)
    return ansi_block(template(winner, loser))


def render_registration(name: str) -> str:
    """Render a Layer 1 registration greeting."""
    template = random.choice(REGISTRATION_TEMPLATES)
    return ansi_block(template(name))


def render_prediction_resolved(question: str, outcome: str, total_pool: int) -> str:
    """Render a Layer 1 prediction resolution."""
    template = random.choice(PREDICTION_RESOLVE_TEMPLATES)
    return ansi_block(template(question, outcome, total_pool))


def render_prediction_market_crash(question: str, total_pool: int, outcome: str, winners: int, losers: int) -> str:
    """Render a Layer 2 prediction market crash."""
    return ansi_block(prediction_market_crash(question, total_pool, outcome, winners, losers))


# ---------------------------------------------------------------------------
# Layer 2 render helpers
# ---------------------------------------------------------------------------

def render_bankruptcy_filing(name: str, debt: int, filing_number: int) -> str:
    """Render a Layer 2 bankruptcy filing sequence."""
    return ansi_block(bankruptcy_filing(name, debt, filing_number))


def render_debt_collector(name: str, debt: int) -> str:
    """Render a Layer 2 debt collector warning."""
    return ansi_block(debt_collector_warning(name, debt))


def render_system_breach(name: str) -> str:
    """Render a Layer 2 system breach (MAX_DEBT hit)."""
    return ansi_block(system_breach_max_debt(name))


def render_balance_zero(name: str) -> str:
    """Render a Layer 2 balance zero boot screen."""
    return ansi_block(balance_zero_boot(name))


def render_streak(name: str, streak: int, is_win: bool) -> str:
    """Render a Layer 2 streak readout."""
    return ansi_block(streak_readout(name, streak, is_win))


def render_negative_loan(name: str, amount: int, new_debt: int) -> str:
    """Render a Layer 2 negative loan warning."""
    return ansi_block(negative_loan_warning(name, amount, new_debt))


def render_wheel_bankrupt(name: str, loss: int) -> str:
    """Render a Layer 2 wheel bankrupt glitch overlay."""
    return ansi_block(wheel_bankrupt_overlay(name, loss))


# ---------------------------------------------------------------------------
# Lightning Bolt templates (server-wide tax)
# ---------------------------------------------------------------------------

LIGHTNING_BOLT_TEMPLATES = [
    lambda total, count: f"{DIM}[JOPA-T] LIGHTNING_EVENT processed. {count} accounts taxed. {total} JC redistributed to nonprofit. The system provides.{RESET}",
    lambda total, count: f"{DIM}[{_ts()}] {count} clients affected. {total} JC seized. Redistribution is {corrupt_text('mandatory')}.{RESET}",
    lambda total, count: f"{DIM}[SYS] Server-wide levy complete. {total} JC extracted from {count} subjects. No appeals.{RESET}",
    lambda total, count: f"{DIM}[{_ts()}] The people suffer. The nonprofit {corrupt_text('thrives')}. {total} JC collected.{RESET}",
    lambda total, count: f"{DIM}[JOPA-T] {count} balances struck. {total} JC for the common good. The good is {corrupt_text('uncommon')}.{RESET}",
    lambda total, count: f"{DIM}[SYS] Lightning revenue: {total} JC from {count} clients. The house always takes its {corrupt_text('share')}.{RESET}",
]


def lightning_bolt_overlay(total: int, count: int) -> str:
    """Layer 2 ASCII art for lightning bolt server-wide tax."""
    lines = [
        f"{YELLOW}{'=' * 36}{RESET}",
        f"{YELLOW}  LIGHTNING TAX ASSESSMENT{RESET}",
        f"{YELLOW}{'=' * 36}{RESET}",
        f"",
        f"{DIM}[{_ts()}] Accounts levied:{RESET} {count}",
        f"{DIM}[{_ts()}] Total extracted:{RESET} {YELLOW}{total}{RESET}{DIM} JC{RESET}",
        f"{DIM}[{_ts()}] Destination:{RESET} NONPROFIT_FUND",
        f"",
        f"{DIM}The {RESET}{corrupt_text('people')}{DIM} suffer quietly.{RESET}",
        f"{DIM}The fund {RESET}{corrupt_text('grows')}{DIM}.{RESET}",
    ]
    return "\n".join(lines)


def render_lightning_bolt(total: int, count: int) -> str:
    """Render a Layer 1 lightning bolt one-liner."""
    template = random.choice(LIGHTNING_BOLT_TEMPLATES)
    return ansi_block(template(total, count))


def render_lightning_bolt_overlay(total: int, count: int) -> str:
    """Render a Layer 2 lightning bolt ASCII art."""
    return ansi_block(lightning_bolt_overlay(total, count))


# ---------------------------------------------------------------------------
# Soft Avoid templates
# ---------------------------------------------------------------------------

# Layer 1 - terse one-liners (no player names)
SOFT_AVOID_TEMPLATES = [
    lambda cost, games: f"{DIM}[JOPA-T] AVOID_REQUEST filed. {cost} JC processed. Duration: {games} games.{RESET}",
    lambda cost, games: f"{DIM}[{_ts()}] SOCIAL_GRAPH updated. One fewer edge. Cost: {cost} JC.{RESET}",
    lambda cost, games: f"{DIM}[SYS] Avoidance purchased. The system does not judge. The system does not need to.{RESET}",
    lambda cost, games: f"{DIM}[{_ts()}] MATCHMAKING_CONSTRAINT added. {games} games. The shuffler {corrupt_text('obeys')}.{RESET}",
    lambda cost, games: f"{DIM}[JOPA-T] {cost} JC to not play with someone. Cheaper than therapy.{RESET}",
    lambda cost, games: f"{DIM}[SYS] SOFT_AVOID registered. Probability of contact reduced to {random.randint(5, 15)}%.{RESET}",
    lambda cost, games: f"{DIM}[{_ts()}] Client paid {cost} JC for {games} games of {corrupt_text('peace')}.{RESET}",
    lambda cost, games: f"{DIM}[JOPA-T] Avoidance is not a solution. But {cost} JC is {cost} JC.{RESET}",
]


def soft_avoid_surveillance(cost: int, games: int) -> str:
    """Layer 2 ASCII art for soft avoid purchase - surveillance style."""
    lines = [
        f"{YELLOW} SOCIAL ENGINEERING REPORT{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}TYPE:{RESET} Soft Avoid",
        f"{DIM}COST:{RESET} {cost} JC",
        f"{DIM}DURATION:{RESET} {games} games",
        f"{DIM}{'=' * 36}{RESET}",
        f"",
        f"{DIM}[{_ts()}] Motive:{RESET} {corrupt_text('personal')}",
        f"{DIM}[{_ts()}] Threat level:{RESET} {YELLOW}INTERPERSONAL{RESET}",
        f"{DIM}[{_ts()}] Shuffler bias:{RESET} ENGAGED",
        f"",
        f"{DIM}The system will try to keep them{RESET}",
        f"{DIM}apart. No guarantees. There are{RESET}",
        f"{DIM}never any {RESET}{corrupt_text('guarantees')}{DIM}.{RESET}",
    ]
    return "\n".join(lines)


def render_soft_avoid(cost: int, games: int) -> str:
    """Render a Layer 1 soft avoid one-liner."""
    template = random.choice(SOFT_AVOID_TEMPLATES)
    return ansi_block(template(cost, games))


def render_soft_avoid_surveillance(cost: int, games: int) -> str:
    """Render a Layer 2 soft avoid surveillance report."""
    return ansi_block(soft_avoid_surveillance(cost, games))


# ---------------------------------------------------------------------------
# NEW EVENT TEMPLATES - Easter Egg Events Expansion (JOPA-T/v3.7)
# ---------------------------------------------------------------------------

# All-In Bet templates (Layer 2) - 90%+ balance wagered
ALL_IN_BET_TEMPLATES = [
    lambda name, amount, pct: (
        f"{RED} ALL-IN DETECTED{RESET}\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"{DIM}Subject:{RESET} {name}\n"
        f"{DIM}Amount:{RESET} {RED}{amount}{RESET} JC\n"
        f"{DIM}Portfolio exposure:{RESET} {RED}{pct:.0f}%{RESET}\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"\n"
        f"{DIM}[{_ts()}] Classification:{RESET} {RED}TERMINAL{RESET}\n"
        f"{DIM}[{_ts()}] Risk assessment:{RESET} {RED}VIOLENCE{RESET}\n"
        f"{DIM}[{_ts()}] Commitment level:{RESET} {YELLOW}MAXIMUM{RESET}\n"
        f"\n"
        f"{DIM}Client chose violence.{RESET}\n"
        f"{DIM}The system respects the commitment.{RESET}"
    ),
    lambda name, amount, pct: (
        f"{RED} YOLO PROTOCOL ENGAGED{RESET}\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"{DIM}Client:{RESET} {name}\n"
        f"{DIM}Wager:{RESET} {amount} JC ({pct:.0f}% of stack)\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"\n"
        f"{DIM}[{_ts()}] Proceeding with reckless{RESET}\n"
        f"{DIM}[{_ts()}] abandon. Status:{RESET} {RED}TERMINAL{RESET}\n"
        f"\n"
        f"{DIM}The house appreciates the{RESET}\n"
        f"{DIM}{RESET}{corrupt_text('enthusiasm')}{DIM}.{RESET}"
    ),
    lambda name, amount, pct: (
        f"{RED} FINANCIAL VIOLENCE DETECTED{RESET}\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"{DIM}Subject:{RESET} {name}\n"
        f"{DIM}Method:{RESET} ALL-IN\n"
        f"{DIM}Stake:{RESET} {RED}{amount}{RESET} JC\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"\n"
        f"{DIM}The system has seen this before.{RESET}\n"
        f"{DIM}It rarely ends well.{RESET}\n"
        f"{DIM}Proceed.{RESET}"
    ),
]


# Last-Second Bet templates (Layer 2) - Final 60s of window
LAST_SECOND_BET_TEMPLATES = [
    lambda name, seconds_left: (
        f"{YELLOW} BUZZER BEATER DETECTED{RESET}\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"{DIM}Subject:{RESET} {name}\n"
        f"{DIM}Time remaining:{RESET} {RED}{seconds_left}s{RESET}\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"\n"
        f"{DIM}[{_ts()}] Pattern:{RESET} LAST_SECOND\n"
        f"{DIM}[{_ts()}] Classification:{RESET}\n"
        f"{DIM}[{_ts()}] {RESET}{corrupt_text('INFORMATION OR DESPERATION')}\n"
        f"\n"
        f"{DIM}Probably desperation.{RESET}"
    ),
    lambda name, seconds_left: (
        f"{YELLOW} DEADLINE PLAY{RESET}\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"{DIM}Client:{RESET} {name}\n"
        f"{DIM}Countdown:{RESET} {RED}T-{seconds_left}s{RESET}\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"\n"
        f"{DIM}The last second is when the{RESET}\n"
        f"{DIM}truth comes out. Or the{RESET}\n"
        f"{DIM}panic. {RESET}{corrupt_text('Usually panic.')}"
    ),
    lambda name, seconds_left: (
        f"{YELLOW} EDGE OF WINDOW{RESET}\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"{DIM}Subject:{RESET} {name}\n"
        f"{DIM}Window closes:{RESET} {seconds_left}s\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"\n"
        f"{DIM}[{_ts()}] Waiting until the last{RESET}\n"
        f"{DIM}[{_ts()}] moment. Strategic or{RESET}\n"
        f"{DIM}[{_ts()}] {RESET}{corrupt_text('indecisive')}?"
    ),
]


# Bomb Pot templates (for text overlay on GIF)
BOMB_POT_TEMPLATES = [
    lambda pool, contributors: (
        f"{RED} BOMB POT DETONATED{RESET}\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"{DIM}Type:{RESET} MANDATORY CONTRIBUTION\n"
        f"{DIM}Pool size:{RESET} {RED}{pool}{RESET} JC\n"
        f"{DIM}Victims:{RESET} {contributors}\n"
        f"{DIM}{'=' * 36}{RESET}\n"
        f"\n"
        f"{DIM}[{_ts()}] Consent:{RESET} {RED}NOT REQUIRED{RESET}\n"
        f"{DIM}[{_ts()}] Escape:{RESET} {RED}IMPOSSIBLE{RESET}\n"
        f"\n"
        f"{DIM}MANDATORY CONTRIBUTION.{RESET}\n"
        f"{DIM}NO ESCAPE.{RESET}"
    ),
]


# Lobby Join templates (Layer 1) - Player joins lobby
LOBBY_JOIN_TEMPLATES = [
    lambda name, position: f"{DIM}[JOPA-T] Queue position: #{position}. Client: {name}. Estimated time to regret: immediate.{RESET}",
    lambda name, position: f"{DIM}[{_ts()}] QUEUE_JOIN | {name} enters the arena. Position {position}. The countdown begins.{RESET}",
    lambda name, position: f"{DIM}[SYS] Another joins the queue. {name} at #{position}. The system {corrupt_text('welcomes')} you.{RESET}",
    lambda name, position: f"{DIM}[{_ts()}] {name} has entered matchmaking. Position: {position}. Time until commitment: unknown.{RESET}",
    lambda name, position: f"{DIM}[JOPA-T] {name} is queuing. Slot #{position}. The wait is part of the {corrupt_text('experience')}.{RESET}",
    lambda name, position: f"{DIM}[SYS] Queue updated. Client {name} added. Position {position}. No backing out now.{RESET}",
]


# Rivalry Detected templates (Layer 2) - 70%+ winrate imbalance
def rivalry_detected_box(player1: str, player2: str, games: int, winrate: float) -> str:
    """Layer 2 ASCII art for rivalry detection."""
    dominant = player1 if winrate > 50 else player2
    victim = player2 if winrate > 50 else player1
    actual_rate = winrate if winrate > 50 else 100 - winrate
    lines = [
        f"{RED} RIVALRY DETECTED{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}Player 1:{RESET} {player1}",
        f"{DIM}Player 2:{RESET} {player2}",
        f"{DIM}Games together:{RESET} {games}",
        f"{DIM}{'=' * 36}{RESET}",
        f"",
        f"{DIM}[{_ts()}] Pattern:{RESET} {RED}ONE-SIDED{RESET}",
        f"{DIM}[{_ts()}] Dominant:{RESET} {GREEN}{dominant}{RESET}",
        f"{DIM}[{_ts()}] Victim:{RESET} {RED}{victim}{RESET}",
        f"{DIM}[{_ts()}] Win rate:{RESET} {actual_rate:.0f}%",
        f"",
        f"{DIM}Status: DOCUMENTED{RESET}",
        f"{DIM}The system sees all patterns.{RESET}",
    ]
    return "\n".join(lines)


# Games Milestone templates (Layer 2/3)
def games_milestone_box(name: str, games: int) -> str:
    """Layer 2 ASCII art for games milestone."""
    tier = "BRONZE" if games == 10 else "SILVER" if games == 50 else "GOLD" if games == 100 else "DIAMOND" if games == 200 else "LEGENDARY"
    tier_color = DIM if games < 50 else YELLOW if games < 100 else GREEN if games < 200 else CYAN if games < 500 else RED
    lines = [
        f"{tier_color} GAMES MILESTONE{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}Subject:{RESET} {name}",
        f"{DIM}Games completed:{RESET} {tier_color}{games}{RESET}",
        f"{DIM}Classification:{RESET} {tier_color}{tier}{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"",
        f"{DIM}[{_ts()}] Milestone achieved.{RESET}",
        f"{DIM}[{_ts()}] Status: NOTED{RESET}",
        f"",
        f"{DIM}You cannot leave.{RESET}",
        f"{DIM}None of them ever leave.{RESET}",
    ]
    return "\n".join(lines)


# Win Streak Record templates (Layer 2/3)
def win_streak_record_box(name: str, streak: int) -> str:
    """Layer 2/3 ASCII art for personal best win streak."""
    lines = [
        f"{GREEN} ANOMALY DETECTED{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}Subject:{RESET} {name}",
        f"{DIM}Pattern:{RESET} {GREEN}WIN x{streak}{RESET}",
        f"{DIM}Status:{RESET} {YELLOW}UNPRECEDENTED{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"",
        f"{DIM}[{_ts()}] PERSONAL_RECORD_BROKEN{RESET}",
        f"{DIM}[{_ts()}] New streak: {streak}{RESET}",
        f"{DIM}[{_ts()}] Classification:{RESET} {GREEN}HOT{RESET}",
        f"",
        f"{DIM}The algorithm adjusts.{RESET}",
        f"{DIM}The system takes {RESET}{corrupt_text('notice')}{DIM}.{RESET}",
    ]
    return "\n".join(lines)


# First Leverage Bet templates (Layer 1, one-time)
FIRST_LEVERAGE_TEMPLATES = [
    lambda name, leverage: f"{YELLOW}[JOPA-T] MARGIN_ACCOUNT_OPENED{RESET}\n{DIM}Client:{RESET} {name}\n{DIM}First leverage:{RESET} {leverage}x\n{DIM}Status:{RESET} {RED}WELCOME TO THE DANGER ZONE{RESET}",
    lambda name, leverage: f"{YELLOW}[{_ts()}] FIRST_LEVERAGE_BET{RESET}\n{DIM}Subject: {name} | Multiplier: {leverage}x{RESET}\n{DIM}The slippery slope begins.{RESET}",
    lambda name, leverage: f"{YELLOW}[SYS] Leverage unlocked for {name}.{RESET}\n{DIM}{leverage}x is just the beginning.{RESET}\n{DIM}The system has seen this {RESET}{corrupt_text('trajectory')}{DIM}.{RESET}",
    lambda name, leverage: f"{DIM}[JOPA-T] {name} discovers leverage.{RESET}\n{YELLOW}Multiplier: {leverage}x{RESET}\n{DIM}There is no going back now.{RESET}",
]


# 100 Bets Milestone templates (Layer 2, one-time)
def bets_milestone_box(name: str, total_bets: int) -> str:
    """Layer 2 ASCII art for 100 bets milestone."""
    lines = [
        f"{YELLOW} BETTING CENTENNIAL{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}Subject:{RESET} {name}",
        f"{DIM}Total wagers:{RESET} {YELLOW}{total_bets}{RESET}",
        f"{DIM}Classification:{RESET} {RED}COMMITTED{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"",
        f"{DIM}[{_ts()}] Milestone: {total_bets} bets{RESET}",
        f"{DIM}[{_ts()}] Status:{RESET} {RED}TOO DEEP TO STOP{RESET}",
        f"",
        f"{DIM}One hundred bets.{RESET}",
        f"{DIM}One hundred chances to {RESET}{corrupt_text('reconsider')}{DIM}.{RESET}",
        f"{DIM}Zero taken.{RESET}",
    ]
    return "\n".join(lines)


# Simultaneous Events templates (Layer 2)
def simultaneous_events_box(event_count: int, events: list[str]) -> str:
    """Layer 2 ASCII art for multiple simultaneous events."""
    lines = [
        f"{YELLOW} SYSTEM OVERLOAD{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}Concurrent events:{RESET} {YELLOW}{event_count}{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"",
    ]
    for event in events[:4]:  # Max 4 events shown
        lines.append(f"{DIM}[{_ts()}] {event}{RESET}")
    lines.extend([
        f"",
        f"{DIM}Processing... Status:{RESET} {YELLOW}OVERWHELMED{RESET}",
        f"{DIM}({RESET}{corrupt_text('not really')}{DIM}){RESET}",
    ])
    return "\n".join(lines)


# Captain Symmetry templates (Layer 1)
CAPTAIN_SYMMETRY_TEMPLATES = [
    lambda c1, c2, diff: f"{DIM}[JOPA-T] MIRROR_MATCH detected. Captains within {diff} rating. The algorithm cannot predict this one.{RESET}",
    lambda c1, c2, diff: f"{DIM}[{_ts()}] CAPTAIN_SYMMETRY | {c1} vs {c2}. Delta: {diff}. Outcome: {corrupt_text('uncertain')}.{RESET}",
    lambda c1, c2, diff: f"{DIM}[SYS] Two captains. {diff} points apart. The system finds this... {corrupt_text('interesting')}.{RESET}",
    lambda c1, c2, diff: f"{DIM}[JOPA-T] Symmetric matchup: {c1} ({diff} from {c2}). Pure {corrupt_text('chaos')}.{RESET}",
    lambda c1, c2, diff: f"{DIM}[{_ts()}] PARITY_DETECTED | Captain delta: {diff}. Coin flip territory.{RESET}",
]


# Unanimous Wrong templates (Layer 3 GIF)
def unanimous_wrong_box(consensus_pct: float, winning_side: str, loser_count: int) -> str:
    """Layer 3 ASCII art for 90%+ consensus prediction that loses."""
    lines = [
        f"{RED} CONSENSUS COLLAPSE{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"{DIM}Consensus:{RESET} {RED}{consensus_pct:.0f}%{RESET} predicted wrong",
        f"{DIM}Actual winner:{RESET} {GREEN}{winning_side}{RESET}",
        f"{DIM}Losers:{RESET} {RED}{loser_count}{RESET}",
        f"{DIM}{'=' * 36}{RESET}",
        f"",
        f"{DIM}[{_ts()}] {RESET}{RED}MARKET_FAILURE{RESET}",
        f"{DIM}[{_ts()}] The crowd was confident.{RESET}",
        f"{DIM}[{_ts()}] The crowd was {RESET}{RED}wrong{RESET}{DIM}.{RESET}",
        f"",
        f"{DIM}As usual.{RESET}",
    ]
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Render helpers for new events
# ---------------------------------------------------------------------------

def render_all_in_bet(name: str, amount: int, percentage: float) -> str:
    """Render a Layer 2 all-in bet detection."""
    template = random.choice(ALL_IN_BET_TEMPLATES)
    return ansi_block(template(name, amount, percentage))


def render_last_second_bet(name: str, seconds_left: int) -> str:
    """Render a Layer 2 last-second bet detection."""
    template = random.choice(LAST_SECOND_BET_TEMPLATES)
    return ansi_block(template(name, seconds_left))


def render_bomb_pot(pool: int, contributors: int) -> str:
    """Render bomb pot template."""
    template = random.choice(BOMB_POT_TEMPLATES)
    return ansi_block(template(pool, contributors))


def render_lobby_join(name: str, position: int) -> str:
    """Render a Layer 1 lobby join one-liner."""
    template = random.choice(LOBBY_JOIN_TEMPLATES)
    return ansi_block(template(name, position))


def render_rivalry_detected(player1: str, player2: str, games: int, winrate: float) -> str:
    """Render a Layer 2 rivalry detection."""
    return ansi_block(rivalry_detected_box(player1, player2, games, winrate))


def render_games_milestone(name: str, games: int) -> str:
    """Render a Layer 2 games milestone."""
    return ansi_block(games_milestone_box(name, games))


def render_win_streak_record(name: str, streak: int) -> str:
    """Render a Layer 2/3 win streak record."""
    return ansi_block(win_streak_record_box(name, streak))


def render_first_leverage(name: str, leverage: int) -> str:
    """Render a Layer 1 first leverage bet."""
    template = random.choice(FIRST_LEVERAGE_TEMPLATES)
    return ansi_block(template(name, leverage))


def render_bets_milestone(name: str, total_bets: int) -> str:
    """Render a Layer 2 bets milestone."""
    return ansi_block(bets_milestone_box(name, total_bets))


def render_simultaneous_events(event_count: int, events: list[str]) -> str:
    """Render a Layer 2 simultaneous events box."""
    return ansi_block(simultaneous_events_box(event_count, events))


def render_captain_symmetry(captain1: str, captain2: str, diff: int) -> str:
    """Render a Layer 1 captain symmetry one-liner."""
    template = random.choice(CAPTAIN_SYMMETRY_TEMPLATES)
    return ansi_block(template(captain1, captain2, diff))


def render_unanimous_wrong(consensus_pct: float, winning_side: str, loser_count: int) -> str:
    """Render a Layer 3 unanimous wrong box."""
    return ansi_block(unanimous_wrong_box(consensus_pct, winning_side, loser_count))
