"""
Tests for permission helpers.
"""

from types import SimpleNamespace

from services.permissions import (
    has_admin_permission,
    has_allowlisted_admin,
    has_allowlisted_tax_man,
    has_tax_man_permission,
)


def test_has_allowlisted_admin(monkeypatch):
    monkeypatch.setattr("services.permissions.ADMIN_USER_IDS", [101])
    interaction = SimpleNamespace(user=SimpleNamespace(id=101))

    assert has_allowlisted_admin(interaction) is True


def test_has_admin_permission_allowlist(monkeypatch):
    monkeypatch.setattr("services.permissions.ADMIN_USER_IDS", [202])
    interaction = SimpleNamespace(user=SimpleNamespace(id=202), guild=None)

    assert has_admin_permission(interaction) is True


def test_has_admin_permission_guild_member_permissions(monkeypatch):
    monkeypatch.setattr("services.permissions.ADMIN_USER_IDS", [])

    perms = SimpleNamespace(administrator=True, manage_guild=False)
    member = SimpleNamespace(guild_permissions=perms)
    guild = SimpleNamespace(get_member=lambda _uid: member)
    interaction = SimpleNamespace(user=SimpleNamespace(id=303), guild=guild)

    assert has_admin_permission(interaction) is True


def test_has_admin_permission_user_permissions_fallback(monkeypatch):
    monkeypatch.setattr("services.permissions.ADMIN_USER_IDS", [])

    perms = SimpleNamespace(administrator=False, manage_guild=True)
    interaction = SimpleNamespace(user=SimpleNamespace(id=404, guild_permissions=perms), guild=None)

    assert has_admin_permission(interaction) is True


def test_has_admin_permission_false(monkeypatch):
    monkeypatch.setattr("services.permissions.ADMIN_USER_IDS", [])
    interaction = SimpleNamespace(user=SimpleNamespace(id=505), guild=None)

    assert has_admin_permission(interaction) is False


def test_has_allowlisted_tax_man(monkeypatch):
    monkeypatch.setattr("services.permissions.TAX_MAN_USER_IDS", [606])
    interaction = SimpleNamespace(user=SimpleNamespace(id=606))

    assert has_allowlisted_tax_man(interaction) is True


def test_has_tax_man_permission_defaults_to_admin(monkeypatch):
    monkeypatch.setattr("services.permissions.ADMIN_USER_IDS", [707])
    monkeypatch.setattr("services.permissions.TAX_MAN_USER_IDS", [])
    interaction = SimpleNamespace(user=SimpleNamespace(id=707), guild=None)

    assert has_tax_man_permission(interaction) is True


def test_has_tax_man_permission_allowlist(monkeypatch):
    monkeypatch.setattr("services.permissions.ADMIN_USER_IDS", [])
    monkeypatch.setattr("services.permissions.TAX_MAN_USER_IDS", [808])
    interaction = SimpleNamespace(user=SimpleNamespace(id=808), guild=None)

    assert has_tax_man_permission(interaction) is True


def test_has_tax_man_permission_false(monkeypatch):
    monkeypatch.setattr("services.permissions.ADMIN_USER_IDS", [])
    monkeypatch.setattr("services.permissions.TAX_MAN_USER_IDS", [])
    interaction = SimpleNamespace(user=SimpleNamespace(id=909), guild=None)

    assert has_tax_man_permission(interaction) is False
