# Steam/Dota/Deadlock generated protocol provenance review

Reviewed 2026-09-11 by Codex (automated source review). Scope is the exact crates.io packages steam-vent-proto-steam 0.5.2 and steam-vent-proto-dota2 0.5.2. This is a reproducible generation and complete structural capability review, not a claim to have manually read approximately 1.2 million generated lines.

## Artifact and provenance evidence

| Package | crates.io archive SHA-256 | Archive files compared | Generated files reproduced |
|---|---|---:|---:|
| steam-vent-proto-steam 0.5.2 | 69b590586d9c9c002f36e7e65d14caaed2301249a7e1badae776a8349ed4a8ce | 222 | 109 |
| steam-vent-proto-dota2 0.5.2 | 42fcc961de3a221252996edf1ebc651bfcc5503a0bcb79d960beedfd291994ed | 165 | 77 |

Every archive file equals its extracted reviewed copy. Cargo.lock uses these archive checksums. Package VCS metadata identifies Steam proto commit 8634a446730e3c24cce72f3086b89ef1570aaba2 (steam subdirectory of https://codeberg.org/steam-vent/proto), and Dota proto commit 3cd36cdeb8e12342221ff3a7c641b59bffdb4761 (https://codeberg.org/steam-vent/proto-dota2).

The Dota package's shipped flake.lock pins https://codeberg.org/steam-vent/steam-vent.git to c03aa0f82cd4a4bccd55de13696f8fd9bffaf2bb. Retrieved that exact upstream archive, read the complete protobuf/build/src/main.rs (414 lines) and kinds.rs (147 lines), and built the generator using its own Cargo.lock. Generator pins protobuf, protobuf-codegen and protobuf-parse to 3.5.1, uses the pure parser, and requests lite_runtime(true). It sorts bundled schema paths before invoking codegen. Its extra code contains schema-derived message kinds, RPC names, trait delegates, module imports, and version constants. It has no runtime side effect injected into the output. The generator is a review aid, not a dependency being certified; its local filesystem operations only occur during explicit generation.

Ran this exact generator against each published package's bundled protos directory. All 109 Steam files and 77 Dota files were byte-for-byte equal to src/generated, with zero missing, extra or differing files. This establishes the published Rust is generated from the published schemas by the pinned generator, including its custom extra implementations. It does not independently authenticate Valve's ownership of every schema.


## Complete structural and handwritten review

Parsed every one of the 189 Rust files (186 generated plus three handwritten) with syn 2.0.79 and traversed imports, paths, calls, methods, macros, attributes, statics, unsafe expressions/implementations/signatures, and foreign items. Parsing succeeded for all files; zero unsafe/foreign anomalies.

All external runtime paths are ordinary std container/default/result/hash operations, caller-supplied std::io::Read and Write traits, and steam-vent-proto-common traits/reexported protobuf operations. Imports are sibling modules and protobuf::Message. No ambient filesystem, network, process, environment, native ABI, assembly, code inclusion, or independent execution capability exists. The only expression macro is panic!, in generated oneof accessors whose exclusive mutable control flow first establishes the matching variant. Generated statics hold default empty message values. Attributes are allow, derive, doc, cfg_attr, and non_exhaustive; there are no constructor/export/link attributes.

Reviewed the entire handwritten entrypoints: Steam reexports generated modules and implements JobMultiple by negating response_pending; Dota reexports modules and defines a handshake with fixed app id 570 and cloned caller-provided hello. Normalized and original manifests contain no build scripts, build dependencies or executable targets; sole runtime dependency is steam-vent-proto-common 0.5.1.

Sampled and reasoned through generator-produced parsing, size/write, accessor, RPC conversion and oneof families. They use safe owned containers, fixed wire tags and runtime parse helpers, preserving unknown fields through the runtime. No independent deserialization implementation, arbitrary code evaluation, global I/O or schema-driven execution is introduced. Read/Write delegates exercise only streams explicitly supplied by callers. Allocation/recursion protections are supplied by the separately reviewed protobuf runtime and transport limits.

## Decision and boundary

The evidence supports package-local safe-to-deploy entries for these two exact generated packages under cargo-vet's built-in criterion (https://mozilla.github.io/cargo-vet/built-in-criteria.html), with scope recorded in audit notes. No exemption is requested. This conclusion does NOT certify transitive protobuf 3.5.1: the runtime has confirmed unknown-group recursion vulnerability RUSTSEC-2024-0437 (https://rustsec.org/advisories/RUSTSEC-2024-0437.html). The [local runtime backport](protobuf-runtime.md) addresses that vulnerability and two additional recursion paths. Audit entries for generated packages do not waive that dependency requirement.

## Reproducing generation

1. Fetch the source archive at Steam generator commit `c03aa0f82cd4a4bccd55de13696f8fd9bffaf2bb` from the linked upstream repository. Preserve its Cargo.lock.
2. Download the two exact crates.io archives above and verify SHA-256 before extracting.
3. Build `protobuf/build/Cargo.toml` from the generator checkout with `cargo build --locked`. The generator accepts two positional arguments: the package's bundled `protos` directory and a fresh output directory.
4. Run it separately for Steam and Dota, then compare every generated file byte-for-byte with each package's `src/generated` directory. Require the same file set as well as identical contents.

The observed result was 109 Steam and 77 Dota generated files, all identical. The generator is explicit review tooling; it is not part of the application's runtime or build dependency graph.

## Deadlock 0.1.0 review — 2026-10-06

Reviewed by Codex (automated source review) using the same provenance-and-generation approach. This is not a claim to have manually read the 320,321 generated lines.

- Package: `steam-vent-proto-deadlock 0.1.0`.
- Exact crates.io archive SHA-256: `6cb90b9f61017aa240eb125a5e89163e984f3990c5e64693ee3191c6d9b99b79`, matching the application lockfile.
- All 109 regular archive files were compared byte-for-byte with the reviewed extracted package; no archive links or special files were accepted.
- Shipped VCS metadata identifies commit `02dadfa0a111e0f42b56cf2b7e53f8117a67c82b` in `https://codeberg.org/newt/proto-deadlock`.
- The shipped `flake.lock` pins the same generator commit `c03aa0f82cd4a4bccd55de13696f8fd9bffaf2bb` used above. The downloaded upstream tarball SHA-256 was `5d89eb8fdd0775898aee30c6a8d53698d8dca0645fa0d970b4e03a0608830a8b`.

Read the complete custom generator (`main.rs` and `kinds.rs`) and rebuilt it with its shipped Cargo.lock, using protobuf/codegen/parser 3.5.1 and the pure parser with lite runtime. Ran it against the package's bundled `protos` directory into a fresh output directory. The generated file sets match exactly: 49 Rust files.

The publication differs from regeneration only in CRLF line endings and this exact extra import in 45 files:

```rust
#[allow(unused_imports)]
use ::steam_vent_proto_common::protobuf as protobuf;
```

Four files match after line-ending normalization alone. Removing this exact import from the other 45 published files, and normalizing line endings, makes every file equal to regenerated output. The alias names the already-declared dependency's protobuf reexport; it neither executes code nor changes the runtime selected by Cargo. No other source difference is present. Reproduction follows the four steps above with the Deadlock archive, followed by this explicitly limited normalization; do not accept arbitrary whitespace or source rewrites.

Read the entire handwritten `src/lib.rs`, `src/handshake.rs`, and normalized/original manifests. The library only reexports generated types and a handshake returning fixed app ID 1422450 plus a clone of the caller-owned hello. Its sole dependency is `steam-vent-proto-common 0.5.1`. There is no build script, executable target, native dependency or additional feature-controlled code.

A temporary Rust review helper parsed all 51 Rust files with syn 2.0.79 and traversed paths, imports, attributes, macros, unsafe expressions/signatures/impls/traits, foreign modules and mutable statics. No unsafe/foreign code or mutable statics were found. Standard-library paths are limited to owned containers, defaults, comparisons, hashing, and caller-supplied `std::io::Read`/`Write`. Imports name sibling modules and the common protobuf dependency. Attributes are ordinary lint allowances, built-in derives, docs, `non_exhaustive`, and a rustfmt-only attribute. The only expression macro is the generated `panic!` accessor pattern. There are no code-inclusion, link/export, constructor, ambient filesystem/network/process/environment, or native ABI capabilities. The scan helper is temporary review tooling, not a production dependency or CI test.

Sampled generated message parsing, field accessors, size/write methods, and RPC delegates against the reviewed generator. They use safe containers and the independently reviewed protobuf runtime; caller-supplied streams do not create independent connections or open files. Allocation and recursion limits remain responsibilities of that runtime and the bounded Steam transport/metadata adapters.

This evidence supports a package-local `safe-to-deploy` audit for this exact version. It adds no exemption, changes no dependency requirement, and does not certify current live Deadlock hosting, spectator permissions, or result availability. Those remain disabled-by-default operational qualification items.
