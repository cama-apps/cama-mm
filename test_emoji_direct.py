"""Test the emoji display directly"""
from services.bankruptcy_service import BankruptcyRepository
from repositories.player_repository import PlayerRepository
from utils.embeds import format_player_list

# Use actual database
player_repo = PlayerRepository("cama_shuffle.db")
bankruptcy_repo = BankruptcyRepository("cama_shuffle.db")

# Get your player
player = player_repo.get_by_id(627001217502937099)
players = [player]
player_ids = [627001217502937099]

# Test the format
formatted_list, count = format_player_list(players, player_ids, bankruptcy_repo=bankruptcy_repo)

print("Formatted player list:")
print(formatted_list)
print(f"\nCount: {count}")

# Check bankruptcy status
penalty_games = bankruptcy_repo.get_penalty_games(627001217502937099)
print(f"\nPenalty games for player: {penalty_games}")
