import commands.tax as tax_commands


def test_tax_group_is_audit_only():
    names = {cmd.name for cmd in tax_commands.TaxCommands.tax.walk_commands()}

    assert names == {"audit", "player", "ledger"}
