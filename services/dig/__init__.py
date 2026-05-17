"""Focused mixin modules that compose into :class:`~services.dig_service.DigService`.

``services.dig_service`` was historically one ~9k-line module. The game
logic now lives in cohesive mixins here; ``dig_service`` keeps the
constructor, the ``dig`` / ``_compute_preconditions`` entrypoints, and a
handful of cross-cutting helpers, and composes the mixins into the public
``DigService`` class.
"""
