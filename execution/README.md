# Native work scheduling

Owns process-local ordering and capacity for durable references and decides
whether a claimed attempt should run or record an interruption. Storage
admission, renewable claims and actual attempts belong to the worker and its
persistence ports.

- `Cargo.toml` declares the library.
- `src/` contains the ready queue.
- `tests/` verifies resource and retry ordering.
