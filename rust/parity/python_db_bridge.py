"""Python side of the serial SQLite interoperability gate."""

from __future__ import annotations

import sys
from pathlib import Path

from database import Database
from repositories.duel_challenge_repository import DuelChallengeRepository
from repositories.economy_event_repository import EconomyEventRepository
from repositories.guild_config_repository import GuildConfigRepository
from repositories.loan_repository import LoanRepository
from repositories.low_priority_repository import LowPriorityRepository
from repositories.package_deal_repository import PackageDealRepository
from repositories.pairings_repository import PairingsRepository
from repositories.pet_brawl_repository import PetBrawlRepository
from repositories.pet_repository import PetRepository
from repositories.player_repository import PlayerRepository
from repositories.soft_avoid_repository import SoftAvoidRepository
from repositories.tip_repository import TipRepository

GUILD_ID = 424_242
LOW_PRIORITY_PLAYER_ID = 515_151
SOFT_AVOID_AVOIDER_ID = 616_161
SOFT_AVOID_AVOIDED_ID = 626_262
PACKAGE_BUYER_ID = 717_171
PACKAGE_PARTNER_ID = 727_272
TIP_SENDER_ID = 818_181
TIP_RECIPIENT_ID = 828_282
PAIRING_PLAYER_1_ID = 919_191
PAIRING_PLAYER_2_ID = 929_292
PAIRING_PLAYER_3_ID = 939_393
PET_OWNER_ID = 1_010_101
PET_NOW = 1_800_000_000
PET_HATCH_SECONDS = 86_400
BRAWL_CHALLENGER_ID = 1_020_202
BRAWL_RECIPIENT_ID = 1_030_303
DUEL_CHALLENGER_ID = 1_040_404
DUEL_RECIPIENT_ID = 1_050_505
DUEL_NOW = 1_900_000_000
ECONOMY_POLICY_NOW = 1_700_000_000
LOAN_GUILD_ID = 434_343
LOAN_PLAYER_ID = 1_060_606


def _render(
    guild_repository: GuildConfigRepository,
    low_priority_repository: LowPriorityRepository,
    soft_avoid_repository: SoftAvoidRepository,
    package_deal_repository: PackageDealRepository,
    tip_repository: TipRepository,
    pairings_repository: PairingsRepository,
    pet_repository: PetRepository,
    player_repository: PlayerRepository,
    pet_brawl_repository: PetBrawlRepository,
    duel_repository: DuelChallengeRepository,
    economy_event_repository: EconomyEventRepository,
    loan_repository: LoanRepository,
) -> str:
    config = guild_repository.get_config(GUILD_ID)
    if config is None:
        raise RuntimeError("guild_config interoperability row is missing")
    low_priority = low_priority_repository.get_state(
        LOW_PRIORITY_PLAYER_ID,
        GUILD_ID,
    )
    if low_priority is None:
        raise RuntimeError("low_priority interoperability row is missing")
    soft_avoids = soft_avoid_repository.get_active_avoids_for_players(
        GUILD_ID,
        [SOFT_AVOID_AVOIDER_ID, SOFT_AVOID_AVOIDED_ID],
    )
    if len(soft_avoids) != 1:
        raise RuntimeError("soft_avoid interoperability row is missing or ambiguous")
    soft_avoid = soft_avoids[0]
    package_deals = package_deal_repository.get_active_deals_for_players(
        GUILD_ID,
        [PACKAGE_BUYER_ID, PACKAGE_PARTNER_ID],
    )
    if len(package_deals) != 1:
        raise RuntimeError("package-deal interoperability row is missing or ambiguous")
    package_deal = package_deals[0]
    tip_stats = tip_repository.get_user_tip_stats(TIP_SENDER_ID, GUILD_ID)
    pairings = pairings_repository.get_pairings_for_players(
        [PAIRING_PLAYER_1_ID, PAIRING_PLAYER_2_ID, PAIRING_PLAYER_3_ID],
        GUILD_ID,
    )
    teammate_pairing = pairings[(PAIRING_PLAYER_1_ID, PAIRING_PLAYER_2_ID)]
    opponent_pairing = pairings[(PAIRING_PLAYER_1_ID, PAIRING_PLAYER_3_ID)]
    pet = pet_repository.get_active_pet(PET_OWNER_ID, GUILD_ID)
    if pet is None:
        raise RuntimeError("pet interoperability row is missing")
    supplies = pet_repository.get_supplies(PET_OWNER_ID, GUILD_ID)
    with pet_repository.connection() as connection:
        pet_ledger_count = connection.execute(
            "SELECT COUNT(*) FROM economy_ledger_entries WHERE source = 'pet'"
        ).fetchone()[0]
    brawl = pet_brawl_repository.get_brawl(1, GUILD_ID)
    if brawl is None:
        raise RuntimeError("pet-brawl interoperability row is missing")
    duel = duel_repository.get_challenge(1, GUILD_ID)
    if duel is None:
        raise RuntimeError("duel interoperability row is missing")
    with player_repository.connection() as connection:
        wheel_rows = connection.execute(
            """
            SELECT result, outcome_code, is_bonus, event_id, outcome_metadata
            FROM wheel_spins
            WHERE discord_id = ? AND guild_id = ?
            ORDER BY spin_id ASC
            """,
            (PET_OWNER_ID, GUILD_ID),
        ).fetchall()
    if not wheel_rows:
        raise RuntimeError("wheel-spin interoperability row is missing")
    first_wheel = wheel_rows[0]
    latest_wheel = wheel_rows[-1]
    economy_policy = economy_event_repository.get_policy_state(GUILD_ID)
    if not economy_policy:
        raise RuntimeError("economy-policy interoperability row is missing")
    loan_state = loan_repository.get_state(LOAN_PLAYER_ID, LOAN_GUILD_ID)
    if not loan_state:
        raise RuntimeError("loan interoperability row is missing")
    core_player = player_repository.get_by_id(PET_OWNER_ID, GUILD_ID)
    if core_player is None:
        raise RuntimeError("core player interoperability row is missing")
    loan_balance = player_repository.get_balance(LOAN_PLAYER_ID, LOAN_GUILD_ID)
    loan_fund = loan_repository.get_nonprofit_fund(LOAN_GUILD_ID)
    with player_repository.connection() as connection:
        loan_ledger_sources = connection.execute(
            """
            SELECT source
            FROM economy_ledger_entries
            WHERE guild_id = ? AND source IN ('loan', 'loan_repayment')
            ORDER BY ledger_id ASC
            """,
            (LOAN_GUILD_ID,),
        ).fetchall()
    return "\t".join(
        (
            str(config["guild_id"]),
            str(config["league_id"]),
            str(bool(config["auto_enrich_matches"])).lower(),
            str(bool(config["ai_features_enabled"])).lower(),
            str(low_priority.discord_id),
            str(low_priority.wins_required),
            str(low_priority.wins_remaining),
            str(low_priority.start_pending_match_id),
            str(low_priority.active).lower(),
            str(low_priority.reason),
            str(low_priority.set_by),
            str(soft_avoid.guild_id),
            str(soft_avoid.avoider_discord_id),
            str(soft_avoid.avoided_discord_id),
            str(soft_avoid.games_remaining),
            str(package_deal.guild_id),
            str(package_deal.buyer_discord_id),
            str(package_deal.partner_discord_id),
            str(package_deal.games_remaining),
            str(package_deal.cost_paid),
            str(tip_stats["total_sent"]),
            str(tip_stats["tips_sent_count"]),
            str(tip_stats["fees_paid"]),
            str(tip_stats["total_received"]),
            str(tip_stats["tips_received_count"]),
            str(teammate_pairing["games_together"]),
            str(teammate_pairing["wins_together"]),
            str(opponent_pairing["games_against"]),
            str(opponent_pairing["player1_wins_against"]),
            str(max(teammate_pairing["last_match_id"], opponent_pairing["last_match_id"])),
            str(pet.discord_id),
            pet.name,
            pet.species,
            pet.egg_tier,
            str(pet.adopted_at),
            str(pet.hatched_at),
            str(player_repository.get_balance(PET_OWNER_ID, GUILD_ID)),
            str(supplies.get("tango", 0)),
            str(pet_ledger_count),
            str(brawl.brawl_id),
            str(brawl.status),
            str(brawl.challenger_pet_id),
            str(brawl.recipient_pet_id) if brawl.recipient_pet_id is not None else "none",
            str(duel.challenge_id),
            duel.status.value,
            str(duel.message_id) if duel.message_id is not None else "none",
            duel.trial_type.value if duel.trial_type is not None else "none",
            str(player_repository.get_balance(DUEL_CHALLENGER_ID, GUILD_ID)),
            str(player_repository.get_balance(DUEL_RECIPIENT_ID, GUILD_ID)),
            str(len(wheel_rows)),
            str(first_wheel["result"]),
            str(first_wheel["outcome_code"]),
            str(first_wheel["is_bonus"]),
            str(first_wheel["event_id"]),
            str(first_wheel["outcome_metadata"]),
            str(latest_wheel["result"]),
            str(latest_wheel["outcome_code"]),
            str(latest_wheel["is_bonus"]),
            str(latest_wheel["event_id"]),
            str(latest_wheel["outcome_metadata"]),
            str(economy_policy["guild_id"]),
            str(economy_policy["mode"]),
            str(economy_policy["target_annual_rate"]),
            str(economy_policy["inflation_ceiling"]),
            (
                str(economy_policy["recovery_started_at"])
                if economy_policy["recovery_started_at"] is not None
                else "none"
            ),
            str(economy_policy["updated_at"]),
            str(loan_state["discord_id"]),
            str(loan_state["guild_id"]),
            str(loan_state["total_loans_taken"]),
            str(loan_state["total_fees_paid"]),
            str(loan_state["negative_loans_taken"]),
            str(loan_state["outstanding_principal"]),
            str(loan_state["outstanding_fee"]),
            str(loan_balance),
            str(loan_fund),
            ",".join(str(row["source"]) for row in loan_ledger_sources),
            ",".join(core_player.preferred_roles or []),
            (
                str(core_player.glicko_rating)
                if core_player.glicko_rating is not None
                else "none"
            ),
            str(core_player.glicko_rd) if core_player.glicko_rd is not None else "none",
            (
                str(core_player.glicko_volatility)
                if core_player.glicko_volatility is not None
                else "none"
            ),
        )
    )


def main() -> None:
    if len(sys.argv) != 3 or sys.argv[1] not in {"seed", "read"}:
        raise SystemExit("usage: python_db_bridge.py {seed|read} DB_PATH")

    operation = sys.argv[1]
    path = Path(sys.argv[2])
    if operation == "seed":
        database = Database(str(path))
        database.close()
        guild_repository = GuildConfigRepository(str(path))
        guild_repository.set_league_id(GUILD_ID, 777)
        guild_repository.set_auto_enrich(GUILD_ID, False)
        guild_repository.set_ai_enabled(GUILD_ID, True)
        economy_event_repository = EconomyEventRepository(str(path))
        economy_event_repository.ensure_policy_state(
            GUILD_ID,
            mode="normal",
            target_annual_rate=0.02,
            inflation_ceiling=0.08,
            now=ECONOMY_POLICY_NOW,
        )
        low_priority_repository = LowPriorityRepository(str(path))
        low_priority_repository.set_low_priority(
            LOW_PRIORITY_PLAYER_ID,
            GUILD_ID,
            set_by=901,
            reason="python seed",
            wins_required=5,
            start_pending_match_id=41,
        )
        soft_avoid_repository = SoftAvoidRepository(str(path))
        soft_avoid_repository.create_or_reactivate_avoid(
            GUILD_ID,
            SOFT_AVOID_AVOIDER_ID,
            SOFT_AVOID_AVOIDED_ID,
            games=3,
        )
        package_deal_repository = PackageDealRepository(str(path))
        package_deal_repository.create_or_extend_deal(
            GUILD_ID,
            PACKAGE_BUYER_ID,
            PACKAGE_PARTNER_ID,
            games=4,
            cost=70,
        )
        tip_repository = TipRepository(str(path))
        tip_repository.log_tip(
            sender_id=TIP_SENDER_ID,
            recipient_id=TIP_RECIPIENT_ID,
            amount=12,
            fee=2,
            guild_id=GUILD_ID,
        )
        pairings_repository = PairingsRepository(str(path))
        pairings_repository.update_pairings_for_match(
            901,
            GUILD_ID,
            [PAIRING_PLAYER_1_ID, PAIRING_PLAYER_2_ID],
            [PAIRING_PLAYER_3_ID],
            1,
        )
        player_repository = PlayerRepository(str(path))
        player_repository.add(LOAN_PLAYER_ID, "Loan interop", LOAN_GUILD_ID)
        player_repository.update_balance(LOAN_PLAYER_ID, LOAN_GUILD_ID, 50)
        loan_repository = LoanRepository(str(path))
        loan_repository.execute_loan_atomic(
            LOAN_PLAYER_ID,
            LOAN_GUILD_ID,
            amount=100,
            fee=10,
            cooldown_seconds=86_400,
            max_amount=200,
        )
        player_repository.add(PET_OWNER_ID, "Pet interop", GUILD_ID)
        player_repository.update_balance(PET_OWNER_ID, GUILD_ID, 1_000)
        pet_repository = PetRepository(str(path))
        pet_repository.adopt_pet(
            PET_OWNER_ID,
            GUILD_ID,
            name="Interop Blep",
            egg_tier="standard",
            fee=20,
            now=PET_NOW,
            hatch_seconds=PET_HATCH_SECONDS,
        )
        player_repository.log_wheel_spin(
            PET_OWNER_ID,
            GUILD_ID,
            25,
            1_700_000_000,
            outcome_code="CROWN",
            event_id="python-wheel-1",
            outcome_metadata={"z": 2, "a": {"b": True}},
        )
        brawl_pets = []
        for owner_id, name in (
            (BRAWL_CHALLENGER_ID, "Brawl Alpha"),
            (BRAWL_RECIPIENT_ID, "Brawl Beta"),
        ):
            player_repository.add(owner_id, name, GUILD_ID)
            player_repository.update_balance(owner_id, GUILD_ID, 100)
            egg = pet_repository.adopt_pet(
                owner_id,
                GUILD_ID,
                name=name,
                egg_tier="standard",
                fee=20,
                now=PET_NOW,
                hatch_seconds=PET_HATCH_SECONDS,
            )
            brawl_pets.append(
                pet_repository.resolve_hatch_species(
                    egg.pet_id,
                    GUILD_ID,
                    species="common_cama",
                    now=egg.hatched_at,
                )
            )
        pet_brawl_repository = PetBrawlRepository(str(path))
        pet_brawl_repository.create_brawl_atomic(
            GUILD_ID,
            222,
            BRAWL_CHALLENGER_ID,
            BRAWL_RECIPIENT_ID,
            brawl_pets[0].pet_id,
            now=PET_NOW + PET_HATCH_SECONDS + 1,
            expires_at=PET_NOW + PET_HATCH_SECONDS + 181,
        )
        for player_id, rating in (
            (DUEL_CHALLENGER_ID, 1_400.0),
            (DUEL_RECIPIENT_ID, 1_500.0),
        ):
            player_repository.add(
                player_id,
                f"Duel {player_id}",
                GUILD_ID,
                glicko_rating=rating,
                glicko_rd=80.0,
                glicko_volatility=0.06,
            )
            player_repository.update_balance(player_id, GUILD_ID, 1_000)
        duel_repository = DuelChallengeRepository(str(path))
        duel_repository.create_challenge_atomic(
            GUILD_ID,
            333,
            DUEL_CHALLENGER_ID,
            DUEL_RECIPIENT_ID,
            500,
            DUEL_NOW,
            30 * 86_400,
            7 * 86_400,
            7 * 86_400,
            DUEL_CHALLENGER_ID,
        )
    else:
        if not path.is_file():
            raise RuntimeError("Rust interoperability database does not exist")
        guild_repository = GuildConfigRepository(str(path))
        low_priority_repository = LowPriorityRepository(str(path))
        soft_avoid_repository = SoftAvoidRepository(str(path))
        package_deal_repository = PackageDealRepository(str(path))
        tip_repository = TipRepository(str(path))
        pairings_repository = PairingsRepository(str(path))
        pet_repository = PetRepository(str(path))
        player_repository = PlayerRepository(str(path))
        pet_brawl_repository = PetBrawlRepository(str(path))
        duel_repository = DuelChallengeRepository(str(path))
        economy_event_repository = EconomyEventRepository(str(path))
        loan_repository = LoanRepository(str(path))

    print(
        _render(
            guild_repository,
            low_priority_repository,
            soft_avoid_repository,
            package_deal_repository,
            tip_repository,
            pairings_repository,
            pet_repository,
            player_repository,
            pet_brawl_repository,
            duel_repository,
            economy_event_repository,
            loan_repository,
        )
    )


if __name__ == "__main__":
    main()
