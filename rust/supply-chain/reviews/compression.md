# Compression source review and patched snapshots

Reviewed 2026-09-11 for the Cama production dependency graph. The two registry
attestations are in [audits.toml](../audits.toml). The three vulnerable
registry wrappers are deliberately not given audit entries: their patched local
snapshots are reviewed local dependencies, with details in each CAMA-PATCH.md.

## Provenance

[compression-archives.json](compression-archives.json) records exact archive SHA-256 and
complete extracted-file comparison for all five packages. Every file matched.

| Package | Version | Files verified |
|---|---|---:|
| bzip2 | 0.6.1 | 18 |
| libbz2-rs-sys | 0.2.5 | 17 |
| zstd | 0.13.3 | 35 |
| zstd-safe | 7.3.0 | 13 |
| zstd-sys | 2.1.0+zstd.1.5.7 | 129 |

All 104 bundled native zstd source/build/license files match upstream v1.5.7
byte-for-byte ([zstd-source-provenance.json](zstd-source-provenance.json)), downloaded from
https://codeload.github.com/facebook/zstd/tar.gz/refs/tags/v1.5.7 . Release page:
https://github.com/facebook/zstd/releases/tag/v1.5.7 (commit prefix f8745da).
GitHub displays a verified release commit; independent signature verification is
not claimed. No earlier full native safe-to-deploy audit was found among the
consulted audit datasets; an old safe-to-run or partial delta was not substituted.

## Findings fixed locally

1. zstd-safe tied retained native prepared dictionary pointers to dictionary byte
   lifetimes instead of the dictionary owner's lifetime. zstd stream constructors
   propagated that mistake. A safe high-level counterexample compiles against the
   original registry graph and fails
   with E0505 against the patch. Four new
   compile-fail doctests cover low/high-level encoder and decoder retention.
2. zstd-safe OutBuffer::around_pos could expose uninitialized Vec capacity as an
   initialized prefix when native streaming preserved a nonzero output cursor.
   The patch initializes any newly exposed prefix to zero before crossing FFI.
3. zstd-safe public InBuffer fields allowed an out-of-range cursor to bypass
   set_pos validation. The native stream entry forms pointers before its own
   bounds check; the wrapper now rejects invalid cursors before FFI.
4. libbz2-rs-sys's pending RLE output path performed zero-count Rust pointer
   intrinsics on a null pointer permitted by the raw ABI. It now skips these
   operations when their bound is zero. Safe bzip2 slices supply nonnull pointers.
   This is a demonstrated contract violation, not a claimed sanitizer crash.

## Reviewed boundaries and deployment judgment

The entire bzip2 wrapper runtime was read: allocation and stable stream ownership,
FFI slice borrowing/nonoverlap and u32 lengths, initialized output length updates,
End destruction, Send/Sync, reader and writer progress/error behavior. No ambient
capabilities or build script. The Write adapter can stall if an underlying writer
repeatedly returns Ok(0); production uses bounded Read decompression and this is
not an attacker-controlled writer capability. libbz2's compiled rust-allocator
boundary was reviewed through allocation layouts, raw state initialization and
validation, indexed storage, compression/decompression accounting, RLE and cleanup.
The main algorithm modules forbid unsafe. Optional stdio/C allocator code is not
within the deployed configuration. ISRG's 0.1.1 review is historical corroboration,
not a claim of full delta coverage to 0.2.5.

The zstd-safe safe API/FFI methods and zstd runtime wrappers were examined for
owned context allocation, dictionaries/prefixes, streaming cursor/initialization,
output capacity, and native error conversion. Their concrete unsoundness findings
are fixed above. Original registry versions are not certified.

For zstd-sys, read the complete build.rs and entrypoint/feature selection: local
bundled sources, target compiler selection, local OUT_DIR generation/copying;
optional pkg-config/bindgen behavior is conventional and disabled in production.
Native runtime source was scanned for network/process/environment/file capabilities;
its normal operations are compression, decompression and memory allocation.

Native manual review concentrated on hostile compressed input and allocation:

- zstd_decompress.c: frame header minimum sizes, reserved bits and size arithmetic;
  window limits before allocation; streaming header accumulation, input/output
  bounds, allocation failure checks, block/flush progress and no-progress limits.
- zstd_decompress_block.c: compressed block size cap, literal modes and compressed
  span bounds, output-space constraints, literal allocation/split buffers, sequence
  headers and copy routines' destination/history/offset checks, overlap handling,
  slow edge paths and fast wild-copy margins.
- huf_decompress.c: bounded workspace/table construction, four-stream minimum and
  jump-table checks, minimum bytes before machine-word reads, output segment
  bounds and final consumption validation. Fast/assembly dispatch boundary was
  reviewed; every assembly instruction was not manually audited.
- fse_decompress.c: table log/symbol/workspace restrictions, normalized-count
  parse error propagation, bounded table construction and bitstream/output tail
  checks; supporting bounded bitstream calls were followed at those boundaries.

The package-level safe-to-deploy judgment uses this concrete security review,
source provenance and testing together. It does not claim a line-by-line review
of all native algorithms, a mathematical memory-safety proof, or exhaustive fuzzing.
The raw native API remains unsafe: callers must honor pointer/size contracts.
Production's patched wrapper and metadata path additionally cap window_log_max(26)
(64 MiB), downloaded bytes and decompressed output, and use single-frame decoding.
Features beyond std-only native zstd and rust-allocator-only libbz2 need renewed
review. This scope is recorded in the audit notes and enforced by parent feature
and reviewed-file guards.

## Validation

All passed against the locked workspace graph:

```
cargo test --locked --offline --manifest-path rust/Cargo.toml -p zstd-safe --lib
cargo test --locked --offline --manifest-path rust/Cargo.toml -p zstd-safe --doc
cargo test --locked --offline --manifest-path rust/Cargo.toml -p zstd --doc
cargo test --locked --offline --manifest-path rust/Cargo.toml -p libbz2-rs-sys --lib
```

Results: zstd-safe 7 units and 2 doctests; zstd 4 doctests; libbz2 2 units.
These commands are included in the CI dependency regression script.
The upstream zstd full unit suite depends on assets/nested repository fixtures
and extra dev dependencies omitted from the minimal runtime snapshot; no full
upstream unit-suite pass is claimed. Original unit sources remain available.

Built exact upstream v1.5.7 regression/fuzzer with GCC:

```
make -j2 -C zstd-1.5.7/tests fuzzer \
  CFLAGS='-O1 -g -fsanitize=address,undefined -fno-omit-frame-pointer' \
  LDFLAGS='-fsanitize=address,undefined'
ASAN_OPTIONS=detect_leaks=0:abort_on_error=1 UBSAN_OPTIONS=halt_on_error=1 \
  zstd-1.5.7/tests/fuzzer --no-big-tests -i200 -s669
```

Native regression cases and 200 seeded randomized trials passed with no ASan or
UBSan finding. The first run completed tests then
LeakSanitizer failed because ptrace is unsupported; rerun explicitly disabled
leak detection. No leak-safety claim is made. Official ongoing OSS-Fuzz sanitizer
configuration inspected at
https://raw.githubusercontent.com/google/oss-fuzz/master/projects/zstd/project.yaml
(address, memory, undefined; libFuzzer/AFL/Honggfuzz; x86_64/i386).

No Steam session or production service was contacted. Security review and native
fuzz tests used only local source/data.
