"""Repo-level tests for the atomic Double or Nothing cooldown claim."""

from tests.conftest import TEST_GUILD_ID

COOLDOWN = 30 * 86400


def _add_player(player_repository, discord_id: int) -> None:
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"don_{discord_id}",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
    )


def test_second_claim_within_cooldown_is_rejected(player_repository):
    _add_player(player_repository, 501)
    now = 1_800_000_000

    assert player_repository.try_claim_double_or_nothing(501, TEST_GUILD_ID, now, COOLDOWN) is True
    # A concurrent/second claim seconds later must lose the race.
    assert (
        player_repository.try_claim_double_or_nothing(501, TEST_GUILD_ID, now + 5, COOLDOWN)
        is False
    )
    # The winning claim recorded the timestamp.
    assert player_repository.get_last_double_or_nothing(501, TEST_GUILD_ID) == now


def test_claim_succeeds_again_after_cooldown_elapses(player_repository):
    _add_player(player_repository, 502)
    now = 1_800_000_000

    assert player_repository.try_claim_double_or_nothing(502, TEST_GUILD_ID, now, COOLDOWN) is True
    later = now + COOLDOWN + 1
    assert player_repository.try_claim_double_or_nothing(502, TEST_GUILD_ID, later, COOLDOWN) is True
    assert player_repository.get_last_double_or_nothing(502, TEST_GUILD_ID) == later


def test_claim_requires_player_row(player_repository):
    assert (
        player_repository.try_claim_double_or_nothing(999, TEST_GUILD_ID, 1_800_000_000, COOLDOWN)
        is False
    )
