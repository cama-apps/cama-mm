# Infrastructure and macro dependency reviews

Automated source reviews of exact archives, verified against Cargo.lock before inspection. Versioned attestations and checksums are in [audits.toml](../audits.toml). The restrictions below require the [feature, consumer, and source guards](pr669.md#required-scope-and-source-guards); they are not unrestricted assurances about excluded APIs.

| Package | Review and capability boundary |
|---|---|
| proc-macro-crate 3.5.0 | Complete library/manifests read. No unsafe or build script. Reads Cargo manifests and metadata from the trusted build environment; invokes the Cargo-selected executable as `locate-project --workspace --message-format=plain` with a separate manifest argument. No shell, network, mutation, or secret discovery. Cache uses a mutex and manifest timestamps. |
| winnow 1.0.4 | Unsafe operations, transparent byte/string wrappers, slicing contracts, and parser progress checked. Explicit unsafe slice methods require valid bounds/UTF-8; safe parsers use checked operations. Repetition rejects parsers that make no progress. Optional debug writes caller diagnostics, not autonomous network/filesystem access. Grammar depth/backtracking remain caller budgets. |
| toml_edit 0.25.14+spec-1.1.0 | In-memory parsing with normal recursion guard depth 80, bounded dotted paths, checked scalar conversion, and no unsafe code or ambient I/O. The `unbounded` feature is excluded. |
| toml_parser 1.1.3+spec-1.1.0 | Normal configuration forbids unsafe code and uses checked source bounds. The optional `unsafe` feature is excluded: safe receiver methods can otherwise forward caller-invalid spans to unchecked slicing. This review does not certify that configuration. |
| crossbeam-utils 0.8.23 | Restricted to `CachePadded`. Its Send/Sync bounds follow the owned value, and access/drop use safe fields. Build script only reads Cargo target/sanitizer metadata and emits cfg directives. `AtomicCell` and other concurrency APIs are excluded: optimistic volatile reads racing a writer remain a Rust memory-model concern. Consumer and source guards must preserve this restriction. |
| binrw / binrw_derive 0.15.2 | Complete manifests/build scripts, unsafe/capability inventory, collection/primitive helpers, macro entrypoints and generated templates reviewed. No runtime unsafe or ambient I/O; stream methods use caller-supplied Read/Write/Seek. Build scripts invoke trusted RUSTC with literal `--version`; macros transform trusted Rust schemas, not network-supplied code. Count-based allocation remains a schema/caller obligation; patched Steam framing validates its peer sizes first. |

## DashMap patch

The complete runtime/manifest delta from 5.5.3 to 6.2.1 was reviewed, including entry slots, table reallocation, borrowed/owned iteration, Rayon, retain, and panic handling. Raw entries and iterators retain the relevant shard guards; each mutable bucket is yielded once. In-place replacement keeps its abort guard armed until the replacement is written. There is no added ambient I/O or build script.

Review found preexisting generic Send/Sync bounds that accepted values which were Sync without being Send, or shared iterators over maps not proven Sync. The [local patch](../../vendor/dashmap/CAMA-PATCH.md) strengthens those bounds without changing synchronization algorithms. Five adversarial compile-only cases accepted upstream are rejected after the patch; ordinary Send+Sync cases remain accepted. The retained [rustdoc cases](../../vendor/dashmap/CAMA-TRAIT-TESTS.md) never execute unsound code. This is reviewed local source, not a safe-to-deploy attestation for unpatched 6.2.1.

## Validation

A standalone offline infrastructure harness passed six tests, including renamed/missing Cargo dependencies; UTF-8 and arbitrary byte wrappers; retained mutable iterator values; CachePadded ownership/alignment/drop; malformed TOML Unicode/numeric/depth cases; and 10,000 deterministic malformed inputs. Optional unsafe TOML behavior was investigated without executing undefined behavior. The temporary harness is review evidence, not a retained CI target. Executable vendor regressions are linked from the [overview](pr669.md).
