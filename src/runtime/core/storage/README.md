# Native storage rules

Owns bounded append, definite-conflict retry, and exact-identity reconciliation
over the existing transactional catalog. The storage adapter executes each
prepared attempt against its database.

- `Cargo.toml` declares the storage library.
- `src/` contains durable identity and append contracts.
- `src/local_projection.rs` reads plain local projection files and replaces complete snapshots atomically.
