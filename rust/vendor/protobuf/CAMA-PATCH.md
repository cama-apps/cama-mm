# Reviewed protobuf 3.5.1 security backport

Upstream: https://github.com/stepancheg/rust-protobuf, source commit
`1e1e80adaf1853fde48aba9fc4fc1951d1adeae6` (package VCS metadata).
The crates.io `protobuf-3.5.1.crate` SHA-256 is
`0bcc343da15609eaecd65f8aa76df8dc4209d325131d8219358c0aaaebab0bf6`.
MIT license is retained in LICENSE.txt. This directory contains the archive's
complete runtime source, original/normalized manifests, build script, README,
license and VCS metadata. Repository-only generator tooling and benchmarks are
not needed to build the library.

The deployed package retains version 3.5.1 for the generated Steam/Dota
VERSION_3_5_1 compile-time assertion. It is a reviewed local patched snapshot,
not an assertion that the vulnerable registry release is safe.

## Changes

1. Backport the complete official commit
   https://github.com/stepancheg/rust-protobuf/commit/f06992f46771c0a092593b9ebf7afd48740b3ed6
   fixing RUSTSEC-2024-0437 / CVE-2025-53605. Unknown-group skipping now uses
   the existing recursion budget, and error returns restore the counter.
   The upstream message helper refactor and two upstream tests are retained.
2. Apply the same budget to dynamic binary messages. Previously
   merge_message_dyn skipped the counter used by static messages, so a
   recursive dynamic schema could evade the binary depth limit.
3. Bound nested text-format messages to depth 100. Previously the text
   parser had no depth guard. The parser decrements its count after both
   successful and unsuccessful helper returns.
4. Add regressions exercising unknown groups through a generated message,
   a truly dynamic recursive descriptor, nested text, shallow successful
   parses, and recursion counter restoration on errors.
5. Restore the upstream unit-test hex helper omitted from the crate archive:
   src/test_hex.rs is byte-identical to
   https://raw.githubusercontent.com/stepancheg/rust-protobuf/1e1e80adaf1853fde48aba9fc4fc1951d1adeae6/test-crates/protobuf-test-common/src/hex.rs
   (SHA-256 ed54b5fb2a1b7b02b8454ebdcb7fb8ad9d37dbef9daac1f0f4a363c9ada15873).
   Only its cfg(test) path in lib.rs changes. No runtime dependency is added.

Modified original runtime files: src/coded_input_stream/mod.rs and
src/text_format/parse.rs. The cfg(test) path in src/lib.rs is the only other
modified original file. All other copied source/manifests equal the archive.

## Review of unsafe and capabilities

Codex reviewed every unsafe site and the surrounding constructors/call paths,
as located by full text scan and a syn AST traversal of all 124 Rust files
(including build.rs and the restored test helper). Areas reviewed:

- InputBuf bounds/limit invariants, borrowed buffer lifetime extension,
  stable backing storage in BufReader, fill/consume ordering and exclusive
  caller borrowing; uninitialized byte storage is only exposed as initialized
  after successful complete reads. Large claimed byte lengths use staged
  allocation instead of one allocation from attacker-supplied length.
- OutputBuffer raw views, spare-capacity writes, capacity checks before unchecked
  writes, pointer refresh after reserve/extend/flush, initialized-length updates,
  fixed-width numeric slice reinterpretation, and bounded varint encoders.
- MaybeUninit conversions have initialized-source or complete-read preconditions;
  EnumOrUnknown has transparent i32 layout; Chars validates UTF-8 at every
  public construction path before unchecked conversion.
- Any/TypeId checks precede reflection downcasts. OwningRef retains an Arc or
  static owner and its Send/Sync bounds cover both owner and pointee; exposed
  mappings retain borrows tied to the owner.
- The build script reads only Cargo-provided RUSTC, package-version, OUT_DIR and
  feature metadata, invokes that compiler with --version, and writes version.rs
  under OUT_DIR. Its sole include! consumes that generated constant file.
  It does not contact the network or inspect application secrets.
- Runtime external capabilities are explicit caller-provided Read/BufRead/Write
  streams, ordinary memory containers/reflection, clock value conversions, and
  dependency lexer/formatting helpers. No runtime filesystem, process, network
  or environment access was found.

This is a bounded unsafe/capability and parser-boundary review of the complete
snapshot, with targeted logic review and regressions; it is not a claim of
exhaustive review of every generated descriptor or proof against every future
Rust memory-model interpretation. Transitive dependencies retain their own
review requirements. Applications must still bound total input/output volumes.
All patched parsing paths reject excessive nesting before recursively allocating
unbounded message trees. Caller-constructed arbitrarily deep in-memory trees
and unlimited caller streams are not created autonomously by this package.

## Validation

`cargo test --locked --offline --manifest-path rust/Cargo.toml -p protobuf --lib`:
69 passed, zero failed or ignored. A temporary harness enabling with-bytes also
ran the complete 69-test package suite successfully. No Steam or other production
connection was made. The retained test commands reproduce these checks. Patched file hashes are
recorded in ../../supply-chain/reviews/vendor-files.json. The repository's dependency policy and
snapshot verification cover this local source rather than granting an exemption
to crates.io protobuf 3.5.1.
