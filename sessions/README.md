# Native session policy

Drives bounded session slices, tracks recovery and schema failures, and
buffers model output until its timeline commit. The host performs effects
through its provider, tool, and persistence ports.

- `Cargo.toml` declares the library.
- `src/` contains the bounded session policy.
