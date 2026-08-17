"""Long-lived Python side of deterministic Rust migration vectors."""

from __future__ import annotations

import sys
from dataclasses import replace
from types import SimpleNamespace

from commands.betting_helpers.disburse_embeds import build_disburse_embed
from commands.betting_helpers.economy_actions import (
    apply_gamba_event_multiplier,
    bounded_economy_multiplier,
)
from commands.betting_helpers.messages import WHEEL_EXPLOSION_REWARD
from commands.betting_helpers.wheel_outcomes import eruption_reward
from commands.shop import _calculate_package_deal_cost
from config import _select_llm_config
from domain.models.pet import PetStage
from domain.pet_brawl import (
    PetBrawlMove,
    brawl_traits,
    build_duelist,
    initial_state,
    move_name,
)
from domain.pet_brawl import resolve_round as resolve_pet_brawl_round
from domain.pet_evolution import PetInstinct, hint_key_for_scores, resolve_evolution
from openskill_rating_system import CamaOpenSkillSystem
from rating_system import CamaRatingSystem
from services.betting_personas import BETTING_PERSONAS
from services.dig_data.balance import (
    CAVE_IN_BLOCK_LOSS_RANGES,
    CAVE_IN_CATASTROPHIC_GEAR_TICKS,
    CAVE_IN_CATASTROPHIC_MEDICAL_BILL,
    CAVE_IN_CATASTROPHIC_MILESTONE_STEP,
    CAVE_IN_CATASTROPHIC_PCT_BY_BAND,
    CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE,
    CAVE_IN_CONSEQUENCE_WEIGHTS,
    CAVE_IN_INJURY_DIGS_BY_BAND,
    CAVE_IN_MEDICAL_BILL_RANGES,
    CAVE_IN_STUN_DIGS_BY_BAND,
    cave_in_band,
    scale_positive_dig_jc,
    strengthen_dig_event_penalty,
)
from services.dig_data.bosses import PHASE_TRANSITION_EVENTS
from services.dig_service import DigService
from services.flavor_personas import PERSONAS
from services.sql_query_service import QueryResult
from services.sql_query_service import SQLQueryService as PythonSQLQueryService
from services.wrapped_service import FLAVOR_POOLS
from utils import economy_scaling
from utils.embed_safety import add_lines_field, truncate_field, validate_embed
from utils.guild import is_dm_context, normalize_guild_id
from utils.rating_insights import get_rd_tier_name, rd_to_certainty
from utils.wheel_drawing import GOLDEN_WHEEL_WEDGES, WHEEL_WEDGES


def _optional_int(raw: str) -> int | None:
    return None if raw == "none" else int(raw)


def _optional_float(raw: str) -> float | None:
    return None if raw == "none" else float(raw)


def _optional_text(raw: str) -> str | None:
    return None if raw == "none" else raw


def _outcome(raw: str) -> bool:
    if raw == "W":
        return True
    if raw == "L":
        return False
    raise ValueError(f"invalid outcome {raw!r}")


def _format(value: object) -> str:
    if isinstance(value, bool):
        return str(value).lower()
    if isinstance(value, float):
        return format(value, ".17g")
    return str(value)


def _escape_vector_text(value: str) -> str:
    return (
        value.replace("\\", "\\\\")
        .replace("\t", "\\t")
        .replace("\r", "\\r")
        .replace("\n", "\\n")
    )


def _openskill_team(raw: str) -> list[tuple[float, float]]:
    return [tuple(map(float, player.split(","))) for player in raw.split(";")]


def _glicko_team(raw: str) -> list[tuple[float, float, float]]:
    players = [tuple(map(float, player.split(","))) for player in raw.split(";")]
    if any(len(player) != 3 for player in players):
        raise ValueError("Glicko vector players must be rating,rd,volatility")
    return players


def _team_multipliers(raw: str, team_sizes: tuple[int, int]) -> dict[int, float]:
    """Parse per-player values as two ;-separated teams keyed by 1-based ID."""
    if raw == "none":
        return {}
    groups = raw.split(";")
    if len(groups) != len(team_sizes):
        raise ValueError("per-player vector values must contain one group per team")
    multipliers: dict[int, float] = {}
    player_id = 1
    for group, size in zip(groups, team_sizes, strict=True):
        values = [float(value) for value in group.split(",")]
        if len(values) != size:
            raise ValueError("per-player vector values must match the team size")
        for value in values:
            multipliers[player_id] = value
            player_id += 1
    return multipliers


def _pet_evolution_scores(raw: str) -> dict[PetInstinct, int]:
    values = [int(value) for value in raw.split(",")]
    if len(values) != len(PetInstinct):
        raise ValueError("pet evolution vector must contain five instinct scores")
    return dict(zip(PetInstinct, values, strict=True))


class _ScriptedBrawlRng:
    def __init__(self, raw_values: str) -> None:
        self.values = [int(value) for value in raw_values.split(",")]

    def randint(self, low: int, high: int) -> int:
        value = self.values.pop(0)
        if not low <= value <= high:
            raise ValueError(f"scripted brawl RNG value {value} outside [{low}, {high}]")
        return value


def _line_fingerprint(lines: list[str] | tuple[str, ...]) -> str:
    fingerprint = 0xCBF29CE484222325
    for line in lines:
        for byte in line.encode() + b"\n":
            fingerprint ^= byte
            fingerprint = (fingerprint * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{fingerprint:016x}"


def _persona_catalog_fingerprint() -> str:
    fields: list[str] = []
    for roster in (PERSONAS, BETTING_PERSONAS):
        for key, persona in roster.items():
            fields.extend(
                (
                    key,
                    persona.key,
                    persona.name,
                    persona.system_prompt,
                    *persona.examples,
                )
            )
    fingerprint = 0xCBF29CE484222325
    for byte in "\0".join(fields).encode():
        fingerprint ^= byte
        fingerprint = (fingerprint * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{fingerprint:016x}"


def _wheel_catalog_fingerprint(kind: str) -> str:
    wedges = WHEEL_WEDGES if kind == "regular" else GOLDEN_WHEEL_WEDGES
    rows = sorted(
        f"{label}|{value}|{color}"
        for label, value, color in wedges
        if not isinstance(value, int) or value >= 0
    )
    return _line_fingerprint(rows)


def _wrapped_flavor_catalog_fingerprint() -> str:
    return _line_fingerprint(
        [
            f"{key}\0{index}\0{entry}"
            for key, pool in FLAVOR_POOLS.items()
            for index, entry in enumerate(pool)
        ]
    )


def _disburse_embed_fingerprint(arguments: list[str]) -> str:
    fund_amount, even_votes, proportional_votes, quorum_required = map(
        int, arguments
    )
    votes = dict.fromkeys(
        (
            "even",
            "proportional",
            "neediest",
            "stimulus",
            "lottery",
            "social_security",
            "richest",
            "burn",
            "next_match_pot",
            "cancel",
        ),
        0,
    )
    votes["even"] = even_votes
    votes["proportional"] = proportional_votes
    total_votes = sum(votes.values())
    proposal = SimpleNamespace(
        fund_amount=fund_amount,
        votes=votes,
        total_votes=total_votes,
        quorum_required=quorum_required,
        quorum_progress=total_votes / quorum_required,
        quorum_reached=total_votes >= quorum_required,
    )
    embed = build_disburse_embed(proposal)
    rows = [embed.title or "", embed.description or ""]
    rows.extend(
        f"{field.name}\0{field.value}\0{field.inline}" for field in embed.fields
    )
    rows.append(f"footer\0{embed.footer.text or ''}")
    return _line_fingerprint(rows)


class _SqlVectorRepository:
    @staticmethod
    def get_all_tables() -> list[str]:
        return ["players", "player_steam_ids"]

    @staticmethod
    def get_guild_scoped_tables() -> set[str]:
        return {"players"}


def _sql_validation_result(sql: str) -> str:
    service = PythonSQLQueryService(None, _SqlVectorRepository())
    valid, error = service._validate_sql(sql)
    return "ok" if valid else f"error:{error.casefold()}"


def _embed_character(raw: str) -> str:
    return "🦙" if raw == "llama" else raw


def _embed_pack_fingerprint(line_count: int, payload_len: int, max_len: int) -> str:
    import discord

    embed = discord.Embed()
    lines = [f"item-{index:03d}-" + "x" * payload_len for index in range(line_count)]
    add_lines_field(embed, "Stuff", lines, max_len=max_len)
    rows = [f"{field.name}\0{field.value}\0{field.inline}" for field in embed.fields]
    return f"{len(embed.fields)}|{_line_fingerprint(rows)}"


def _embed_validation_result(arguments: list[str]) -> str:
    import discord

    title_len, description_len, field_name_len, field_value_len, footer_len, author_len, field_count = map(
        int, arguments
    )
    embed = discord.Embed(
        title=None if title_len < 0 else "t" * title_len,
        description=None if description_len < 0 else "d" * description_len,
    )
    for _ in range(field_count):
        embed.add_field(
            name="n" * field_name_len,
            value="v" * field_value_len,
            inline=False,
        )
    if footer_len >= 0:
        embed.set_footer(text="f" * footer_len)
    if author_len >= 0:
        embed.set_author(name="a" * author_len)
    return "|".join(validate_embed(embed))


def _cave_in_catalog() -> str:
    rows = []
    for band in ("shallow", "mid", "deep", "endgame"):
        block_min, block_max = CAVE_IN_BLOCK_LOSS_RANGES[band]
        bill_min, bill_max = CAVE_IN_MEDICAL_BILL_RANGES[band]
        weights = ",".join(
            f"{consequence}={weight}"
            for consequence, weight in CAVE_IN_CONSEQUENCE_WEIGHTS[band]
        )
        catastrophic_basis_points = round(
            CAVE_IN_CATASTROPHIC_PCT_BY_BAND[band] * 10_000
        )
        rows.append(
            f"{band}:{block_min}-{block_max}:{bill_min}-{bill_max}:"
            f"{CAVE_IN_STUN_DIGS_BY_BAND[band]}:"
            f"{CAVE_IN_INJURY_DIGS_BY_BAND[band]}:"
            f"{catastrophic_basis_points}:{weights}"
        )
    rows.append(
        "cat:"
        f"{CAVE_IN_CATASTROPHIC_MEDICAL_BILL[0]}-"
        f"{CAVE_IN_CATASTROPHIC_MEDICAL_BILL[1]}:"
        f"{CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE[0]}-"
        f"{CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE[1]}:"
        f"{CAVE_IN_CATASTROPHIC_GEAR_TICKS}:"
        f"{CAVE_IN_CATASTROPHIC_MILESTONE_STEP}"
    )
    return "|".join(rows)


def _evaluate(operation: str, arguments: list[str]) -> object:
    if operation == "normalize_guild_id":
        return normalize_guild_id(_optional_int(arguments[0]))
    if operation == "is_dm_context":
        return is_dm_context(_optional_int(arguments[0]))
    if operation == "select_llm_config":
        model, api_key = _select_llm_config(
            _optional_text(arguments[0]),
            _optional_text(arguments[1]),
            _optional_text(arguments[2]),
        )
        return f"{model}|{api_key or 'none'}"
    if operation == "gamba_bound":
        return bounded_economy_multiplier(float(arguments[0]))
    if operation == "gamba_scale":
        return apply_gamba_event_multiplier(
            int(arguments[0]),
            win_multiplier=float(arguments[1]),
            loss_multiplier=float(arguments[2]),
        )
    if operation == "dig_strengthen_penalty":
        return strengthen_dig_event_penalty(int(arguments[0]))
    if operation == "dig_splash_burn_penalty":
        return economy_scaling.scale_deflationary_minigame_jc_delta(
            strengthen_dig_event_penalty(int(arguments[0]))
        )
    if operation == "dig_phase_event_boss_hp_delta":
        event_id = arguments[0]
        return next(
            event.boss_hp_delta
            for event in PHASE_TRANSITION_EVENTS
            if event.id == event_id
        )
    if operation == "wheel_catalog":
        return _wheel_catalog_fingerprint(arguments[0])
    if operation == "wheel_explosion_reward":
        return WHEEL_EXPLOSION_REWARD
    if operation == "wheel_eruption":
        value = None if arguments[0] == "none" else {"result": int(arguments[0])}
        return eruption_reward(value)
    if operation == "embed_truncate_repeat":
        result = truncate_field(
            _embed_character(arguments[0]) * int(arguments[1]),
            max_len=int(arguments[2]),
        )
        return f"{len(result)}|{str(result.endswith('...')).lower()}|{_line_fingerprint([result])}"
    if operation == "embed_pack_lines":
        return _embed_pack_fingerprint(*map(int, arguments))
    if operation == "embed_validate":
        return _embed_validation_result(arguments)
    if operation == "persona_catalog_fingerprint":
        return _persona_catalog_fingerprint()
    if operation == "wrapped_flavor_catalog_fingerprint":
        return _wrapped_flavor_catalog_fingerprint()
    if operation == "disburse_embed_fingerprint":
        return _disburse_embed_fingerprint(arguments)
    if operation == "sql_validate":
        return _sql_validation_result(arguments[0])
    if operation == "ask_format_no_results":
        return _escape_vector_text(
            QueryResult(success=True, results=[], row_count=0).format_for_discord()
        )
    if operation == "ask_format_failure":
        return _escape_vector_text(
            QueryResult(success=False, error="boom").format_for_discord()
        )
    if operation == "ask_format_single":
        return _escape_vector_text(
            QueryResult(
                success=True,
                results=[{"discord_username": "Solo", "wins": 5}],
                row_count=1,
            ).format_for_discord()
        )
    if operation == "ask_format_multi":
        return _escape_vector_text(
            QueryResult(
                success=True,
                results=[
                    {"discord_username": "A", "wins": 10},
                    {"discord_username": "B", "wins": 7},
                ],
                row_count=2,
            ).format_for_discord()
        )
    if operation == "ask_format_cap":
        return _escape_vector_text(
            QueryResult(
                success=True,
                results=[
                    {"discord_username": f"P{index}", "wins": index}
                    for index in range(12)
                ],
                row_count=12,
            ).format_for_discord()
        )
    if operation == "pet_evolution":
        decision = resolve_evolution(
            _pet_evolution_scores(arguments[1]),
            pet_id=int(arguments[0]),
        )
        return ",".join(
            (
                decision.calling.value,
                decision.primary.value if decision.primary else "none",
                decision.secondary.value if decision.secondary else "none",
            )
        )
    if operation == "pet_evolution_hint":
        return hint_key_for_scores(_pet_evolution_scores(arguments[0])) or "none"
    if operation == "pet_brawl_move_name":
        return move_name(arguments[0], PetBrawlMove(arguments[1]))
    if operation == "pet_brawl_traits":
        traits = brawl_traits(arguments[0])
        return ",".join(
            (
                str(traits.dodge_bonus_pp),
                str(traits.crit_pct),
                str(traits.damage_dealt_bonus),
                str(traits.damage_taken_mod),
                str(traits.heal_bonus),
                str(traits.loss_insurance).lower(),
            )
        )
    if operation == "pet_brawl_duelist":
        duelist = build_duelist(
            pet_id=1,
            owner_id=2,
            name="Vector",
            species_id=arguments[0],
            stage=PetStage(arguments[1]),
            hunger=int(arguments[2]),
            happy=arguments[3] == "true",
            aegis_used=int(arguments[4]),
            training_str=int(arguments[5]),
            training_int=int(arguments[6]),
            training_dex=int(arguments[7]),
        )
        return ",".join(
            (
                duelist.tier,
                str(duelist.max_hp),
                str(duelist.hp),
                str(duelist.power),
                str(duelist.shield_available).lower(),
                str(duelist.training_str),
                str(duelist.training_int),
                str(duelist.training_dex),
            )
        )
    if operation == "pet_brawl_round":
        duelists = []
        for pet_id, owner_id, species_id, name in (
            (1, 10, arguments[0], "A"),
            (2, 20, arguments[1], "B"),
        ):
            duelists.append(
                build_duelist(
                    pet_id=pet_id,
                    owner_id=owner_id,
                    name=name,
                    species_id=species_id,
                    stage=PetStage.ADULT,
                    hunger=100,
                    happy=False,
                    aegis_used=0,
                )
            )
        state = initial_state(
            replace(duelists[0], hp=int(arguments[4])),
            replace(duelists[1], hp=int(arguments[5])),
        )
        state = replace(state, round_no=int(arguments[6]))
        state, log = resolve_pet_brawl_round(
            state,
            PetBrawlMove(arguments[2]),
            PetBrawlMove(arguments[3]),
            _ScriptedBrawlRng(arguments[7]),
        )
        return ",".join(
            (
                str(state.a.hp),
                str(state.b.hp),
                str(state.round_no),
                state.winner or "none",
                str(state.a.shield_available).lower(),
                str(state.b.shield_available).lower(),
                str(len(log)),
                _line_fingerprint(log),
            )
        )
    if operation == "package_deal_cost":
        return _calculate_package_deal_cost(
            int(arguments[0]),
            arguments[1] == "true",
            _optional_float(arguments[2]),
            _optional_float(arguments[3]),
        )
    if operation == "dig_cave_in_band":
        return cave_in_band(int(arguments[0]))
    if operation == "dig_cave_in_catalog":
        return _cave_in_catalog()
    if operation in {"scale_minigame", "scale_deflationary"}:
        amount, scale = map(float, arguments)
        economy_scaling.config.MINIGAME_JC_DELTA_SCALE = scale
        if operation == "scale_minigame":
            return economy_scaling.scale_minigame_jc_delta(amount)
        return economy_scaling.scale_deflationary_minigame_jc_delta(amount)
    if operation == "dig_scale_positive":
        return scale_positive_dig_jc(int(arguments[0]))
    if operation in {
        "dig_wager_taper",
        "dig_log_cap",
        "dig_settled_multiplier",
        "dig_strength_effects",
        "dig_smarts_effect",
        "dig_stamina_effect",
        "dig_wager_skin_bonus",
    }:
        dig_service = object.__new__(DigService)
        if operation == "dig_wager_taper":
            return dig_service._effective_wager_multiplier(
                float(arguments[0]), float(arguments[1])
            )
        if operation == "dig_log_cap":
            return dig_service._log_capped_multiplier(float(arguments[0]))
        if operation == "dig_settled_multiplier":
            return dig_service._settled_wager_multiplier(
                float(arguments[0]), float(arguments[1])
            )
        if operation == "dig_wager_skin_bonus":
            return dig_service._wager_skin_bonus(int(arguments[0]))
        stat_name = (
            operation.removeprefix("dig_")
            .removesuffix("_effects")
            .removesuffix("_effect")
        )
        effects = dig_service._get_stat_effects({stat_name: int(arguments[0])})
        if operation == "dig_strength_effects":
            return f"{effects['advance_min_bonus']},{effects['advance_max_bonus']}"
        if operation == "dig_smarts_effect":
            return effects["cave_in_reduction"]
        return effects["cooldown_multiplier"]
    if operation == "mmr_to_rating":
        return CamaRatingSystem().mmr_to_rating(int(arguments[0]))
    if operation == "new_player_seed_mmr":
        return CamaRatingSystem.new_player_seed_mmr(int(arguments[0]))
    if operation == "streak":
        history = [] if arguments[0] == "-" else [_outcome(item) for item in arguments[0].split(",")]
        won = _outcome(arguments[1])
        rate = None if arguments[2] == "default" else float(arguments[2])
        threshold = None if arguments[3] == "default" else int(arguments[3])
        length, multiplier = CamaRatingSystem().calculate_streak_multiplier(
            history,
            won,
            streak_multiplier_per_game=rate,
            streak_threshold=threshold,
        )
        return f"{length},{_format(multiplier)}"
    if operation == "rd_to_certainty":
        return rd_to_certainty(float(arguments[0]))
    if operation == "rd_tier":
        return get_rd_tier_name(float(arguments[0]))
    if operation == "openskill_display":
        return CamaOpenSkillSystem.mu_to_display(float(arguments[0]))
    if operation == "openskill_fingerprint":
        return CamaOpenSkillSystem.algorithm_fingerprint()
    if operation == "openskill_calibrate":
        return CamaOpenSkillSystem.calibrate_win_probability(
            float(arguments[0]), float(arguments[1])
        )
    if operation == "openskill_predict":
        return CamaOpenSkillSystem().os_predict_win_probability(
            _openskill_team(arguments[0]), _openskill_team(arguments[1])
        )
    if operation == "glicko_match_update":
        system = CamaRatingSystem()
        team1_inputs = _glicko_team(arguments[0])
        team2_inputs = _glicko_team(arguments[1])
        team1 = [
            (system.create_player_from_rating(rating, rd, volatility), index + 1)
            for index, (rating, rd, volatility) in enumerate(team1_inputs)
        ]
        team2 = [
            (
                system.create_player_from_rating(rating, rd, volatility),
                len(team1) + index + 1,
            )
            for index, (rating, rd, volatility) in enumerate(team2_inputs)
        ]
        sizes = (len(team1), len(team2))
        team1_updated, team2_updated = system.update_ratings_after_match(
            team1,
            team2,
            int(arguments[2]),
            streak_multipliers=_team_multipliers(arguments[3], sizes),
            gain_multipliers=_team_multipliers(arguments[4], sizes),
            base_rating_delta_multiplier=_optional_float(arguments[5]),
        )
        return ";".join(
            f"{_format(rating)},{_format(rd)}"
            for rating, rd, _volatility, _player_id in team1_updated + team2_updated
        )
    if operation == "openskill_match_update":
        team1_inputs = _openskill_team(arguments[0])
        team2_inputs = _openskill_team(arguments[1])
        sizes = (len(team1_inputs), len(team2_inputs))
        fantasy = _team_multipliers(arguments[3], sizes)
        team1_data = [
            (index + 1, mu, sigma, fantasy.get(index + 1))
            for index, (mu, sigma) in enumerate(team1_inputs)
        ]
        team2_data = [
            (sizes[0] + index + 1, mu, sigma, fantasy.get(sizes[0] + index + 1))
            for index, (mu, sigma) in enumerate(team2_inputs)
        ]
        results = CamaOpenSkillSystem().update_ratings_after_match(
            team1_data,
            team2_data,
            int(arguments[2]),
            streak_multipliers=_team_multipliers(arguments[4], sizes),
            gain_multipliers=_team_multipliers(arguments[5], sizes),
        )
        return ";".join(
            f"{_format(results[player_id][0])},{_format(results[player_id][1])}"
            for player_id in range(1, sizes[0] + sizes[1] + 1)
        )
    raise ValueError(f"unknown vector operation {operation!r}")


def main() -> None:
    for raw_line in sys.stdin:
        line = raw_line.rstrip("\n")
        if not line or line.lstrip().startswith("#"):
            continue
        vector_id, operation, *arguments = line.split("\t")
        print(f"{vector_id}\t{_format(_evaluate(operation, arguments))}")


if __name__ == "__main__":
    main()
