# Reviewed local libbz2-rs-sys 0.2.5 snapshot

Source: crates.io libbz2-rs-sys 0.2.5, archive SHA-256
`34b357333733e8260735ba5894eb928c02ecc69c78715f01a8019e7fa7f2db4c`.
Original bzip2 license and manifests retained. This local snapshot is a reviewed
local dependency; it is not an audit of the unpatched registry release.

The raw decompression ABI permits a null output pointer when available output is
zero. A pending run could call Rust `ptr::write_bytes` and pointer `add` with that
null pointer even when the byte count was zero. These intrinsics still require
a valid nonnull pointer. Skip the pointer operations when their bound is zero.
This is a Rust contract defect, not a demonstrated runtime crash; safe bzip2
slice callers already supply nonnull pointers.

Review covered the production rust-allocator configuration: allocation layouts,
zero initialization, stream ownership/state checks, raw slice/pointer contracts,
compression/decompression buffer accounting, pending run writes, end cleanup,
and whole-buffer API paths. The main blocksort/compression/decompression modules
forbid unsafe code; their malformed-input state limits were also examined.
Optional stdio/C allocator/export-symbol configurations are not deployed and
require additional review before enabling. The independent ISRG 0.1.1 review
was consulted as historical evidence, not treated as covering all 0.2.5 changes.

```
cargo test --locked --offline --manifest-path rust/Cargo.toml -p libbz2-rs-sys --lib
```

Two unit tests pass. The regression compresses a repeated byte run, decodes one
byte, asserts that pending run output remains, then supplies null/zero output
and verifies successful progress handling and final cleanup. No sanitizer or
Miri finding is claimed for the original zero-count intrinsic defect.
