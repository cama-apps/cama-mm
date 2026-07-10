"""Behavioral coverage for the read-only Dotabase ORM gateway."""

import subprocess
import sys
import textwrap
from pathlib import Path


def test_fresh_process_queries_bundled_database_without_importing_dotabase():
    repo_root = Path(__file__).resolve().parents[1]
    script = textwrap.dedent(
        """
        import sys

        from sqlalchemy.orm import Session, joinedload

        from dotabase_integration import Ability, Hero, Item, Response, dotabase_session

        forbidden = [
            name
            for name in sys.modules
            if name == "dotabase" or name.startswith("dotabase.")
        ]
        assert not forbidden, forbidden

        session = dotabase_session()
        try:
            assert isinstance(session, Session)
            assert len(session.query(Hero).all()) > 100

            pudge = (
                session.query(Hero)
                .options(
                    joinedload(Hero.abilities),
                    joinedload(Hero.talents),
                    joinedload(Hero.facets),
                )
                .filter(Hero.localized_name == "Pudge")
                .one()
            )
            meat_hook = next(
                ability
                for ability in pudge.abilities
                if ability.localized_name == "Meat Hook"
            )
            assert meat_hook.hero is pudge
            assert not meat_hook.is_talent
            assert len(pudge.talents) >= 8
            assert {talent.level for talent in pudge.talents} == {10, 15, 20, 25}
            assert {talent.is_right_side for talent in pudge.talents} == {False, True}
            assert all(talent.localized_name for talent in pudge.talents)
            assert all(talent.ability.is_talent for talent in pudge.talents)
            assert isinstance(pudge.facets, list)

            witch_doctor = (
                session.query(Hero)
                .options(joinedload(Hero.facets))
                .filter(Hero.localized_name == "Witch Doctor")
                .one()
            )
            assert len(witch_doctor.facets) >= 2
            assert all(facet.description for facet in witch_doctor.facets)

            assert session.query(Ability).filter(Ability.id == meat_hook.id).one() is meat_hook
            assert session.query(Item).filter(Item.localized_name == "Blink Dagger").one()
            assert (
                session.query(Response)
                .filter(Response.hero_id == pudge.id, Response.text_simple.isnot(None))
                .first()
                is not None
            )
        finally:
            session.close()

        forbidden = [
            name
            for name in sys.modules
            if name == "dotabase" or name.startswith("dotabase.")
        ]
        assert not forbidden, forbidden
        """
    )

    completed = subprocess.run(
        [sys.executable, "-c", script],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=False,
        timeout=120,
    )

    assert completed.returncode == 0, (
        "Fresh Dotabase integration query failed.\n"
        f"stdout:\n{completed.stdout}\n"
        f"stderr:\n{completed.stderr}"
    )
