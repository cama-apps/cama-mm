from types import SimpleNamespace

import commands.tax as tax_commands
from utils.embed_safety import EMBED_LIMITS, validate_embed


def _empty_prediction_exposure():
    return {
        "summary": {
            "cost_basis": 0,
            "expected_payout": 0,
            "ev": 0,
            "max_payout": 0,
        },
        "positions": [],
    }


def _ledger_row(
    idx: int,
    *,
    reason: str | None = None,
    source: str = "balance_update",
) -> dict:
    return {
        "ledger_id": idx,
        "guild_id": 123,
        "account_type": "player",
        "account_id": 10_000 + idx,
        "delta": idx + 1,
        "balance_before": idx * 10,
        "balance_after": idx * 10 + idx + 1,
        "source": source,
        "actor_id": 42,
        "related_type": "wheel_spin" if source == "gamba" else None,
        "related_id": "LIGHTNING_BOLT" if source == "gamba" else None,
        "reason": reason,
        "metadata": None,
        "created_at": 1_700_000_000 + idx,
    }


def _player_snapshot(recent_ledger: list[dict]) -> dict:
    return {
        "balance": 100,
        "visible_debt": 0,
        "loan_principal": 0,
        "loan_fee": 0,
        "loan_total": 0,
        "total_loans_taken": 0,
        "bankruptcy_count": 0,
        "penalty_games_remaining": 0,
        "dark_bargain_count": 0,
        "dark_bargain_due": 0,
        "effective_obligations": 0,
        "prediction_exposure": _empty_prediction_exposure(),
        "recent_ledger": recent_ledger,
    }


def test_tax_group_is_audit_only():
    names = {cmd.name for cmd in tax_commands.TaxCommands.tax.walk_commands()}

    assert names == {"audit", "player", "ledger"}


def test_tax_player_recent_ledger_splits_long_field():
    rows = [
        _ledger_row(
            idx,
            reason="gamba lightning bolt tax " + ("x" * 120),
            source="gamba",
        )
        for idx in range(12)
    ]
    user = SimpleNamespace(name="taxpayer", display_name="Taxpayer")

    embed = tax_commands._build_player_embed(
        user,
        _player_snapshot(rows),
        tax_service=object(),
    )

    assert validate_embed(embed) == []
    recent_fields = [
        field for field in embed.fields if field.name in {"Recent Ledger", "\u200b"}
    ]
    assert len(recent_fields) > 1
    assert all(len(field.value) <= EMBED_LIMITS["field_value"] for field in recent_fields)


def test_ledger_rows_prefer_descriptive_reason():
    text = tax_commands._format_ledger_rows(
        [
            _ledger_row(
                1,
                reason="gamba lightning bolt tax",
                source="gamba",
            )
        ]
    )

    assert "gamba lightning bolt tax" in text
    assert "via `balance_update`" not in text


def test_ledger_embed_truncates_description_to_discord_limit():
    rows = [
        _ledger_row(
            idx,
            reason="dig event credit " + ("y" * 240),
            source="dig",
        )
        for idx in range(25)
    ]

    embed = tax_commands._build_ledger_embed(rows, user=None)

    assert len(embed.description) <= EMBED_LIMITS["description"]
    assert validate_embed(embed) == []
