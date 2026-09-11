# Cama security patch: steam-vent 0.5.0

Source: the crates.io `steam-vent-0.5.0.crate` archive, SHA-256
`d9d416a5598309a8e526c4a5cc24ee1eaed712390f4a96ecc48db019a6ce1300`.
Upstream VCS revision: `1325a4a0235a22c3604ea943b4362148afc13276`,
https://codeberg.org/steam-vent/steam-vent . Author: Robin Appelman.
The normalized and original manifests declare MIT. Neither the published archive
nor the pinned upstream repository root includes a LICENSE file; the original
manifest and README are retained as published. No license text has been invented.
Only production Rust source, manifests, and README were extracted from the
checksum-verified archive. Examples, unrelated development dependencies, Nix,
protocol-generation tooling, and images are not part of this production patch.
The original manifest is preserved verbatim as `Cargo.toml.orig`.

The patch does not certify unmodified crates.io 0.5.0. It is reviewed local source:

- Reject complete messages above 16 MiB and protobuf headers above 64 KiB;
  check available bytes before allocating a header and before splitting buffers.
- Bound aggregate multi-message inflation to 32 MiB, require gzip output to match
  its declared size, and reject truncated, zero-length, or oversized child records.
  Inner lengths only slice existing bounded buffers; malformed iterators terminate.
- Reject invalid authentication poll intervals (accepted: 0.1 to 60 seconds) and
  heartbeat intervals (accepted: 1 to 300 seconds). These are finite protocol
  sanity bounds, not operator environment variables.
- Remove raw authentication/payload/session-key tracing and TLS key logging.
  Token and session debug representations redact credentials. Invalid shared-secret configuration
  aborts authentication instead of panicking.
- Cancel the incoming-message task when its last owner drops. Clear pending
  requests on stream end; unregister jobs on success, error, timeout, and future
  cancellation. Prune abandoned direct receiver registrations on new requests.
  Full multi-response queues close that response instead of stalling every job.
- Tie GC forwarding to the owning filter, ignore messages for other app IDs,
  and reject disagreements between outer and inner GC message kinds.
- Use the workspace-compatible reqwest 0.12 rustls transport and explicit ring
  TLS provider, with trusted WebPKI roots and bounded WebSocket frame/message sizes.
  This removes the second reqwest/TLS/provider/native-library dependency tree.
- Apply the same frame cap to legacy TCP and reject malformed encrypted frames
  before invoking its AES decoder. TCP remains unused by the Cama bot.

Validation: `cargo test --locked --manifest-path rust/Cargo.toml -p steam-vent --lib`
and `cargo clippy --locked --manifest-path rust/Cargo.toml -p steam-vent --lib
--tests -- -D warnings`. Twenty deterministic unit tests pass, covering framing,
gzip size/CRC/truncation, request cleanup/queue overflow/source cancellation,
coordinator isolation, intervals, and credential redaction. No live Steam login
or connection is used by these tests.

Review boundaries: Cama uses WSS, its own restrictive and atomic credential store,
and an outer authentication deadline. The upstream default FileGuardDataStore
still relies on filesystem umask and is not used by Cama. The traits' default
unimplemented encode/decode methods and the legacy encryption buffer API retain
documented caller preconditions. Dependency cryptographic primitives and generated
protocol implementations require separate review coverage. No security exemption
or audit of unmodified upstream source is implied by this patch.

Independent follow-up: heartbeat cancellation now also interrupts a stalled transport write, and failed sends terminate the worker. A deterministic fake-sink test proves dropping the connection releases the blocked write future and its resources.
