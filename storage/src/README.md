# Native storage source

- `lib.rs` exposes stable content-derived catalog identities.
- `timeline.rs` owns the sequenced append and reconciliation state machine.
- `chunks.rs` writes bounded content-addressed trees and verifies the payload on read.
- `fences.rs` orders document locks and retries only stale fence conditions.
- `leases.rs` bounds lease contention waits and checks expiry against the adapter's supplied clock.
- `notifications.rs` owns local commit versions and waits for a change without advancing durable cursors.
