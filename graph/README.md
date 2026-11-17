# Native workflow graph

Pure, ordered activation replay shared by workflow acceptance and recovery.
The caller supplies the topology and accepted receipt identities. Input payloads,
activity execution, and storage belong to the host.

- `Cargo.toml` declares the library and its dependencies.
- `src/` contains structural graph contracts, validation, and replay.
- `tests/` exercises the public Rust behavior.

Every accepted activity selects a declared route and produces one output. A route
can feed several named ports. Alternative producers may feed the same port on
different paths, and concurrent arrivals must target distinct ports. Fan-out
coverage and sibling history remain scoped to the active branch. A node revisit
receives a fresh attempt identity.
