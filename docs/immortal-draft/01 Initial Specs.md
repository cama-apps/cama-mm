Current Matchmaking system:
- Algorithmically calculate "balanced teams"

We would like to implement an alternative matchmaking mode: "Immortal Draft" mode (what it's called in game). 

What we want this to look like:

Registering phase:
- users can run a new command /setcaptain (yes) to be considered an eligible captain

Matchmaking phase:
- /lobby should stay the same. Opens up a lobby where people can react (think its' currently up to 14)
- Existing system is to run /shuffle.
- New desired system is to run /startdraft, which should start the following draft process:

Immortal Draft:
- Select the pool of 10 players out of the people who are registered in /lobby. Excluded players in previous games should be prioritized (this should already exist)
- /startdraft should take in two optional argumenets - captain1 and captain2. Both should be user arguments. Captains not specified should be automatically assigned. Requirements:
    - Captains must have /setcaptain true.
    - Have some max rating threshold diff between captains.
- Pick order should be a snake 1-2-2-2-1. Lower rating catpain should have choice between first and second pick
- Captains take turns drafting players. Ideally, only the captains need to input anything to draft.
- Some /finalizedraft to conclude drafting process.

The draft process is pretty established and not fundamnetally difficult to implement. The biggest challenge will be our UI constraint. The draft should take place in Discord, so we are limited in UI options. We should discuss this further.