"""Check bankruptcy state in database."""
import sqlite3

conn = sqlite3.connect('cama_shuffle.db')
cursor = conn.cursor()

# Check fake players
cursor.execute('SELECT discord_id, discord_username FROM players WHERE discord_id < 0')
print('Fake players:')
for row in cursor.fetchall():
    print(f'  {row[0]}: {row[1]}')

# Check bankruptcy states
cursor.execute('SELECT discord_id, penalty_games_remaining FROM bankruptcy_state')
print('\nBankruptcy states:')
rows = cursor.fetchall()
if rows:
    for row in rows:
        print(f'  Player {row[0]}: {row[1]} penalty games')
else:
    print('  (none)')

conn.close()
