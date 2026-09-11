# Reviewed local zstd-safe 7.3.0 snapshot

Source: crates.io zstd-safe 7.3.0, archive SHA-256
`64d80649ab6db9d9f6f9c80a40becd948eda4714a0a5ac8c4d157a32231c7882`.
Original license and manifest retained. This local snapshot is a reviewed local
dependency; it is not an audit of the unpatched registry release.

Security changes:

- Tie `ref_cdict`, `ref_ddict`, and deprecated experimental dictionary initializers
  to the lifetime of the prepared dictionary owner, not just its underlying byte
  contents. Native contexts retain a pointer to that owner. Previously safe Rust
  could drop the prepared dictionary before another context operation.
- Initialize the unwritten prefix when `OutBuffer::around_pos` exposes vector
  capacity beyond its initialized length. Native completion may preserve the
  cursor without writing any bytes; subsequent vector length updates must never
  expose uninitialized storage.
- Validate public `InBuffer.pos` before constructing the native input buffer.
  Struct literals can bypass `set_pos`; the native streaming implementation forms
  pointers before rejecting an invalid position.

Reviewed the safe API/FFI boundary, output initialization and capacity contracts,
context allocation/free and borrowing, dictionary/prefix retention, streaming
buffer guards, and build feature forwarding. Production uses std without
experimental, legacy, dictionary-builder, seekable, or threading features.
Features outside this reviewed production configuration require renewed review.

Validation from the parent workspace (no added development dependencies):

```
cargo test --locked --offline --manifest-path rust/Cargo.toml -p zstd-safe --lib
cargo test --locked --offline --manifest-path rust/Cargo.toml -p zstd-safe --doc
```

Seven unit tests and two compile-fail dictionary-lifetime doctests pass. The new
unit regressions exercise the zero-filled prefix, empty decoding with a nonzero
cursor, and a forged out-of-range public input cursor. The existing dictionary
training test and its fixture constant now require `zdict_builder`, matching the
availability of the API being tested. Other upstream tests are retained.
