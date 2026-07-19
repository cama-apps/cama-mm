"""Tests for provider-aware LLM startup configuration."""

from config import _select_llm_config


def test_groq_is_primary_when_both_keys_exist():
    assert _select_llm_config("groq-key", "cerebras-key", None) == (
        "groq/qwen/qwen3.6-27b",
        "groq-key",
    )


def test_cerebras_only_defaults_to_gemma_4():
    assert _select_llm_config(None, "cerebras-key", None) == (
        "cerebras/gemma-4-31b",
        "cerebras-key",
    )


def test_model_override_uses_matching_provider_key():
    assert _select_llm_config(
        "groq-key",
        "cerebras-key",
        "cerebras/gemma-4-31b",
    ) == ("cerebras/gemma-4-31b", "cerebras-key")


def test_unsupported_provider_does_not_reuse_another_provider_key():
    assert _select_llm_config(
        "groq-key",
        "cerebras-key",
        "other/model",
    ) == ("other/model", None)
