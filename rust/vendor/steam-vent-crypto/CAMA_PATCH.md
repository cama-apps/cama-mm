# Cama security patch: steam-vent-crypto 0.2.1

Source: checksum-verified crates.io `steam-vent-crypto-0.2.1.crate`, SHA-256
`c37c08a0e9fa3fbc85a3359a02cf82700e39810a01e707aeaa1869915def112d`.
Author Robin Appelman; upstream manifest declares MIT and is preserved verbatim
alongside `Cargo.toml.orig`. No LICENSE file is present in the published archive.
Production `src/lib.rs`, `build.rs`, and embedded public `system.pem` are retained.

Complete source and build-script review: the build reads the embedded public RSA
key and writes numeric public-key constants under Cargo OUT_DIR; it performs no
network, subprocess, native build, or ambient-secret access. Runtime delegates
RSA/AES/HMAC primitives to RustCrypto dependencies, with no unsafe code or ambient
filesystem/network operations. Cama WSS uses RSA public-key password encryption;
legacy TCP uses the symmetric helpers.

Changes:

- Validate ciphertext contains a 16-byte encrypted IV plus at least one aligned
  AES block before any split, for both authenticated and explicitly unauthenticated
  public decode functions. Malformed peer bytes return `MalformedMessage`.
- Verify Steam's 13-byte truncated HMAC-SHA1 using the primitive's constant-time
  `Mac::verify_truncated_left` instead of ordinary slice equality.
- Regression tests exercise every short/misaligned length, each corrupted byte
  of an otherwise valid authentication tag, and empty/multiblock round trips.

The protocol-mandated SHA-1 algorithms and documented unauthenticated helper are
not generic cryptography recommendations. Callers of the buffer API must provide
a 16-byte IV buffer as documented. Secret memory is not guaranteed to be zeroized;
keys and plaintext are never logged by this crate. No claim is made that the
unmodified crates.io version contains these fixes.
