# Dependency delta review evidence

Completed automated source reviews of six exact dependency deltas. These are delta attestations relying on the existing imported audit chains, not full fresh reviews of unchanged upstream code or guarantees of absence of all bugs.

Archive provenance: target SHA256 checked against the PR Cargo.lock; baseline SHA256 checked against the crates.io sparse registry. Each archive was extracted separately, and full recursive file differences were generated. All compiled-source/manifests and changed tests were read. Packaged Cargo.lock changes were treated as publication metadata (dependency lockfiles are not used when these crates are dependencies); dependency audit coverage remains separate.

## async-stream: 0.3.5 -> 0.3.6

Reviewed every source/manifest delta against the Google-audited baseline. Sender PhantomData changes to fn(T)->T, making T invariant and preventing leaked borrowed yielded values. The thread-local store initialization becomes const without changing pointer lifecycle. Read the whole yielder module to verify the unsafe store dereference and RAII restoration remain coupled to the macro-generated stream. No new build script, filesystem, network, process, or environment access. Companion async-stream-impl 0.3.6 reviewed separately. Compile-fail reproductions verify the lifetime and nested-async soundness bugs are rejected; nested streams of different item types pass.

- Baseline SHA256: `cd56dd203fef61ac097dd65721a419ddccb106b2d2b70ba60a6b529f03961a51`
- Target SHA256: `0b5a71a6f37880a80d1d7f19efd781e4b5de42c88f0722cc13bcb6cc2cfe8476`

## async-stream-impl: 0.3.5 -> 0.3.6

Reviewed every source/manifest delta against the Google-audited baseline. Generated yield expressions now carry an unreachable break to a hygienic label surrounding their own async-stream body. Rust rejects the label through nested async/closure/function boundaries, preventing writes through a different typed thread-local receiver. Existing unsafe pair construction and for-await pinning are unchanged. The other code change removes an unnecessary mutable iterator reference when emitting diagnostics. No build scripts or ambient I/O. Both historical unsoundness compile-fail reproductions reject correctly; normal nested streams run successfully. proc-macro2 minimum version update requires independent dependency coverage.

- Baseline SHA256: `16e62a023e7c117e27523144c5d2459f4397fcc3cab0085af8e2224f643a0193`
- Target SHA256: `c7c24de15d275a1ecfd47a380fb4d5ec9bfe0933f309ed5e705b775596a3574d`

## bytemuck: 1.25.0 -> 1.25.2

Reviewed every source/manifest delta against the Google/toolkit audited chain ending at 1.25.0. New unsafe NoUninit implementations cover arrays of char, bool, and each NonZero integer type only: these elements already satisfy NoUninit, and arrays add no inter-element or tail padding beyond their element layout; zero-length arrays are also inhabited and padding-free. These impls do not assert AnyBitPattern/Zeroable. Remaining runtime changes only omit core::error::Error on SPIR-V. Empty-slice alignment change is documentation plus regression test; rejection confirmed locally. No new build script or ambient I/O. bytemuck_derive is independently audited.

- Baseline SHA256: `c8efb64bd706a16a1bdde310ae86b351e4d21550d98d056f22f8a7f7a2183fec`
- Target SHA256: `95832e849adfb21180ccb6826a99da14e5d266ae5c2e668e1602cf234f153797`

## bytemuck_derive: 1.9.3 -> 1.12.0

Reviewed all source/manifest changes against the Google/Mozilla audited chain ending at 1.9.3. New NoUninit enum derivation validates every field and requires each variant field-size sum plus discriminant size to equal the whole enum size, rejecting internal/tail padding and unequal variants. Struct padding validation uses an evaluated const assertion instead of an unused transmute expression; transparent and packed(1) generic structs retain field trait checks and compiler layout guarantees. C-enum discriminant size is obtained from a same-variant fieldless enum, replacing an assumed c_int tag; bit-pattern union payload access remains guarded by discriminant checks. TransparentWrapper parsing accepts full types but still requires one token-matched wrapped field and compile-time zero-size/alignment plus Zeroable checks on extras. No build script or ambient I/O. Local compile-fail tests reject padded structs/enums, unequal enum variants, and extra transparent data; valid enum/array casts and invalid tag rejection pass. syn3 dependency needs its own coverage; this review does not certify syn.

- Baseline SHA256: `7ecc273b49b3205b83d648f0690daa588925572cc5063745bfe547fe7ec8e1a1`
- Target SHA256: `fc0e56a716f1e132ff6bf4bdac1c944a3fcdc1cae65f70a4a2a1ac3b401d2d1f`

## tokio-stream: 0.1.17 -> 0.1.19

Reviewed every compiled-source and manifest delta against the Google/Zcash audited chain ending at 0.1.17, including new collection and JoinSet wrappers. FusedStream implementations follow retained state or fused inner streams; map_while now latches termination. Peekable and StreamMap hints use saturating lower bounds and checked upper bounds; StreamMap next_many enforces its caller-provided limit. Collection extensions use standard collections, caller stream size hints, and mem::take with no new unsafe memory operations. Receiver size hints account for held permits; JoinSetStream only polls the caller-owned JoinSet. Cooperative budgeting delegates to Tokio under rt and retains bounded fallback polling otherwise. No new unsafe blocks, build script, autonomous network/filesystem/process behavior; existing wrappers require caller-owned resources. Runtime regressions for map_while termination, next_many limit, and hint overflow pass. Unbounded collecting still requires caller resource limits as before.

- Baseline SHA256: `eca58d7bba4a75707817a2c44174253f9236b2d5fbd055602e9d5c07c139a047`
- Target SHA256: `a3d06f0b082ba57c26b79407372e57cf2a1e28124f78e9479fe80322cf53420b`

## serde_path_to_error: 0.1.11 -> 0.1.20

Reviewed every compiled-source and manifest delta against Mozilla audit 0.1.11. Changes move imports to core/alloc and serde_core, unconditionally delegate 128-bit Serde methods, format numeric/bool map keys using itoa and bounded scalar strings, expose Segment Display, and make Track::new const. Numeric keys are copied before delegation; error-chain ownership/lifetimes remain safe Rust. No unsafe code, build script, network/filesystem/process/environment access was introduced. Allocation for path keys remains proportional to supplied input, with the existing deserializer responsible for overall depth/size limits. Malformed nested numeric-key input reports the expected error path in local regression test. New itoa/serde_core dependencies need independent coverage.

- Baseline SHA256: `f7f05c1d5476066defcdfacce1f52fc3cae3af1d3089727100c02ae92e5abbe0`
- Target SHA256: `10a9ff822e371bb5403e391ecd83e182e0e77ba7f6fe0160b795797109d1b457`

## Validation

A standalone offline harness passed five runtime tests. Six adversarial compile-fail cases rejected padded structs, unequal or padded enum variants, extra transparent-wrapper data, escaping async-stream lifetimes, and nested async scope escapes. Expected compiler diagnostics were checked. The temporary harness was a review aid; it is not a retained CI target. The deployed versions and baseline-to-target audit chains are recorded in [audits.toml](../audits.toml).
