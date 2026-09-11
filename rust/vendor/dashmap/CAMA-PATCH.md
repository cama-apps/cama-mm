# Cama patch of dashmap 6.2.1

Source: exact crates.io dashmap 6.2.1 archive. Changes strengthen preexisting unsafe Send/Sync implementations:

- Mutable value guards and owned entries require Send as well as Sync for keys/values.
- Borrowed map/set iterators require their referenced map M to be Sync.

This closes compile-time acceptance of Sync-but-not-Send values crossing threads through mutable/owned guards and Send-but-not-Sync map contents crossing threads via a shared iterator. Current Steam filter key/value types already satisfy the stronger bounds.

No synchronization algorithm or raw table operation changed. This is not a claim that every possible generic use of upstream dashmap has been proven sound.

Validation: five compile-only adversarial cases succeeded on upstream and correctly failed after the patch; ordinary Send+Sync usage succeeds on both. The cases are retained as executable rustdoc tests in CAMA-TRAIT-TESTS.md; they do not execute unsound code. Baseline source review covered the complete 5.5.3-to-6.2.1 runtime delta.
