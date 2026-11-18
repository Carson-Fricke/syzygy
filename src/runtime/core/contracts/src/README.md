# Runtime contract source

- `lib.rs` defines append-only output fragments and their standard channels.
- `message.rs` defines session directions and which envelopes can carry requests or correlations.
- `model/` defines the provider interface, neutral model history and replies, and tool schemas and identities.
- `settlement.rs` defines exact tool correlation and derives which requests, jobs, children and workflows remain pending.
