Vendored from crates.io tungstenite 0.29.0.
Archive SHA-256: 6c01152af293afb9c7c2a57e4b559c5620b421f6d133261c60dd2d0cdb38e6b8

Dependency change: use rand 0.10.2 already present in Cama instead of rand 0.9. The two uses are rand::random for a 128-bit WebSocket handshake nonce and a 32-bit frame mask; both APIs are unchanged. No code calls distribution deserialization. This removes the duplicate rand/getrandom graph and avoids carrying the flawed rand 0.9.5 UniformChar serde range validation found during review.

Examples/benchmarks and their development dependencies are omitted. Production source change: payload/frame/handshake-request and close-reason trace/debug logging now reports lengths or event names; no authentication bytes are logged. Other production Rust source is identical to the published archive. Upstream MIT and Apache licenses are retained. Review and validation are recorded in rust/supply-chain/reviews/pr669.md.
