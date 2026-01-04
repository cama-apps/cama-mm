import pytest

from utils.interaction_safety import safe_followup


class _StubFollowup:
    def __init__(self):
        self.last_kwargs = None

    async def send(self, **kwargs):
        self.last_kwargs = kwargs
        return "ok"


class _StubResponse:
    def is_done(self):
        return False


class _StubInteraction:
    def __init__(self):
        self.id = 123
        self.followup = _StubFollowup()
        self.response = _StubResponse()
        self.channel = None


@pytest.mark.asyncio
async def test_safe_followup_does_not_raise_due_to_dbg():
    interaction = _StubInteraction()

    result = await safe_followup(interaction, content="hi", ephemeral=True)

    assert result == "ok"
    assert interaction.followup.last_kwargs["content"] == "hi"
    assert interaction.followup.last_kwargs["ephemeral"] is True
