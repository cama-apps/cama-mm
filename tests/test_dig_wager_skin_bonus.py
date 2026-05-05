"""Silent wager-size hit bonus.

The fight loop adds a small bonus to player_hit that scales with wager size,
capped at +3% when the wager hits 500 JC. Free fights get nothing. The bonus
is silent — no UI surface and not exposed in the win_chance display.
"""

import pytest

from repositories.dig_repository import DigRepository
from services.dig_service import DigService


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


class TestWagerSkinBonus:
    def test_zero_wager_no_bonus(self, dig_service):
        assert dig_service._wager_skin_bonus(0) == 0.0

    def test_max_wager_hits_three_percent(self, dig_service):
        # 500 JC = full bonus
        assert dig_service._wager_skin_bonus(500) == pytest.approx(0.03)

    def test_wager_above_cap_does_not_exceed_three_percent(self, dig_service):
        # Going all-in past 500 doesn't keep ramping the bump.
        assert dig_service._wager_skin_bonus(2000) == pytest.approx(0.03)
        assert dig_service._wager_skin_bonus(50_000) == pytest.approx(0.03)

    def test_partial_wager_partial_bonus(self, dig_service):
        # 250 / 500 * 0.03 = 0.015
        assert dig_service._wager_skin_bonus(250) == pytest.approx(0.015)

    def test_low_balance_all_in_does_not_max(self, dig_service):
        # 200 JC wager (e.g. low-balance all-in) only gets 200/500 * 3% = 1.2%.
        # Flat denominator behavior is intentional — keeps the bonus an
        # actual reward for committing meaningful stakes.
        assert dig_service._wager_skin_bonus(200) == pytest.approx(0.012)

    def test_negative_wager_no_bonus(self, dig_service):
        # Defensive — the fight paths reject negative wagers earlier, but
        # the helper itself should never return negative.
        assert dig_service._wager_skin_bonus(-50) == 0.0
