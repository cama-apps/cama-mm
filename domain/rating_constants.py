"""Canonical constants for displaying OpenSkill ratings."""

# OpenSkill display constants shared by rating systems and domain models.
OPENSKILL_MIN_MU = 25.0  # mu floor (display rating 0)
# Display scale factor: (mu - MIN_MU) * DISPLAY_SCALE = display rating.
# Chosen so that mmr_to_os_mu(MMR_MAX) produces display == Glicko-2 RATING_MAX.
# With mmr_to_os_mu(mmr) = 25 + mmr/200 and MMR_MAX=12000 → mu=85, and
# Glicko-2 RATING_MAX=3000, we need factor = 3000 / (85 - 25) = 50.
# This keeps OpenSkill and Glicko-2 display ratings on the same 0-3000 scale
# so team-value computations do not inflate OpenSkill-rated players.
OPENSKILL_DISPLAY_SCALE = 50.0
