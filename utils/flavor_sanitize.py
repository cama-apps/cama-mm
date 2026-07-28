"""Shared sanitization for AI-generated flavor text shown in Discord.

Strips control/format characters, collapses whitespace, truncates, rejects
anything that looks like a link or Discord markup, and neutralizes mentions.
"""

from __future__ import annotations

import re
import unicodedata

DEFAULT_MAX_LENGTH = 220

_SCHEME_RE = re.compile(
    r"\b(?:[a-z][a-z0-9+.-]{1,31}://|(?:data|javascript|mailto|sms|tel):)",
    re.IGNORECASE,
)
_MARKDOWN_LINK_RE = re.compile(r"\[[^\]]+\]\s*\([^)]+\)")
_DOMAIN_RE = re.compile(
    r"\b(?:[a-z0-9-]+\.)+[a-z]{2,63}(?:[/:?#][^\s]*)?",
    re.IGNORECASE,
)
_IP_ADDRESS_RE = re.compile(
    r"\b(?:\d{1,3}\.){3}\d{1,3}(?::\d+)?(?:[/?#][^\s]*)?"
)
_DISCORD_MARKUP_RE = re.compile(r"<(?:@!?|@&|#|/|t:|a?:)[^>\r\n]+>")


def contains_link(value: str) -> bool:
    """Return True if the text contains a URL, domain, IP, or Discord markup."""
    return bool(
        _SCHEME_RE.search(value)
        or _MARKDOWN_LINK_RE.search(value)
        or _DOMAIN_RE.search(value)
        or _IP_ADDRESS_RE.search(value)
        or _DISCORD_MARKUP_RE.search(value)
    )


def sanitize_output(value: str | None, *, max_length: int = DEFAULT_MAX_LENGTH) -> str:
    """Sanitize AI output for display; returns "" if the text is unsafe."""
    if not value:
        return ""
    normalized = unicodedata.normalize("NFKC", value)
    cleaned = "".join(
        " " if unicodedata.category(character) in {"Cc", "Cf", "Cs"} else character
        for character in normalized
    )
    cleaned = " ".join(cleaned.split())[:max_length]
    if contains_link(cleaned):
        return ""
    return cleaned.replace("@", "＠")
