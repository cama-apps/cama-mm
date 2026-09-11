Vendored const-oid 0.9.6; crates.io SHA-256 c2459377285ad874054d797f3ccebf984978aa39129f6eafde5cdc8315b612f8.

The decimal OID parser recursively consumed one stack frame per digit and used unchecked u32 multiply/add, permitting overflow to alias another OID. Replaced recursion with an index loop and checked arithmetic. Valid arc behavior and const evaluation remain supported. Regression tests cover maximum/overflowing arcs and 100,000 leading-zero digits. Original Cargo.toml and licenses retained. Audit source review and snapshot hash in supply-chain/reviews/pr669.md.

Independent follow-up fixed test integration without adding dependencies: test builds import std explicitly, and the upstream hex fixture uses its equivalent byte array. Consecutive separators now return DigitExpected instead of silently introducing a zero arc; a regression covers that alias. Production no_std behavior is unchanged.

The independent review also found the binary DER arc decoder checked the final byte rather than accumulator overflow. It could alias 2^32 to zero and reject valid large u32 arcs. Binary decoding now checks each base-128 multiply/add and rejects nonminimal leading-zero continuation bytes. Tests cover the overflow attack, maximum u32 round trip/iteration, noncanonical encodings, and truncation.

Boundary round-trip validation uncovered two coupled upstream encoding defects: arc128 was encoded as a leading-zero continuation pair, and the byte constructor rejected valid two-byte encodings of three-arc OIDs. The base128 continuation comparison is now >=128, and the minimum byte length matches the existing encoder. Boundary tests cover 127/128, 16383/16384, 2097151/2097152 and u32max.
