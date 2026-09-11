# Cryptographic dependency source review

## aes 0.8.4

Reviewed manifest, CPU dispatch/union lifecycle, x86 AES-NI single/eight-block load/store and key expansion, ARMv8 load/store/key expansion/inline assembly, and safe software backend structure. Unsafe loads/stores operate on fixed 16-byte blocks and 8-block arrays through cipher InOut; AES-192 expands a 24-byte key into a 32-byte temporary before vector loads. Runtime cpufeatures tokens select exactly the initialized union member and gate target-feature calls, clone, conversion, and drop. ARM register-only asm uses nomem/nostack; key schedule slice reinterpretation has explicit alignment and array-size checks. No build script, network/filesystem/process access, or secret logging. Cryptographic primitive review, not a new proof of AES; use through existing RustCrypto CBC and Steam's authenticated protocol. Current graph enables default features, no hazmat or custom AES target cfg.

## block-padding 0.3.3

Reviewed complete padding API structure and unsafe unpad_blocks slice conversion. The returned byte length is derived from the existing block slice and checked final-block unpadding; custom padding cannot return a longer result without hitting an assertion. Pkcs7 checks padding count and bytes for a nonempty valid-size cipher block. No build/native/network/process/filesystem behavior. Documented raw-pad position/block-size preconditions remain caller obligations; Cama uses fixed 16-byte AES blocks through cipher, and patched Steam crypto rejects empty/misaligned ciphertext before padding decode.

## cbc 0.1.2

Read complete lib.rs, encrypt.rs, decrypt.rs and manifest. forbid(unsafe_code), no build script or ambient operations. Fixed-size cipher block wrappers implement XOR chaining, preserving previous ciphertext for parallel decode; IV and cipher Debug output redact internal state. CBC is explicitly unauthenticated by design and is not approved as standalone authenticated encryption: Steam's wrapper verifies HMAC and Cama uses WSS. Optional zeroize drops only IV state through the zeroize primitive; no hidden system behavior.

## sha-1 0.10.1

Reviewed manifest, hash state/buffer/finalization, compression dispatch, all active x86 unsafe sites, and software compressor structure. GenericArray<u8,U64> to [u8;64] slice cast preserves layout/length; four unaligned 16-byte loads stay inside each 64-byte block. SHA/SSE2/SSSE3/SSE4.1 target-feature function is gated by the matching cpufeatures check, with software fallback. No build script or ambient operations; optional external asm dependency is not enabled. SHA-1 collision resistance is broken and is not endorsed here; graph usage is Steam legacy HMAC/OAEP protocol compatibility, not collision-resistant content identity.

## another-steam-totp 0.4.2

Read all runtime source, manifest, decoder/tag/error paths, and optional HTTP module. No unsafe/native/build/process/fs operations. Current graph enables no HTTP features and calls generate_auth_code with a configured nonempty shared secret and None offset. HMAC-SHA1 truncation bounds are within its 20-byte output; hex decoding checks even length and digits, base64 errors propagate, secrets are not logged. SHA-1 is mandated Steam HMAC compatibility. Explicit caveats: empty decoded secrets are accepted by HMAC, and extreme manually supplied i64::MIN offsets can overflow negation; neither is supplied by Cama. Optional time-query HTTP feature is outside current usage; it contacts only a fixed HTTPS Steam time endpoint and never sends the secret. Not a claim that every malformed local configuration produces an error.

## num-bigint-dig 0.8.6

Reviewed manifest/build.rs, all unsafe sites, parsing/string conversion, and the public modular-exponentiation boundary used by RSA. Build script only emits has_i128 cfg; no process/network/fs access. The only runtime unsafe operations are UTF-8 unchecked constructors after conversion to ASCII digits in radix 2..=36 plus optional ASCII minus sign, preserving UTF-8. Numeric storage/arithmetic otherwise uses safe Rust; ordinary arithmetic preconditions such as nonzero divisor and valid radix are required. Variable-time big integer arithmetic is not approved for secret RSA private-key operations; Cama uses public-key encryption with bounded public modulus/exponent. No new constant-time or full mathematical correctness proof is asserted.

## spin 0.9.9

Reviewed manifest and synchronization boundaries, especially complete active Once initialization/publication state machine and Lazy integration. Current graph enables only once (via lazy_static spin_no_std). Unsafe status casts remain valid because private AtomicStatus stores only its repr(u8) enum values. The initializer obtains exclusive state by acquire compare_exchange; it writes MaybeUninit before release Complete publication; readers use acquire and only access Complete data. Panic poisons state, failed fallible initialization resets Incomplete, and drop destroys T only after completed initialization. Send/Sync bounds reflect shared access to T. Also inspected mutex/rwlock guard lifetimes and acquire/release transitions; raw force-unlock APIs require their documented unsafe obligations. No build script/network/fs/ambient-secret operations. Optional std abort is for impossible overflow, not normal control flow; spin waiting remains a caller scheduling consideration.

## RSA decision

No unrestricted safe-to-deploy audit. Prepared safe-to-run plus rsa-public-key-encryption criterion scoped to the actual public encryption operations, with explicit Marvin limitation and mandatory callsite/dependency guards. The criterion and records are in [audits.toml](../audits.toml); integration must enforce the restrictions described in [the overview](pr669.md).

All eight registry archives match Cargo.lock SHA-256, and all archived files match their reviewed extracted counterparts. Registry source was not modified by these primitive reviews; the separate [Steam wrapper patches](steam-transport.md) address their own findings.
