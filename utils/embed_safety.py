"""Discord embed safety utilities.

Provides utilities for ensuring Discord embed content stays within limits.
"""

EMBED_LIMITS = {
    "field_value": 1024,
    "field_name": 256,
    "description": 4096,
    "footer": 2048,
    "total": 6000,
    "max_fields": 25,
}


def truncate_field(text: str, max_len: int = 1024) -> str:
    """Truncate text to fit Discord field limit.

    Args:
        text: The text to truncate
        max_len: Maximum length (default 1024 for field values)

    Returns:
        Original text if within limit, truncated with "..." if over
    """
    if len(text) <= max_len:
        return text
    return text[: max_len - 3] + "..."


def validate_embed(embed) -> list[str]:
    """Return list of validation errors, empty if valid.

    Args:
        embed: A discord.Embed object to validate

    Returns:
        List of error messages, empty if embed is valid
    """
    errors = []

    # Check description
    if embed.description and len(embed.description) > EMBED_LIMITS["description"]:
        errors.append(
            f"Description exceeds {EMBED_LIMITS['description']} chars ({len(embed.description)})"
        )

    # Check fields
    for i, field in enumerate(embed.fields):
        if len(field.name) > EMBED_LIMITS["field_name"]:
            errors.append(
                f"Field {i} name '{field.name[:20]}...' exceeds {EMBED_LIMITS['field_name']} chars"
            )
        if len(field.value) > EMBED_LIMITS["field_value"]:
            errors.append(
                f"Field {i} '{field.name}' value exceeds {EMBED_LIMITS['field_value']} chars ({len(field.value)})"
            )

    # Check footer
    if embed.footer and embed.footer.text and len(embed.footer.text) > EMBED_LIMITS["footer"]:
        errors.append(f"Footer exceeds {EMBED_LIMITS['footer']} chars ({len(embed.footer.text)})")

    # Check field count
    if len(embed.fields) > EMBED_LIMITS["max_fields"]:
        errors.append(f"Too many fields: {len(embed.fields)} > {EMBED_LIMITS['max_fields']}")

    return errors
