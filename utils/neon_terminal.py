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
