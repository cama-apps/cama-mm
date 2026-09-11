# Dota dependency review

The original CI failure exposed missing audit evidence for the Steam/Dota dependency graph. The graph has now been reduced and reviewed: duplicate HTTP/TLS/random-platform dependencies were removed, generated Steam/Dota code was reproduced, and concrete findings were fixed in explicitly vendored source. No new cargo-vet exemptions were added.

The [full review record](rust/supply-chain/reviews/pr669.md) links exact source provenance, reviewed deltas, security patches, regression evidence and deployment restrictions. Versioned audit evidence lives in [audits.toml](rust/supply-chain/audits.toml); the exact graph is [Cargo.lock](rust/Cargo.lock).

RSA retains a narrowly defined public-key-encryption criterion because its private-key timing vulnerability is not fixed by this work. Feature, consumer and reviewed-source checks enforce this and the other documented usage restrictions; a path dependency alone is not an audit bypass.

**Local checks pass:** locked cargo-vet, no new exemptions, reviewed source/usage enforcement, dependency regressions, and full-workspace formatting/Clippy/tests. The suite reports 9,562 passed and zero failures. The [review record](rust/supply-chain/reviews/pr669.md) documents scope and limitations; GitHub checks validate the pushed commit separately.
