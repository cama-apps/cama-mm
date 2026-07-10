"""Read-only SQLAlchemy mappings for Dotabase's packaged database.

The upstream package eagerly imports an undeclared optional dependency.  This
module deliberately locates its data distribution without importing package
code and reflects only the ORM relationships used by this application.
"""

from importlib.metadata import distribution
from pathlib import Path
from threading import Event, Lock

from sqlalchemy import URL, create_engine
from sqlalchemy.ext.declarative import DeferredReflection
from sqlalchemy.orm import DeclarativeBase, Session, relationship

__all__ = ("Ability", "Hero", "Item", "Response", "dotabase_session")


class _Base(DeclarativeBase):
    pass


class _Reflected(DeferredReflection):
    __abstract__ = True


class Hero(_Reflected, _Base):
    __tablename__ = "heroes"

    abilities = relationship("Ability", order_by="Ability.slot", back_populates="hero")
    talents = relationship("_Talent", order_by="_Talent.slot")
    facets = relationship("_Facet", back_populates="hero")

    def __repr__(self) -> str:
        return f"Hero: {self.localized_name}"


class Ability(_Reflected, _Base):
    __tablename__ = "abilities"

    hero = relationship("Hero", back_populates="abilities")
    talent_links = relationship("_Talent", back_populates="ability")

    @property
    def is_talent(self) -> bool:
        return len(self.talent_links) > 0

    def __repr__(self) -> str:
        return f"Ability: {self.localized_name}"


class _Talent(_Reflected, _Base):
    __tablename__ = "talents"

    ability = relationship("Ability", back_populates="talent_links")

    @property
    def localized_name(self) -> str | None:
        return self.ability.localized_name

    @property
    def level(self) -> int:
        return ((self.slot // 2) * 5) + 10

    @property
    def is_right_side(self) -> bool:
        return (self.slot % 2) == 0

    def __repr__(self) -> str:
        return f"Talent: {self.localized_name}"


class _Facet(_Reflected, _Base):
    __tablename__ = "facets"

    hero = relationship("Hero", back_populates="facets")

    def __repr__(self) -> str:
        return f"Facet: {self.localized_name}"


class Item(_Reflected, _Base):
    __tablename__ = "items"

    def __repr__(self) -> str:
        return f"Item: {self.localized_name}"


class Response(_Reflected, _Base):
    __tablename__ = "responses"

    def __repr__(self) -> str:
        return f"Response: {self.name}"


_DOTABASE_DB = Path(
    distribution("dotabase").locate_file("dotabase/dotabase.db")
).resolve(strict=True)
_ENGINE = create_engine(
    URL.create(
        "sqlite+pysqlite",
        database=f"file:{_DOTABASE_DB.as_posix()}",
        query={"immutable": "1", "mode": "ro", "uri": "true"},
    )
)
_MAPPING_LOCK = Lock()
_MAPPINGS_READY = Event()


def _prepare_mappings() -> None:
    if _MAPPINGS_READY.is_set():
        return

    with _MAPPING_LOCK:
        if _MAPPINGS_READY.is_set():
            return
        _Reflected.prepare(_ENGINE)
        _MAPPINGS_READY.set()


def dotabase_session() -> Session:
    """Return a session over the immutable database bundled by Dotabase."""
    _prepare_mappings()
    return Session(_ENGINE)
