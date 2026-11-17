# Syzygy

The [runtime](runtime/) crate provides the portable Rust workflow callback
boundary and shared runtime policies. Its workflow graph, inbox, provider
projection, tool policy, storage, and scheduling crates are workspace members.

The workflow callback contract supports Rust callables and local executable
commands. See the [runtime documentation](runtime/README.md) for the JSON
protocol, invocation retention, and timeout behavior.

The original FCA checkout continues to compose the full application host and its
CPython integration. This repository contains the portable Rust packages.

Run the workspace checks from this repository root.

```text
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Initial source copied from FCA commit `36212e98`.
