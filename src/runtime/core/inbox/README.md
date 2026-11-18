# Native inbox state

Owns the ordered canonical message references and pending input positions for
one session. It also coalesces local conversation wakes against in-flight and
durable consumption. The host retains payloads and supplies the persistence
and synchronization ports.

- `Cargo.toml` declares the library.
- `src/` contains the order and consumption state.
- `tests/` exercises wake generations across durable acknowledgements.
