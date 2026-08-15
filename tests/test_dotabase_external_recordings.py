"""Offline replay of the sanitized Dotabase adapter contract.

The SQL fixture is deliberately minimized and synthetic. It is checked in to
exercise the same public dotabase_session API as the packaged asset, but
it is not captured production data and does not close the external-service
readiness gate.
"""

import hashlib
import json
import sqlite3
import subprocess
import sys
from pathlib import Path

FIXTURE_DIR = (
    Path(__file__).resolve().parents[1]
    / "rust"
    / "crates"
    / "cama-app"
    / "tests"
    / "fixtures"
    / "dotabase"
)
MANIFEST_PATH = FIXTURE_DIR / "manifest.json"
CATALOG_PATH = FIXTURE_DIR / "sanitized_catalog.sql"


def _materialize_catalog(path: Path) -> None:
    with sqlite3.connect(path) as connection:
        connection.executescript(CATALOG_PATH.read_text())


def _run_fresh_process(database_path: Path, script: str) -> subprocess.CompletedProcess[str]:
    bootstrap = f"""
import importlib.metadata
from pathlib import Path

class _FixtureDistribution:
    def locate_file(self, relative_path):
        assert str(relative_path) == "dotabase/dotabase.db"
        return Path({str(database_path)!r})

_original_distribution = importlib.metadata.distribution

def _fixture_distribution(name):
    if name == "dotabase":
        return _FixtureDistribution()
    return _original_distribution(name)

importlib.metadata.distribution = _fixture_distribution
"""
    return subprocess.run(
        [sys.executable, "-c", bootstrap + script],
        cwd=Path(__file__).resolve().parents[1],
        capture_output=True,
        text=True,
        check=False,
        timeout=120,
    )


def test_dotabase_fixture_manifest_is_hashed_and_redacted():
    manifest = json.loads(MANIFEST_PATH.read_text())
    assert manifest["schema"] == "cama.external-service-recording/v1"
    assert manifest["provider"] == "dotabase_sqlite"
    assert manifest["schema_version"] == "dotabase-compatible-7.8.11"
    assert manifest["provenance"]["live_network_used"] is False
    manifest_text = MANIFEST_PATH.read_text()
    assert "sanitized_contract_fixture" in manifest_text
    assert "Authorization" not in manifest_text
    assert "Bearer" not in manifest_text

    body = CATALOG_PATH.read_bytes()
    success = manifest["contracts"][0]
    assert success["body"] == "sanitized_catalog.sql"
    assert success["sha256"] == hashlib.sha256(body).hexdigest()
    assert {contract["id"] for contract in manifest["contracts"]} == {
        "catalog-success",
        "missing-asset",
        "schema-mismatch",
    }


def test_dotabase_success_recording_replays_through_python_public_api(tmp_path):
    database_path = tmp_path / "dotabase.db"
    _materialize_catalog(database_path)
    original = database_path.read_bytes()
    completed = _run_fresh_process(
        database_path,
        """
from dotabase_integration import Ability, Hero, Item, Response, dotabase_session
from sqlalchemy.orm import joinedload

session = dotabase_session()
try:
    anti_mage = (
        session.query(Hero)
        .options(
            joinedload(Hero.abilities),
            joinedload(Hero.talents),
            joinedload(Hero.facets),
        )
        .filter(Hero.localized_name == "Anti-Mage")
        .one()
    )
    blink = next(ability for ability in anti_mage.abilities if ability.localized_name == "Blink")
    assert blink.hero is anti_mage
    assert not blink.is_talent
    assert anti_mage.talents[0].localized_name == "Hidden Talent"
    assert anti_mage.facets[0].description == "Reflects a spell."
    assert len(
        session.query(Hero)
        .filter(Hero.localized_name == "Pudge")
        .one()
        .talents
    ) == 8
    assert session.query(Ability).filter(Ability.id == blink.id).one() is blink
    assert session.query(Item).filter(Item.localized_name == "Blink Dagger").one()
    assert session.query(Response).filter(Response.hero_id == anti_mage.id).first()
finally:
    session.close()
""",
    )
    assert completed.returncode == 0, (
        "Python Dotabase fixture replay failed.\n"
        f"stdout:\n{completed.stdout}\n"
        f"stderr:\n{completed.stderr}"
    )
    assert database_path.read_bytes() == original


def test_dotabase_missing_asset_recording_fails_without_creating_database(tmp_path):
    database_path = tmp_path / "missing-dotabase.db"
    completed = _run_fresh_process(
        database_path,
        """
try:
    from dotabase_integration import Hero, dotabase_session
    dotabase_session().query(Hero).count()
except Exception as error:
    assert "no such file" in str(error).lower() or "unable to open database file" in str(error).lower()
else:
    raise AssertionError("missing Dotabase asset must fail closed")
""",
    )
    assert completed.returncode == 0, (
        "Python missing-asset replay failed.\n"
        f"stdout:\n{completed.stdout}\n"
        f"stderr:\n{completed.stderr}"
    )
    assert not database_path.exists()


def test_dotabase_schema_mismatch_recording_fails_closed(tmp_path):
    database_path = tmp_path / "incomplete-dotabase.db"
    with sqlite3.connect(database_path) as connection:
        connection.executescript(
            """
            PRAGMA user_version = 70000;
            CREATE TABLE heroes (id INTEGER PRIMARY KEY, localized_name TEXT);
            """
        )
    completed = _run_fresh_process(
        database_path,
        """
from dotabase_integration import Hero, dotabase_session

try:
    dotabase_session().query(Hero).count()
except Exception as error:
    assert (
        "no such column" in str(error).lower()
        or "requested table(s) not available" in str(error).lower()
    )
else:
    raise AssertionError("incomplete Dotabase schema must fail closed")
""",
    )
    assert completed.returncode == 0, (
        "Python schema-mismatch replay failed.\n"
        f"stdout:\n{completed.stdout}\n"
        f"stderr:\n{completed.stderr}"
    )
    assert database_path.exists()
