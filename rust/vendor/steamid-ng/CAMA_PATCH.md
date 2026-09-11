# Cama security patch: steamid-ng 3.0.0

Source: checksum-verified crates.io `steamid-ng-3.0.0.crate`, SHA-256
`d3ed9aa559d0d2d286dfe34f728d00232281da08f2c6a2eaf09f67f4899cfdb3`.
Author Moses Miller; upstream repository https://github.com/Majora320/steamid-ng .
The published manifests, README, and license are retained where present.

Complete runtime source review: pure safe Rust identifier parsing, formatting,
validated numeric conversions, and optional serde adapters. No build script,
native code, process, network, filesystem, or ambient-secret operations.

Changes:

- Reject Steam2 account numbers whose doubled value overflows u32. They previously
  wrapped and could alias a different account ID.
- Mask public `Instance` values to their documented 20-bit field in construction
  and mutation, so oversized values cannot corrupt account-type/universe bits or
  make their validated getters panic.
- Unit regressions cover overflow rejection, maximum valid Steam2 ID, and oversized
  instance construction/mutation preserving account type, universe and account ID.

No audit of the unmodified crates.io package is asserted by this local patch.
