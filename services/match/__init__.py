"""Focused mixin modules that compose into :class:`~services.match_service.MatchService`.

``services.match_service`` was historically one ~2.2k-line module. The
orchestration logic now lives in cohesive mixins here; ``match_service`` keeps
the constructor, a couple of shared cross-cutting helpers, and composes the
mixins into the public ``MatchService`` class.
"""
