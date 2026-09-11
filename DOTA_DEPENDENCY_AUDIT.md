# Dota dependency audit findings

This note records the cargo-vet review performed for the Dota lobby host
dependency graph, rechecked on 2026-09-11. The checked graph is the locked workspace
graph (`cargo vet ... --locked`) after adding `cama-steam`.

The policy now imports published audits from the following sources. The five
new registry URLs are pinned to the source commits resolved with
`git ls-remote`, and their files were fetched successfully at those commits:

- [Embark Studios](https://github.com/EmbarkStudios/rust-ecosystem/tree/bc9f6c527108a8b79b74d44121aed574e7b5e526)
- [Google](https://github.com/google/supply-chain/tree/c5115397e1ad0a0d2ac630a9aafe3eb2b615426b)
- [ISRG](https://github.com/divviup/libprio-rs/tree/b7f0cbe7145c01b618cd7b2f6a9636490b62f276)
- [Mozilla](https://github.com/mozilla/supply-chain/tree/d7f9f897cbabc04d16ae2a62374e098d850d46fa)
- [Zcash](https://github.com/zcash/rust-ecosystem/tree/ce132d7abdd52f2b3b0d55ed5dc9f152331535c2)

Embark's published audit covers `array-init 2.1.0`, reducing the residual
unvetted set from 95 to 94. The other official registry candidates examined
(Actix, Ariel OS, and Fermyon) had no audit that covered a package version in
this graph. No new exemption was added, and all 246 exemption tuples present
on `main` remain in the policy.

The 2026-09-11 check reports **99 unvetted dependencies**. Postgame metadata
decoding added five uncovered versions: `bzip2 0.6.1`, `libbz2-rs-sys 0.2.5`,
`zstd 0.13.3`, `zstd-safe 7.3.0`, and `zstd-sys 2.1.0+zstd.1.5.7`.

The cargo-vet gate still fails because the following exact package versions
have no complete `safe-to-deploy` audit chain in the imported evidence:

```text
aes 0.8.4
another-steam-totp 0.4.2
async-stream 0.3.6
async-stream-impl 0.3.6
aws-lc-rs 1.18.1
aws-lc-sys 0.45.0
axum 0.8.9
axum-core 0.5.6
binrw 0.15.2
binrw_derive 0.15.2
block-padding 0.3.3
bytemuck 1.25.2
bytemuck_derive 1.12.0
bzip2 0.6.1
cbc 0.1.2
cipher 0.4.4
cmake 0.1.58
combine 4.6.8
const-oid 0.9.6
core-foundation 0.9.4
core-foundation 0.10.1
crc 3.4.0
crc-catalog 2.5.0
crossbeam-utils 0.8.23
dashmap 6.2.1
der 0.7.10
directories 6.0.0
dirs-sys 0.5.0
either 1.18.0
fs_extra 1.3.0
getrandom 0.3.4
h2 0.4.19
indexmap 2.14.2
jni 0.22.4
jni-macros 0.22.4
jni-sys 0.4.1
jni-sys-macros 0.4.1
jobserver 0.1.35
libbz2-rs-sys 0.2.5
libredox 0.1.23
linux-raw-sys 0.12.1
matchit 0.8.4
multiversion 0.9.0
multiversion-macros 0.9.0
num-bigint-dig 0.8.6
num-iter 0.1.46
num_enum 0.7.6
num_enum_derive 0.7.6
openssl-probe 0.2.1
owo-colors 4.4.0
pem-rfc7468 0.7.0
pkcs1 0.7.5
pkcs8 0.10.2
proc-macro-crate 3.5.0
protobuf 3.5.1
protobuf-support 3.5.1
r-efi 5.3.0
rand 0.9.5
redox_users 0.5.2
reqwest 0.13.5
rsa 0.9.10
rustix 1.1.4
rustls-native-certs 0.8.4
rustls-platform-verifier 0.7.0
rustls-platform-verifier-android 0.1.1
same-file 1.0.6
schannel 0.1.29
security-framework 3.7.0
security-framework-sys 2.17.0
semver 1.0.28
serde_path_to_error 0.1.20
sha-1 0.10.1
simd_cesu8 1.2.0
spin 0.9.9
spki 0.7.3
steam-vent 0.5.0
steam-vent-crypto 0.2.1
steam-vent-proto-common 0.5.1
steam-vent-proto-dota2 0.5.2
steam-vent-proto-steam 0.5.2
steamid-ng 3.0.0
system-configuration 0.7.0
system-configuration-sys 0.6.0
tempfile 3.27.0
tokio-stream 0.1.19
tokio-tungstenite 0.29.0
toml_edit 0.25.14+spec-1.1.0
toml_parser 1.1.3+spec-1.1.0
tungstenite 0.29.0
walkdir 2.5.0
wasip2 1.0.4+wasi-0.2.12
webpki-root-certs 1.0.9
winapi-util 0.1.11
windows-registry 0.6.1
winnow 1.0.4
wit-bindgen 0.57.1
zstd 0.13.3
zstd-safe 7.3.0
zstd-sys 2.1.0+zstd.1.5.7
```

Several imported sources contain useful deltas for these crates but no
audited starting version, so cargo-vet correctly cannot treat those deltas as
a complete chain. The Steam transport crates have no matching audit in the
official cargo-vet registry. Resolving the remaining failures requires a
maintainer review of the exact versions or a future published audit; this
file intentionally does not substitute exemptions or self-attestations.

The reproducible check used for this finding was:

```text
cargo vet check --locked \
  --manifest-path rust/Cargo.toml --no-minimize-exemptions
```
