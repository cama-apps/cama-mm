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


def test_live_monetary_trends_use_nearest_daily_snapshot_anchors(repo_db_path):
    repo = EconomyEventRepository(repo_db_path)
    now = _local_timestamp(2026, 7, 18, 14)
    sheet = {
        "player_wallets": 0,
        "positive_wallets": 0,
        "visible_debt": 0,
        "player_count": 0,
        "average_wallet": 0,
        "reserve_available": 0,
        "reserve_locked": 0,
        "reserve_next_match_pot": 0,
        "prediction_open_cash": 0,
        "wager_escrow": 0,
        "monetary_stock": 0,
    }
    for days_ago, stock in ((1, 1_050), (3, 1_000), (7, 900)):
        captured_at = now - days_ago * 86400
        repo.save_snapshot(
            TEST_GUILD_ID,
            datetime.fromtimestamp(captured_at, tz=UTC).date().isoformat(),
            {**sheet, "monetary_stock": stock},
            captured_at=captured_at,
        )

    trends = repo.get_monetary_trends(
        TEST_GUILD_ID,
        current_stock=1_100,
        now=now,
    )

    assert trends["1d"]["change_jc"] == 50
    assert trends["1d"]["period_rate"] == pytest.approx(1_100 / 1_050 - 1)
    assert trends["3d"]["elapsed_days"] == 3
    assert trends["3d"]["average_daily_change_jc"] == pytest.approx(100 / 3)
    assert trends["7d"]["change_jc"] == 200
    assert trends["7d"]["period_rate"] == pytest.approx(1_100 / 900 - 1)


def test_monetary_trends_report_missing_windows_without_fake_rates(repo_db_path):
    repo = EconomyEventRepository(repo_db_path)

    trends = repo.get_monetary_trends(
        TEST_GUILD_ID,
        current_stock=1_000,
        now=_local_timestamp(2026, 7, 18, 14),
    )

    assert trends == {"1d": None, "3d": None, "7d": None}


def test_flow_indicators_measure_rolling_turnover_and_participation(repo_db_path):
    repo = EconomyEventRepository(repo_db_path)
    now = int(time.time())
    rows = (
        ("player", 1, 100, "dig", now - 86400),
        ("player", 2, -40, "gamba", now - 2 * 86400),
        ("nonprofit", TEST_GUILD_ID, 60, "tax_fine", now - 5 * 86400),
        ("player", 3, 999, "ledger_backfill", now - 86400),
    )
    with sqlite3.connect(repo_db_path) as conn:
        conn.executemany(
            """
            INSERT INTO economy_ledger_entries (
                guild_id, account_type, account_id, delta,
                balance_before, balance_after, source, created_at
            ) VALUES (?, ?, ?, ?, 0, ?, ?, ?)
            """,
            (
                (
                    TEST_GUILD_ID,
                    account_type,
                    account_id,
                    delta,
                    delta,
                    source,
                    created_at,
                )
                for account_type, account_id, delta, source, created_at in rows
            ),
        )

    flows = repo.get_flow_indicators(
        TEST_GUILD_ID,
        current_stock=1_000,
        player_count=4,
        now=now,
    )

    assert flows["3d"]["net_flow_jc"] == 60
    assert flows["3d"]["gross_flow_jc"] == 140
    assert flows["3d"]["active_players"] == 2
    assert flows["3d"]["active_player_rate"] == 0.5
    assert flows["3d"]["daily_turnover_rate"] == pytest.approx(140 / 3 / 1_000)
    assert flows["7d"]["net_flow_jc"] == 120
    assert flows["7d"]["gross_flow_jc"] == 200


def test_distribution_indicators_report_concentration(repo_db_path):
    players = PlayerRepository(repo_db_path)
    for discord_id, balance in enumerate((10, 20, 30, 40, 100), start=1):
        players.add(discord_id, f"player-{discord_id}", TEST_GUILD_ID)
        players.update_balance(discord_id, TEST_GUILD_ID, balance)
    repo = EconomyEventRepository(repo_db_path)

    distribution = repo.get_distribution_indicators(TEST_GUILD_ID)

    assert distribution["positive_player_count"] == 5
    assert distribution["median_positive_wallet"] == 30
    assert distribution["top_decile_count"] == 1
    assert distribution["top_decile_share"] == 0.5
    assert distribution["gini"] == pytest.approx(0.4)


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
    service = EconomyEventService(repo, enabled=False)

    event, created = service.ensure_daily_event(TEST_GUILD_ID)

    assert event is None
    assert created is False
    assert service.get_effects(TEST_GUILD_ID).reward_multiplier == 1.0


def test_policy_targets_two_percent_without_a_recovery_override(repo_db_path):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True)

    policy = service.ensure_policy(TEST_GUILD_ID, now=1_000)

    assert policy["mode"] == "normal"
    assert policy["target_annual_rate"] == pytest.approx(0.02)


def test_active_level_three_deflationary_edict_suspends_reserve_voting(
    repo_db_path,
):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True, trigger_hour_local=10)
    now = _local_timestamp(2026, 7, 18, 12)
    payload = _event_payload("2026-07-18", 2_500)
    payload.update(
        severity=3,
        starts_at=_local_timestamp(2026, 7, 18, 10),
        ends_at=_local_timestamp(2026, 7, 19, 10),
    )
    event, _ = repo.activate_event_atomic(TEST_GUILD_ID, payload)

    restriction = service.get_reserve_voting_restriction(
        TEST_GUILD_ID,
        now=now,
    )

    assert restriction == {
        "event_id": event["event_id"],
        "name": "Ravage",
        "severity": 3,
    }


@pytest.mark.parametrize(
    ("direction", "severity", "active", "enabled"),
    (
        ("deflationary", 2, True, True),
        ("boon", 5, True, True),
        ("deflationary", 5, False, True),
        ("deflationary", 5, True, False),
    ),
)
def test_nonrestrictive_or_inactive_edict_keeps_reserve_voting_open(
    repo_db_path,
    direction,
    severity,
    active,
    enabled,
):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=enabled, trigger_hour_local=10)
    now = _local_timestamp(2026, 7, 18, 12)
    payload = _event_payload("2026-07-18", 2_500)
    payload.update(
        direction=direction,
        severity=severity,
        starts_at=_local_timestamp(2026, 7, 18, 10),
        ends_at=(
            _local_timestamp(2026, 7, 19, 10)
            if active
            else now - 1
        ),
    )
    repo.activate_event_atomic(TEST_GUILD_ID, payload)

    assert (
        service.get_reserve_voting_restriction(TEST_GUILD_ID, now=now)
        is None
    )


@pytest.mark.parametrize(
    ("weekly_deviation", "expected_direction", "expected_severity"),
    (
        (-0.800001, "boon", 5),
        (-0.80, "boon", 4),
        (-0.40, "boon", 3),
        (-0.20, "boon", 2),
        (-0.10, "boon", 1),
        (-0.05, "neutral", 1),
        (0.00, "neutral", 1),
        (0.05, "neutral", 1),
        (0.050001, "deflationary", 1),
        (0.10, "deflationary", 1),
        (0.100001, "deflationary", 2),
        (0.20, "deflationary", 2),
        (0.200001, "deflationary", 3),
        (0.40, "deflationary", 3),
        (0.400001, "deflationary", 4),
        (0.80, "deflationary", 4),
        (0.800001, "deflationary", 5),
    ),
)
def test_weekly_deviation_selects_broad_directional_band(
    weekly_deviation, expected_direction, expected_severity
):
    assert (
        EconomyEventService._band_for_weekly_deviation(weekly_deviation)
        == (expected_direction, expected_severity)
    )


@pytest.mark.parametrize(
    ("severity", "expected_reward_multiplier"),
    (
        (1, 0.925),
        (2, 0.85),
        (3, 0.775),
        (4, 0.6625),
        (5, 0.55),
    ),
)
def test_five_levels_add_granularity_without_exceeding_old_maximum(
    repo_db_path,
    severity,
    expected_reward_multiplier,
):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True)
    doom = next(template for template in _EVENT_CATALOG if template.name == "Doom")
    balance_sheet = repo.capture_balance_sheet(TEST_GUILD_ID)

    effects = service._effects_for(doom, severity, balance_sheet)

    assert effects["reward_multiplier"] == expected_reward_multiplier


@pytest.mark.parametrize(
    ("severity", "expected_disabled"),
    ((2, False), (3, True), (5, True)),
)
def test_only_severe_deflationary_edicts_disable_reserve_voting(
    repo_db_path,
    severity,
    expected_disabled,
):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True)
    doom = next(template for template in _EVENT_CATALOG if template.name == "Doom")

    effects = service._effects_for(
        doom,
        severity,
        repo.capture_balance_sheet(TEST_GUILD_ID),
    )

    assert effects["reserve_voting_disabled"] is expected_disabled


def test_controller_scores_only_the_weekly_band_severity(repo_db_path, monkeypatch):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True, lookback_days=7)
    now = _local_timestamp(2026, 7, 18, 10)
    old_sheet = repo.capture_balance_sheet(TEST_GUILD_ID)
    repo.save_snapshot(
        TEST_GUILD_ID,
        "2026-07-11",
        {**old_sheet, "monetary_stock": 1_000},
        captured_at=now - 7 * 86400,
    )
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
    assert event["severity"] == 5
    assert set(seen_severities) == {5}


def test_controller_uses_a_neutral_edict_until_seven_day_history_exists(
    repo_db_path,
    monkeypatch,
):
    repo, _ = _seed_economy(repo_db_path)
    service = EconomyEventService(repo, enabled=True, lookback_days=7)
    now = _local_timestamp(2026, 7, 18, 10)
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

    event, created = service.ensure_daily_event(
        TEST_GUILD_ID,
        now=now,
        event_date="2026-07-18",
    )

    assert created is True
    assert event["direction"] == "neutral"
    assert event["severity"] == 1


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
        observed_daily_flow=100,
    )

    assert "depth" not in announcement
    assert "resolution **-1.0%**" in announcement
    assert "spread **+2 ticks**" in announcement
    assert "Observed 7-day stock movement: **+100 JC/day**." in announcement
    assert "forecast" not in announcement.lower()


def test_format_event_supports_level_five():
    service = EconomyEventService(repository=None, enabled=True)
    event = _event_payload("2026-07-29", 1000)
    event["severity"] = 5

    title, _ = service.format_event(event)

    assert title == "Ravage — Level V"


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


def test_event_has_one_initial_and_one_twelve_hour_reminder_slot(repo_db_path):
    repo, _ = _seed_economy(repo_db_path)
    date = get_game_date()
    before = repo.capture_balance_sheet(TEST_GUILD_ID)
    payload = _event_payload(date, int(before["monetary_stock"]))
    payload["starts_at"] = 1_000
    payload["ends_at"] = 87_400
    event, _ = repo.activate_event_atomic(TEST_GUILD_ID, payload)
    service = EconomyEventService(repo, enabled=True)

    assert service.pending_announcement_slot(event, now=1_000) == "initial"

    service.mark_event_announced(TEST_GUILD_ID, event["event_id"], now=1_001)
    event = repo.get_event_for_date(TEST_GUILD_ID, date)
    assert service.pending_announcement_slot(event, now=44_199) is None
    assert service.pending_announcement_slot(event, now=44_200) == "reminder"

    service.mark_event_reminder_announced(
        TEST_GUILD_ID,
        event["event_id"],
        now=44_201,
    )
    event = repo.get_event_for_date(TEST_GUILD_ID, date)
    assert event["reminder_announced_at"] == 44_201
    assert service.pending_announcement_slot(event, now=44_202) is None
