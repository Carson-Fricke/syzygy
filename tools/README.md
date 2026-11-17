# Native tool dispatch

Owns session catalog order, effective tool access, and validation before
execution. Consumers supply tool schemas, validators, and concrete handlers.

- `Cargo.toml` declares the catalog and dispatch library.
- `src/` contains their implementation.
