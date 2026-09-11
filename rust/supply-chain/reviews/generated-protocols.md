# Steam/Dota generated protocol provenance review

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
