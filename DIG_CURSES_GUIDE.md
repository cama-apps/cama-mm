# Dig Curses Guide

Curses are negative status effects that can be applied to your miner during dig events. They persist for a set number of digs and reduce various benefits.

## Curse Effects

Curses can affect your dig performance in five ways:

- **Advance Bonus** (negative): Reduces blocks dug per dig
- **Cave-In Bonus** (positive values increase cave-in chance): Increases probability of cave-ins
- **Cooldown Penalty**: Increases cooldown duration between paid digs
- **JC Bonus** (negative): Reduces JC earned per dig
- **Luminosity Drain** (extra): Additional light consumed per dig

## All Curses by Effect Type

### Blocks Dug Reduction Curses

| Curse Name | ID | Duration | Effect |
|---|---|---|---|
| Court Disfavor | hex_hollow_court_audience | 4 digs | -4 blocks |
| Blackblooded Mark | hex_kurgal_summons | 4 digs | -4 blocks |
| Named in the Book | hex_mages_archive | 3 digs | -3 blocks |
| Wrong-Name Hex | hex_necro_s4 | 3 digs | -3 blocks |
| Sanguine Drain | hex_sanguine_pact | 3 digs | -3 blocks |
| Rearranged Mind | hex_whispering_walls_extended | 4 digs | -3 blocks |
| False Route | hex_false_route | 6 digs | -2 blocks (-20 luminosity) |
| Salt-Glyph Bind | hex_glyph_pulse | 3 digs | -2 blocks |
| Mirror-Pull | hex_mirror_tunnel | 3 digs | -2 blocks |
| Thin-Air Curse | hex_necro_s1 | 3 digs | -2 blocks |
| Void Tithe | hex_void_whispers | 3 digs | -2 blocks |
| Resonant Hands | hex_resonant_hands | 4 digs | -1 block |

### JC Earning Reduction Curses

| Curse Name | ID | Duration | Effect |
|---|---|---|---|
| Undermining Acid | hex_branns_potion | 4 digs | -8 JC/dig |
| Bottled Curse | hex_damned_bottle | 4 digs | -8 JC/dig |
| Abyssal Lien | hex_abyssal_fishing | 3 digs | -4 JC/dig |
| Whispering Debt | hex_whispering_token | 3 digs | -4 JC/dig |
| Shrine-Hex | hex_hex_cursed_shrine | 4 digs | -5 JC/dig |
| The Necromancer's Note | hex_necro_s5 | 4 digs | -5 JC/dig |
| Vaal Corruption | hex_vaal_side_area | 4 digs | -5 JC/dig |
| Dragon's Notice | hex_bolas_s1 | 3 digs | -3 JC/dig |
| Crimson Stain | hex_crimson_drizzle | 3 digs | -3 JC/dig |
| Chestbite Hex | hex_cursed_chest | 3 digs | -3 JC/dig |

### Luminosity Drain Curses

These consume additional light per dig:

| Curse Name | ID | Duration | Extra Drain |
|---|---|---|---|
| The Eye's Attention | hex_the_eye_opens | 4 digs | +25 light/dig |
| Siren-Debt | hex_sirens_hollow | 4 digs | +18 light/dig |
| Stolen Voice | hex_stolen_voice | 5 digs | +18 light/dig (-2 JC) |
| False Route | hex_false_route | 6 digs | +20 light/dig (-2 blocks) |
| Voidstare | hex_enderman_stare | 3 digs | +14 light/dig |
| The Mother's Mark | hex_the_mothers_mark | 3 digs | +14 light/dig |
| Salted Shadow | hex_salted_shadow | 3 digs | +12 light/dig |
| Fel Chill | hex_mossfire_echo | 3 digs | +10 light/dig |
| Counted Breaths | hex_necro_s2 | 3 digs | +10 light/dig |
| Clinging Dark | hex_things_in_the_dark | 3 digs | +10 light/dig |

### Cooldown Penalty Curses

| Curse Name | ID | Duration | Effect |
|---|---|---|---|
| Borrowed Time | clockwork_toll_curse | 3 digs | +20% cooldown |

### Cave-In Increase Curses

| Curse Name | ID | Duration | Effect |
|---|---|---|---|
| Compound Spores | spore_debt_curse | 3 digs | +8% cave-in chance |

## Curse Procurement

Curses are obtained through dig events and specific locations:
- Random events during digs
- Boss encounters
- Special threat events
- Event choices that result in curse outcomes

## Curse Management

### Removal Strategies

1. **Complete the Duration**: Simply dig enough times for the curse to expire
2. **Event Resolution**: Some events allow you to resolve or replace curses
3. **Curse Replacement**: Certain events can replace an active curse with a different one (often strengthened)

### Curse Effects Summary

- **Worst JC Loss**: Undermining Acid & Bottled Curse (-8 JC/dig for 4 digs = -32 JC)
- **Worst Block Loss**: Court Disfavor & Blackblooded Mark (-4 blocks/dig for 4 digs = -16 blocks)
- **Worst Light Drain**: The Eye's Attention (+25 light/dig for 4 digs = +100 light consumed)
- **Longest Duration**: False Route (6 digs)
- **Multiple Effects**: Stolen Voice (light + JC), False Route (blocks + light)

## Strategy Notes

- Curses with JC penalties are manageable if you have other income sources
- Block reduction curses significantly impact depth progression
- Luminosity drain curses can be mitigated by carrying torches or managing light carefully
- Some curses (like Borrowed Time's cooldown penalty) are less impactful for free digs

Total Unique Curses: **33**
