"""Blood Pact cap accounting when the minigame JC scale rescales transfers.

The skim is reserved against the pact's 150-JC cap BEFORE the economy scale is
applied to the transfer. With ``MINIGAME_JC_DELTA_SCALE > 1`` the coins actually
moved used to exceed what was counted against the cap, letting a pact skim more
real JC than its cap. The fix reconciles both directions: extra capacity is
claimed for the scaled-up portion, and the transfer is clamped to the cap's
remaining headroom when none is left.
"""

import pytest

import config
from repositories.buff_repository import BuffRepository
from repositories.player_repository import PlayerRepository
from services.buff_service import BuffService
from tests.conftest import TEST_GUILD_ID

SKIMMER = 611
TARGET = 622


@pytest.fixture
def buff_repo(repo_db_path):
    return BuffRepository(repo_db_path)


@pytest.fixture
def buff_service(buff_repo):
    return BuffService(buff_repo)


@pytest.fixture
def player_repo(repo_db_path):
    repo = PlayerRepository(repo_db_path)
    repo.add(discord_id=SKIMMER, discord_username="Skimmer", guild_id=TEST_GUILD_ID)
    repo.add(discord_id=TARGET, discord_username="Target", guild_id=TEST_GUILD_ID)
    repo.update_balance(SKIMMER, TEST_GUILD_ID, 0)
    repo.update_balance(TARGET, TEST_GUILD_ID, 500)
    return repo


def test_scaled_skim_counts_extra_against_cap(
    buff_service, player_repo, monkeypatch
):
    """With headroom left, the scaled-up transfer reserves the extra capacity:
    coins moved == capacity consumed (conservation between the two players)."""
    monkeypatch.setattr(config, "MINIGAME_JC_DELTA_SCALE", 2.0)
    buff_service.grant_blood_pact(SKIMMER, TEST_GUILD_ID, TARGET)

    # earning 100 -> reserve int(100 * 0.25) = 25 -> scaled to 50; the extra
    # 25 fits under the 150 cap, so the full 50 moves and is counted.
    skimmed = buff_service.apply_blood_pact_skim(
        TARGET, TEST_GUILD_ID, 100, player_repo
    )

    assert skimmed == 50
    pact = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    assert pact["data"]["skimmed_total"] == 50
    assert player_repo.get_balance(TARGET, TEST_GUILD_ID) == 450
    assert player_repo.get_balance(SKIMMER, TEST_GUILD_ID) == 50


def test_scaled_skim_clamps_transfer_at_cap(
    buff_service, buff_repo, player_repo, monkeypatch
):
    """With no headroom for the scaled portion, the transfer is clamped so the
    pact can never move more JC than its cap accounts for."""
    monkeypatch.setattr(config, "MINIGAME_JC_DELTA_SCALE", 2.0)
    buff_id = buff_service.grant_blood_pact(SKIMMER, TEST_GUILD_ID, TARGET)
    pact = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    buff_service.record_blood_pact_skim(buff_id, pact["data"], 100)

    # earning 400 -> reserve min(150-100, 100) = 50 -> scaled to 100, but only
    # 0 extra headroom remains after the reservation: transfer clamps to 50.
    skimmed = buff_service.apply_blood_pact_skim(
        TARGET, TEST_GUILD_ID, 400, player_repo
    )

    assert skimmed == 50
    refreshed = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    assert refreshed["data"]["skimmed_total"] == 150
    assert player_repo.get_balance(TARGET, TEST_GUILD_ID) == 450
    assert player_repo.get_balance(SKIMMER, TEST_GUILD_ID) == 50

    # Cap exhausted: further earnings skim nothing.
    assert (
        buff_service.apply_blood_pact_skim(TARGET, TEST_GUILD_ID, 400, player_repo)
        == 0
    )
    assert player_repo.get_balance(TARGET, TEST_GUILD_ID) == 450


def test_partial_headroom_grants_partial_extra(
    buff_service, buff_repo, player_repo, monkeypatch
):
    """Scaled portion partially fits: transfer = reservation + granted extra."""
    monkeypatch.setattr(config, "MINIGAME_JC_DELTA_SCALE", 2.0)
    buff_id = buff_service.grant_blood_pact(SKIMMER, TEST_GUILD_ID, TARGET)
    pact = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    buff_service.record_blood_pact_skim(buff_id, pact["data"], 60)

    # earning 200 -> reserve int(200 * 0.25) = 50 (headroom 90) -> scaled to
    # 100, extra needed 50 but only 40 headroom remains: transfer 90.
    skimmed = buff_service.apply_blood_pact_skim(
        TARGET, TEST_GUILD_ID, 200, player_repo
    )

    assert skimmed == 90
    refreshed = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    assert refreshed["data"]["skimmed_total"] == 150
    assert player_repo.get_balance(TARGET, TEST_GUILD_ID) == 410
    assert player_repo.get_balance(SKIMMER, TEST_GUILD_ID) == 90


def test_unscaled_skim_accounting_unchanged(buff_service, player_repo):
    """Default 1.0 scale: reservation == transfer == counted capacity."""
    assert config.MINIGAME_JC_DELTA_SCALE == 1.0
    buff_service.grant_blood_pact(SKIMMER, TEST_GUILD_ID, TARGET)

    skimmed = buff_service.apply_blood_pact_skim(
        TARGET, TEST_GUILD_ID, 100, player_repo
    )

    assert skimmed == 25
    pact = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    assert pact["data"]["skimmed_total"] == 25
    assert player_repo.get_balance(TARGET, TEST_GUILD_ID) == 475
    assert player_repo.get_balance(SKIMMER, TEST_GUILD_ID) == 25
