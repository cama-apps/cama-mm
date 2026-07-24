"""Controller, balance-sheet, and atomic daily-event coverage."""

from __future__ import annotations

import sqlite3
import time
from datetime import UTC, datetime
from zoneinfo import ZoneInfo

import pytest

from domain.models.economy_event import EconomyEventEffects
from repositories.economy_event_repository import EconomyEventRepository
from repositories.loan_repository import LoanRepository
from repositories.player_repository import PlayerRepository
from services.economy_event_service import _EVENT_CATALOG, EconomyEventService
from services.trivia_data import get_ability_icon_url_by_name
from tests.conftest import TEST_GUILD_ID
from utils.game_date import get_game_date

PACIFIC = ZoneInfo("America/Los_Angeles")


def test_every_economy_event_has_dotabase_spell_art():
    missing = [
        template.name
        for template in _EVENT_CATALOG
        if not get_ability_icon_url_by_name(template.name)
    ]

    assert missing == []


def _local_timestamp(
    year: int,
    month: int,
    day: int,
    hour: int,
    minute: int = 0,
) -> int:
    return int(datetime(year, month, day, hour, minute, tzinfo=PACIFIC).timestamp())


def _seed_economy(db_path: str) -> tuple[EconomyEventRepository, PlayerRepository]:
    players = PlayerRepository(db_path)
    players.add(1, "one", TEST_GUILD_ID, initial_mmr=3000)
    players.add(2, "two", TEST_GUILD_ID, initial_mmr=3000)
    players.update_balance(1, TEST_GUILD_ID, 1000)
    players.update_balance(2, TEST_GUILD_ID, 500)
    LoanRepository(db_path).add_to_nonprofit_fund(TEST_GUILD_ID, 1000)
    return EconomyEventRepository(db_path), players


def _event_payload(event_date: str, stock: int) -> dict:
    now = int(time.time())
    return {
        "event_date": event_date,
        "name": "Ravage",
        "hero": "Tidehunter",
        "direction": "deflationary",
        "severity": 2,
        "target_effect_jc": -100,
        "forecast_flow_jc": 90,
        "expected_effect_jc": -110,
        "monetary_stock_before": stock,
        "effects": {
            "reward_multiplier": 0.8,
            "gamba_win_multiplier": 0.9,
            "gamba_loss_multiplier": 1.1,
            "bet_payout_multiplier": 0.97,
            "prediction_payout_multiplier": 0.99,
            "prediction_depth_multiplier": 0.7,
            "prediction_spread_ticks_delta": 2,
            "reserve_burn_jc": 100,
            "reserve_release_jc": 0,
            "wallet_burn_rate": 0.01,
        },
        "announcement": "A tidal shock hits the economy.",
        "starts_at": now,
        "ends_at": now + 86400,
        "created_at": now,
    }


def test_balance_sheet_counts_reserve_and_average(repo_db_path):
    repo, _ = _seed_economy(repo_db_path)

    sheet = repo.capture_balance_sheet(TEST_GUILD_ID)

    assert sheet["player_wallets"] == 1500
    assert sheet["positive_wallets"] == 1500
    assert sheet["player_count"] == 2
    assert sheet["average_wallet"] == 750
    assert sheet["reserve_available"] == 1000
    assert sheet["monetary_stock"] == 2500


def test_reward_volume_includes_trivia_and_generated_mana(repo_db_path):
    repo, players = _seed_economy(repo_db_path)
    for source, amount in (
        ("dig", 10),
        ("trivia", 20),
        ("player_trivia", 30),
        ("mana_reward", 40),
        ("manashop_buff", 50),
    ):
        players.add_balance(
            1,
            TEST_GUILD_ID,
            amount,
            source=source,
            related_type="test_reward",
            reason="surface-volume test",
        )

    volumes = repo.get_surface_daily_volumes(TEST_GUILD_ID, lookback_days=1)

    assert volumes["reward_credits"] == 150


def test_atomic_event_burns_once_and_records_ledger(repo_db_path):
    repo, players = _seed_economy(repo_db_path)
    date = get_game_date()
    before = repo.capture_balance_sheet(TEST_GUILD_ID)
    payload = _event_payload(date, int(before["monetary_stock"]))

    first, created = repo.activate_event_atomic(TEST_GUILD_ID, payload)
    second, created_again = repo.activate_event_atomic(TEST_GUILD_ID, payload)

    assert created is True
    assert created_again is False
    assert second["event_id"] == first["event_id"]
    assert first["effects"]["reserve_burn_jc"] == 100
    assert first["effects"]["wallet_burn_jc"] == 15
    assert first["direct_effect_jc"] == -115
    assert players.get_balance(1, TEST_GUILD_ID) == 990
    assert players.get_balance(2, TEST_GUILD_ID) == 495
    assert LoanRepository(repo_db_path).get_nonprofit_fund(TEST_GUILD_ID) == 900
    with sqlite3.connect(repo_db_path) as conn:
        rows = conn.execute(
            """
            SELECT account_type, SUM(delta) FROM economy_ledger_entries
            WHERE guild_id = ? AND source = 'economy_event'
            GROUP BY account_type ORDER BY account_type
            """,
            (TEST_GUILD_ID,),
        ).fetchall()
    assert rows == [("nonprofit", -100), ("player", -15)]


def test_daily_controller_is_idempotent_and_exposes_effects(
    repo_db_path, monkeypatch
):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(
        repo,
        enabled=True,
        recovery_mode=True,
        lookback_days=7,
        max_reserve_burn_pct=0.03,
        max_wallet_burn_pct=0.0025,
    )

    now = _local_timestamp(2026, 7, 18, 10)
    monkeypatch.setattr("services.economy_event_service.time.time", lambda: now)
    first, created = service.ensure_daily_event(TEST_GUILD_ID, now=now)
    second, created_again = service.ensure_daily_event(TEST_GUILD_ID, now=now)

    assert created is True
    assert created_again is False
    assert second["event_id"] == first["event_id"]
    assert first["direction"] in {"deflationary", "neutral", "boon"}
    effects = service.get_effects(TEST_GUILD_ID)
    assert effects.reward_multiplier >= 0
    assert 0.9 <= effects.prediction_payout_multiplier <= 1.1
    assert repo.get_latest_snapshot(TEST_GUILD_ID)["snapshot_date"] == "2026-07-18"


def test_disabled_service_returns_neutral_effects(repo_db_path):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=False, recovery_mode=False)

    event, created = service.ensure_daily_event(TEST_GUILD_ID)

    assert event is None
    assert created is False
    assert service.get_effects(TEST_GUILD_ID).reward_multiplier == 1.0


@pytest.mark.parametrize(
    ("required_effect", "deadband", "expected_severity"),
    (
        (0, 5, 1),
        (5, 5, 1),
        (25, 5, 1),
        (26, 5, 2),
        (75, 5, 2),
        (76, 5, 3),
        (-76, 5, 3),
    ),
)
def test_severity_follows_correction_magnitude(
    required_effect, deadband, expected_severity
):
    assert (
        EconomyEventService._severity_for_correction(required_effect, deadband)
        == expected_severity
    )


def test_controller_scores_only_the_flow_selected_severity(repo_db_path, monkeypatch):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(
        repo,
        enabled=True,
        recovery_mode=True,
        lookback_days=7,
    )
    now = _local_timestamp(2026, 7, 18, 10)
    monkeypatch.setattr(repo, "forecast_daily_flow", lambda *args, **kwargs: 1_000)
    monkeypatch.setattr(
        repo,
        "get_surface_daily_volumes",
        lambda *args, **kwargs: {
            "reward_credits": 0.0,
            "gamba_credits": 0.0,
            "gamba_debits": 0.0,
            "bet_payouts": 0.0,
            "prediction_payouts": 0.0,
        },
    )
    seen_severities: list[int] = []
    real_effects_for = service._effects_for

    def _recording_effects_for(template, severity, balance_sheet):
        seen_severities.append(severity)
        return real_effects_for(template, severity, balance_sheet)

    monkeypatch.setattr(service, "_effects_for", _recording_effects_for)

    event, created = service.ensure_daily_event(
        TEST_GUILD_ID,
        now=now,
        event_date="2026-07-18",
    )

    assert created is True
    assert event["direction"] == "deflationary"
    assert event["severity"] == 3
    assert set(seen_severities) == {3}


def test_legacy_prediction_depth_effect_is_neutralized():
    effects = EconomyEventEffects.from_mapping(
        {"prediction_depth_multiplier": 0.16}
    )

    assert effects.prediction_depth_multiplier == 1.0


def test_event_announcement_omits_fixed_prediction_depth():
    template = next(event for event in _EVENT_CATALOG if event.name == "Ravage")
    effects = {
        "reward_multiplier": 1.0,
        "gamba_win_multiplier": 1.0,
        "gamba_loss_multiplier": 1.0,
        "bet_payout_multiplier": 1.0,
        "prediction_payout_multiplier": 0.99,
        "prediction_depth_multiplier": 1.0,
        "prediction_spread_ticks_delta": 2,
        "reserve_burn_jc": 0,
        "reserve_release_jc": 0,
        "wallet_burn_rate": 0.0,
    }

    announcement = EconomyEventService._announcement_text(
        template,
        severity=2,
        effects=effects,
        required_effect=-100,
        forecast=100,
    )

    assert "depth" not in announcement
    assert "resolution **-1.0%**" in announcement
    assert "spread **+2 ticks**" in announcement


def test_pre_trigger_missing_prior_card_stays_neutral(repo_db_path):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True, trigger_hour_local=10)
    now = _local_timestamp(2026, 7, 18, 9, 59)

    event, created = service.ensure_daily_event(TEST_GUILD_ID, now=now)

    assert event is None
    assert created is False
    assert repo.get_event_for_date(TEST_GUILD_ID, "2026-07-17") is None


def test_pre_trigger_returns_existing_prior_day_card(repo_db_path):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True, trigger_hour_local=10)
    prior = _event_payload("2026-07-17", 2500)
    prior["starts_at"], prior["ends_at"] = service._event_window("2026-07-17")
    stored, _ = repo.activate_event_atomic(TEST_GUILD_ID, prior)
    now = _local_timestamp(2026, 7, 18, 9, 59)

    event, created = service.ensure_daily_event(TEST_GUILD_ID, now=now)

    assert created is False
    assert event["event_id"] == stored["event_id"]
    assert event["event_date"] == "2026-07-17"


def test_trigger_boundary_creates_new_local_day_card(repo_db_path):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True, trigger_hour_local=10)
    now = _local_timestamp(2026, 7, 18, 10)

    event, created = service.ensure_daily_event(TEST_GUILD_ID, now=now)

    assert created is True
    assert event["event_date"] == "2026-07-18"
    assert event["starts_at"] == now


def test_get_effects_switches_event_dates_at_ten_am(repo_db_path, monkeypatch):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True, trigger_hour_local=10)
    prior = _event_payload("2026-07-17", 2500)
    prior["effects"]["reward_multiplier"] = 0.8
    prior["starts_at"], prior["ends_at"] = service._event_window("2026-07-17")
    repo.activate_event_atomic(TEST_GUILD_ID, prior)
    current = _event_payload("2026-07-18", 2385)
    current["effects"]["reward_multiplier"] = 0.6
    current["starts_at"], current["ends_at"] = service._event_window("2026-07-18")
    repo.activate_event_atomic(TEST_GUILD_ID, current)

    monkeypatch.setattr(
        "services.economy_event_service.time.time",
        lambda: _local_timestamp(2026, 7, 18, 9, 59),
    )
    assert service.get_effects(TEST_GUILD_ID).reward_multiplier == 0.8

    monkeypatch.setattr(
        "services.economy_event_service.time.time",
        lambda: _local_timestamp(2026, 7, 18, 10),
    )
    assert service.get_effects(TEST_GUILD_ID).reward_multiplier == 0.6


def test_explicit_event_date_bypasses_pre_trigger_creation_guard(repo_db_path):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True, trigger_hour_local=10)
    now = _local_timestamp(2026, 7, 18, 9)

    event, created = service.ensure_daily_event(
        TEST_GUILD_ID,
        now=now,
        event_date="2030-01-15",
    )

    assert created is True
    assert event["event_date"] == "2030-01-15"
    start_local = datetime.fromtimestamp(event["starts_at"], tz=UTC).astimezone(
        PACIFIC
    )
    end_local = datetime.fromtimestamp(event["ends_at"], tz=UTC).astimezone(PACIFIC)
    assert (start_local.isoformat(), end_local.isoformat()) == (
        "2030-01-15T10:00:00-08:00",
        "2030-01-16T10:00:00-08:00",
    )


@pytest.mark.parametrize(
    ("event_date", "expected_duration"),
    (
        ("2026-03-07", 23 * 60 * 60),
        ("2026-10-31", 25 * 60 * 60),
    ),
)
def test_event_window_is_dst_aware(repo_db_path, event_date, expected_duration):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True, trigger_hour_local=10)

    starts_at, ends_at = service._event_window(event_date)

    assert ends_at - starts_at == expected_duration
    for timestamp in (starts_at, ends_at):
        assert datetime.fromtimestamp(timestamp, tz=UTC).astimezone(PACIFIC).hour == 10


@pytest.mark.parametrize(
    ("year", "month", "day", "expected_seconds"),
    (
        (2026, 3, 7, 23 * 60 * 60),
        (2026, 10, 31, 25 * 60 * 60),
    ),
)
def test_seconds_until_next_trigger_tracks_dst(
    repo_db_path, year, month, day, expected_seconds
):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True, trigger_hour_local=10)
    now = _local_timestamp(year, month, day, 10)

    assert service.seconds_until_next_trigger(now=now) == expected_seconds


def test_estimate_effect_treats_reserve_redistribution_as_supply_neutral():
    effects = {
        "reward_multiplier": 1.0,
        "gamba_win_multiplier": 1.0,
        "gamba_loss_multiplier": 1.0,
        "bet_payout_multiplier": 1.0,
        "prediction_payout_multiplier": 1.0,
        "reserve_burn_jc": 0,
        "reserve_release_jc": 250,
        "wallet_burn_rate": 0.0,
    }
    volumes = {
        "reward_credits": 0.0,
        "gamba_credits": 0.0,
        "gamba_debits": 0.0,
        "bet_payouts": 0.0,
        "prediction_payouts": 0.0,
    }
    balance_sheet = {"positive_wallets": 0}

    assert EconomyEventService._estimate_effect(effects, volumes, balance_sheet) == 0


def test_atomic_event_release_credits_players_without_changing_supply(repo_db_path):
    repo, players = _seed_economy(repo_db_path)
    date = get_game_date()
    before = repo.capture_balance_sheet(TEST_GUILD_ID)
    payload = _event_payload(date, int(before["monetary_stock"]))
    payload["effects"]["reserve_burn_jc"] = 0
    payload["effects"]["wallet_burn_rate"] = 0.0
    payload["effects"]["reserve_release_jc"] = 200

    event, created = repo.activate_event_atomic(TEST_GUILD_ID, payload)

    assert created is True
    assert event["effects"]["reserve_release_jc"] == 200
    assert event["direct_effect_jc"] == 0
    assert players.get_balance(1, TEST_GUILD_ID) == 1100
    assert players.get_balance(2, TEST_GUILD_ID) == 600
    assert LoanRepository(repo_db_path).get_nonprofit_fund(TEST_GUILD_ID) == 800


def test_mark_event_announced_stamps_once(repo_db_path):
    repo, _ = _seed_economy(repo_db_path)
    date = get_game_date()
    before = repo.capture_balance_sheet(TEST_GUILD_ID)
    event, _ = repo.activate_event_atomic(
        TEST_GUILD_ID, _event_payload(date, int(before["monetary_stock"]))
    )

    assert event["announced_at"] is None

    service = EconomyEventService(repo, enabled=True)
    service.mark_event_announced(TEST_GUILD_ID, event["event_id"], now=1111)
    assert repo.get_event_for_date(TEST_GUILD_ID, date)["announced_at"] == 1111

    # Retries keep the original announcement timestamp.
    service.mark_event_announced(TEST_GUILD_ID, event["event_id"], now=2222)
    assert repo.get_event_for_date(TEST_GUILD_ID, date)["announced_at"] == 1111
