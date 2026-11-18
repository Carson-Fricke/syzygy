# Inbox source

- `lib.rs` implements arrival insertion, authoritative reconciliation, pending selection, acknowledgement, and compaction.
- `turns.rs` coalesces wakes and tracks observed, consumed, and committed input cursors.
- `admission.rs` validates canonical incoming direction and denies outgoing presentation at admission.
