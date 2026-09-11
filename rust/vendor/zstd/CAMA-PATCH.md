# Reviewed local zstd 0.13.3 snapshot

Source: crates.io zstd 0.13.3, archive SHA-256
`e91ee311a569c327171651566e07972200e76fcfe2242a4fa446149a3881c08a`.
Original license and Cargo.toml.orig retained. This local snapshot is a reviewed
local dependency; it is not an audit of the unpatched registry release.

Propagate the fixed zstd-safe prepared-dictionary owner lifetime through raw,
Read, and Write stream encoder/decoder constructors. Bulk constructors already
borrowed the owner correctly and are unchanged. Two raw-stream compile-fail
doctests verify that dropping the owner before operating on either encoder or
decoder is rejected. The original high-level encoder counterexample compiled
against the registry versions; patched compilation rejects it with E0505.

Reviewed runtime stream/bulk/dictionary APIs and their use of the separately
reviewed zstd-safe boundary. Production enables no optional zstd features.
No ambient filesystem, network, environment, or process operation is introduced.

The minimal snapshot retains runtime sources and their original unit test code.
The normalized manifest omits published example/integration-test target entries
and development dependencies used by those targets and upstream test fixtures.
The upstream full unit suite requires assets and a nested repository layout not
present in this runtime snapshot; it is not represented as having passed.
The checked-in doctests and zstd-safe regression suite need no extra dev graph:

```
cargo test --locked --offline --manifest-path rust/Cargo.toml -p zstd --doc
```

Four doctests pass, including both new compile-fail cases. Application metadata
round-trip/malformed-input tests exercise this wrapper with the production graph.
