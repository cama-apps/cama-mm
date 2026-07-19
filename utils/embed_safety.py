"""Discord embed safety utilities.

Provides utilities for ensuring Discord embed content stays within limits.
"""

from __future__ import annotations

import discord

EMBED_LIMITS = {
    "title": 256,
    "author_name": 256,
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


def add_lines_field(
    embed: discord.Embed,
    name: str,
    lines: list[str],
    *,
    inline: bool = False,
    max_len: int = EMBED_LIMITS["field_value"],
) -> None:
    """Add a field whose value is newline-joined ``lines``, splitting into
    multiple fields when the joined text would exceed Discord's field-value
    limit, so the list isn't dropped wholesale.

    A single line longer than ``max_len`` can't be split further and is
    truncated (with an ellipsis) rather than emitted as an over-limit field.
    Assumes the resulting field count stays within Discord's 25-field cap;
    for lists large enough to exceed that, paginate instead. Continuation
    fields use a blank (zero-width) name so the section still reads as one
    block. No-op when ``lines`` is empty.
    """
    if not lines:
        return

    chunks: list[str] = []
    current: list[str] = []
    length = 0
    for line in lines:
        extra = len(line) + (1 if current else 0)  # +1 for the joining newline
        if current and length + extra > max_len:
            chunks.append("\n".join(current))
            current, length = [line], len(line)
        else:
            current.append(line)
            length += extra
    chunks.append("\n".join(current))

    for i, chunk in enumerate(chunks):
        embed.add_field(
            name=name if i == 0 else "​",
            value=truncate_field(chunk, max_len),  # guard a single over-long line
            inline=inline,
        )


def validate_embed(embed: discord.Embed) -> list[str]:
    """Return list of validation errors, empty if valid.

    This is a debugging/testing utility for verifying embed content
    stays within Discord's limits. Use during development to catch
    potential issues before they reach Discord's API.

    Args:
        embed: A discord.Embed object to validate

    Returns:
        List of error messages, empty if embed is valid
    """
    errors = []

    # Check title
    if embed.title and len(embed.title) > EMBED_LIMITS["title"]:
        errors.append(
            f"Title exceeds {EMBED_LIMITS['title']} chars ({len(embed.title)})"
        )

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

    # Check author name
    if embed.author and embed.author.name and len(embed.author.name) > EMBED_LIMITS["author_name"]:
        errors.append(
            f"Author name exceeds {EMBED_LIMITS['author_name']} chars ({len(embed.author.name)})"
        )

    # Check field count
    if len(embed.fields) > EMBED_LIMITS["max_fields"]:
        errors.append(f"Too many fields: {len(embed.fields)} > {EMBED_LIMITS['max_fields']}")

    # Check combined size (Discord counts title, description, footer text,
    # author name, and all field names/values against a 6000-char total)
    total = len(embed.title or "") + len(embed.description or "")
    if embed.footer and embed.footer.text:
        total += len(embed.footer.text)
    if embed.author and embed.author.name:
        total += len(embed.author.name)
    for field in embed.fields:
        total += len(field.name) + len(field.value)
    if total > EMBED_LIMITS["total"]:
        errors.append(f"Total embed size exceeds {EMBED_LIMITS['total']} chars ({total})")

    return errors
