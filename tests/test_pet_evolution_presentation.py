"""Player-facing evolution status, collection, and art tests."""

from __future__ import annotations

import io
from unittest.mock import MagicMock

from PIL import Image

from commands.pet_helpers import embeds as pet_embeds
from domain.models.pet import Pet, PetMood, PetStage, PetStatus
from domain.pet_evolution import PetCalling, PetInstinct
from utils.pet_drawing import render_pet_card

T0 = 1_800_000_000
DAY = 86400


def make_pet(**overrides) -> Pet:
    values = {
        "pet_id": 7,
        "discord_id": 100,
        "guild_id": 12345,
        "name": "Blep",
        "species": "common_cama",
        "adopted_at": T0 - DAY,
        "hatched_at": T0,
        "adopt_fee": 20,
        "last_fed_at": T0,
        "hunger_at_last_fed": 100,
        "times_fed": 0,
        "feeds_today": 0,
        "feed_date": None,
        "week_consumed_jc": 0,
        "week_key": None,
        "prev_week_consumed_jc": 0,
        "prev_week_key": None,
        "pampered_until": None,
        "accessory": None,
        "dig_work_units": 0,
        "dig_work_at": T0,
        "aegis_used": 0,
        "hatch_announced_at": T0,
        "died_at": None,
        "death_cause": None,
        "death_announced_at": None,
        "evolution_started_at": T0,
        "evolution_due_at": T0 + 7 * DAY,
    }
    values.update(overrides)
    return Pet(**values)


def make_status(pet: Pet, **overrides) -> PetStatus:
    values = {
        "pet": pet,
        "hunger": 80,
        "stage": PetStage.BABY,
        "mood": PetMood.HAPPY,
        "age_seconds": 2 * DAY,
        "supplies": {},
        "evolution_hint": None,
    }
    values.update(overrides)
    return PetStatus(**values)


def test_upbringing_status_shows_only_a_narrative_hint(monkeypatch):
    pet = make_pet()
    status = make_status(pet, evolution_hint="delving_strong")
    monkeypatch.setattr(pet_embeds, "get_pet_card", MagicMock(return_value=None))

    embed, _file = pet_embeds.build_status_embed(
        status,
        20,
        T0 + 2 * DAY,
        owner_name="Owner",
        next_fee=20,
    )

    field = next(field for field in embed.fields if field.name == "Upbringing")
    assert "stone" in field.value.lower() or "deep" in field.value.lower()
    assert not any(character.isdigit() for character in field.value)


def test_evolved_status_uses_calling_title_profile_and_cached_art_key(monkeypatch):
    pet = make_pet(
        evolved_at=T0 + 7 * DAY,
        evolution_calling=PetCalling.PROSPECTOR,
        evolution_primary=PetInstinct.DELVING,
        evolution_secondary=PetInstinct.FORTUNE,
    )
    status = make_status(
        pet,
        stage=PetStage.ADULT,
        age_seconds=8 * DAY,
    )
    get_card = MagicMock(return_value=None)
    monkeypatch.setattr(pet_embeds, "get_pet_card", get_card)

    embed, _file = pet_embeds.build_status_embed(
        status,
        20,
        T0 + 8 * DAY,
        owner_name="Owner",
        next_fee=20,
    )

    assert "Prospector" in embed.title
    calling = next(field for field in embed.fields if field.name == "Calling")
    assert "fortune" in calling.value.lower()
    assert "delving" in calling.value.lower()
    get_card.assert_called_once_with(
        pet.species,
        "adult",
        pet.art_mood(T0 + 8 * DAY, 20),
        pet.pet_id,
        accessory=None,
        calling=PetCalling.PROSPECTOR,
        primary=PetInstinct.DELVING,
        secondary=PetInstinct.FORTUNE,
    )


def test_evolution_announcement_and_graveyard_preserve_calling_identity(
    monkeypatch,
):
    pet = make_pet(
        evolved_at=T0 + 7 * DAY,
        evolution_calling=PetCalling.ORACLE,
        evolution_primary=PetInstinct.FORTUNE,
        evolution_secondary=PetInstinct.WISDOM,
        died_at=T0 + 9 * DAY,
        death_cause="starvation",
    )
    monkeypatch.setattr(pet_embeds, "get_pet_card", MagicMock(return_value=None))

    evolved, _file = pet_embeds.build_evolution_embed(pet)
    graveyard = pet_embeds.build_graveyard_embed(
        [pet],
        "Owner",
        callingdex=([PetCalling.ORACLE], len(PetCalling)),
    )

    assert "Oracle" in evolved.title
    assert "Fortune" in evolved.description
    assert "Wisdom" in evolved.description
    assert "Oracle" in graveyard.description
    callingdex = next(
        field for field in graveyard.fields if field.name.startswith("Callings")
    )
    assert "Oracle" in callingdex.value
    assert "???" in callingdex.value


def test_evolution_motifs_are_deterministic_and_change_the_adult_card():
    base = render_pet_card(
        "common_cama",
        "adult",
        "happy",
        seed=7,
    ).getvalue()
    evolved = render_pet_card(
        "common_cama",
        "adult",
        "happy",
        seed=7,
        calling=PetCalling.PROSPECTOR,
        primary=PetInstinct.DELVING,
        secondary=PetInstinct.FORTUNE,
    ).getvalue()
    repeated = render_pet_card(
        "common_cama",
        "adult",
        "happy",
        seed=7,
        calling=PetCalling.PROSPECTOR,
        primary=PetInstinct.DELVING,
        secondary=PetInstinct.FORTUNE,
    ).getvalue()

    assert evolved != base
    assert evolved == repeated
    with Image.open(io.BytesIO(evolved)) as image:
        assert image.size == (512, 288)
