# Provider projection source

- `lib.rs` defines structural transcript inputs, exact correlation matching, and the ordered projection plan.
- `sse.rs` decodes incremental provider frames with strict UTF-8, line and event limits.
- `completion.rs` assembles deltas, tool calls, terminal metadata and usage.
- `reasoning.rs` separates readable deltas, scalar aliases and opaque continuation blocks.
