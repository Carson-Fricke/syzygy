# Runtime contract source

- `lib.rs` defines append-only output fragments and their standard channels.
- `message.rs` defines session directions and which envelopes can carry requests or correlations.
- `settlement.rs` defines exact tool correlation and derives which requests, jobs, children and workflows remain pending.
