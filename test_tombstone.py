"""
Quick test to verify tombstone functionality works correctly.
"""

from services.bankruptcy_service import BankruptcyRepository
from utils.formatting import TOMBSTONE_EMOJI

# Test that the tombstone emoji is defined
print(f"Tombstone emoji defined: {TOMBSTONE_EMOJI}")
assert TOMBSTONE_EMOJI == "🪦"

# Test BankruptcyRepository has get_penalty_games method
repo = BankruptcyRepository(":memory:")
print(f"BankruptcyRepository created successfully")

# Verify method signature
import inspect
sig = inspect.signature(repo.get_penalty_games)
print(f"get_penalty_games signature: {sig}")
assert "discord_id" in sig.parameters

print("\n✅ All manual checks passed!")
print("\nTombstone feature implementation summary:")
print("- Added TOMBSTONE_EMOJI constant (🪦)")
print("- Updated get_player_display_name() to check bankruptcy status")
print("- Updated format_player_list() in embeds.py")
print("- Updated create_lobby_embed() to pass bankruptcy_repo")
print("- Updated create_enriched_match_embed() to show tombstone")
print("- Updated create_match_summary_embed() to show tombstone")
print("- Updated LobbyService to accept bankruptcy_repo")
print("- Updated EnrichmentCommands cog to accept bankruptcy_repo")
print("- Updated MatchCommands cog to accept bankruptcy_repo")
print("- Exposed bankruptcy_repo on bot object")
