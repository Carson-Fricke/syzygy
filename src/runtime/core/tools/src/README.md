# Tool dispatch source

- `lib.rs` owns ordered registration, catalog filtering, and authorized invocation with separate pre-dispatch and post-dispatch failures.
- `json_recovery.rs` locates candidate JSON documents in model output.
- `reading.rs` pages text with exact UTF-8 boundaries and a JSON-aware cl100k_base token budget.
