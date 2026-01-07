"""
Tests for GuildConfigRepository.
"""

from repositories.guild_config_repository import GuildConfigRepository


def test_get_defaults_when_missing(repo_db_path):
    repo = GuildConfigRepository(repo_db_path)

    assert repo.get_config(123) is None
    assert repo.get_league_id(123) is None
    assert repo.get_auto_enrich(123) is True


def test_set_and_get_league_id(repo_db_path):
    repo = GuildConfigRepository(repo_db_path)

    repo.set_league_id(123, 777)

    assert repo.get_league_id(123) == 777
    config = repo.get_config(123)
    assert config is not None
    assert config["guild_id"] == 123
    assert config["league_id"] == 777


def test_auto_enrich_toggle_preserves_league_id(repo_db_path):
    repo = GuildConfigRepository(repo_db_path)

    repo.set_league_id(55, 999)
    repo.set_auto_enrich(55, False)

    assert repo.get_auto_enrich(55) is False
    config = repo.get_config(55)
    assert config["league_id"] == 999

    repo.set_auto_enrich(55, True)
    assert repo.get_auto_enrich(55) is True
