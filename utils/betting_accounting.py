"""Pure accounting helpers shared by bet settlement and match reporting."""


def calculate_bet_jc_deltas(distributions: dict) -> dict[int, int]:
    """Return each bettor's full wager P/L from placement through settlement."""
    deltas: dict[int, int] = {}
    winners = distributions.get("winners", [])
    losers = distributions.get("losers", [])

    for entry in [*winners, *losers]:
        discord_id = int(entry["discord_id"])
        deltas.setdefault(discord_id, 0)
        if entry.get("refunded"):
            continue
        stake = int(entry.get("effective_bet", entry.get("amount", 0)))
        deltas[discord_id] -= stake

    for entry in winners:
        if not entry.get("refunded"):
            deltas[int(entry["discord_id"])] += int(entry.get("payout", 0))

    for deduction_key in (
        "bankruptcy_penalties",
        "vanity_taxes",
        "blood_pact_skims",
    ):
        for discord_id, amount in distributions.get(deduction_key, {}).items():
            discord_id = int(discord_id)
            deltas.setdefault(discord_id, 0)
            deltas[discord_id] -= int(amount)

    return deltas
