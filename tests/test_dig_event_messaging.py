"""Event flavor and result rendering must match mechanical outcomes.

A risky-success line that reads as coin flowing TO the digger ("spills your
way") must not sit on an outcome that actually costs the digger JC. Players
saw that positive-sounding flavor next to a negative balance line and were
confused about whether the event had helped or hurt them.
"""
from commands.dig_helpers.event_views import EventEncounterView
from services.dig_constants import RANDOM_EVENTS

# Phrases that read as coin flowing toward the digger. An outcome whose text
# uses one must not be a JC loss.
_GAIN_IMPLYING_PHRASES = ("spills your way", "spill your way")


def _outcomes(event):
    """Yield every EventOutcome reachable from a RandomEvent."""
    choices = [event.safe_option, event.risky_option, event.desperate_option]
    for step in (event.steps or ()):
        choices.extend(step.choices)
    for choice in choices:
        if choice is None:
            continue
        for outcome in (choice.success, choice.failure):
            if outcome is not None:
                yield outcome


def test_no_gain_flavor_on_jc_loss_outcomes():
    offenders = [
        f"{event.id}: {outcome.description!r} (jc={outcome.jc})"
        for event in RANDOM_EVENTS
        for outcome in _outcomes(event)
        if outcome.jc < 0
        and any(p in outcome.description.lower() for p in _GAIN_IMPLYING_PHRASES)
    ]
    assert not offenders, (
        "Event outcomes whose flavor reads as a coin gain but cost JC:\n"
        + "\n".join(offenders)
    )


async def test_event_result_embed_surfaces_gear_drop_details():
    class GearDropService:
        def resolve_event(self, discord_id, guild_id, event_id, choice):
            return {
                "success": True,
                "succeeded": True,
                "message": "The armory coughs up one last bad idea.",
                "jc_delta": 4,
                "depth_delta": 0,
                "gear_drop": {
                    "name": "Glassbreaker Pick",
                    "slot": "weapon",
                    "durability": 8,
                    "effect": "Diamond dig bonuses; +2 boss damage; -8% hit chance.",
                },
            }

    view = EventEncounterView(
        dig_service=GearDropService(),
        user_id=10001,
        guild_id=12345,
        event_data={"id": "collapsed_armory", "name": "Collapsed Armory"},
    )

    embed = await view._resolve("risky")
    gear_field = next((field for field in embed.fields if field.name == "Gear Drop"), None)

    assert gear_field is not None
    assert "Glassbreaker Pick" in gear_field.value
    assert "Weapon" in gear_field.value
    assert "Durability: 8" in gear_field.value
    assert gear_field.value.endswith(
        "Diamond dig bonuses; +2 boss damage; -8% hit chance."
    )
