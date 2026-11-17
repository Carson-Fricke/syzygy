# Workflow graph source

- `lib.rs` exports the pure graph API.
- `contracts.rs` defines nodes, destinations, terminal outcomes, accepted receipts, and errors.
- `topology.rs` validates named ports and reachability.
- `replay.rs` computes the ordered frontier, pending named inputs, branch coverage and history references.
- `acceptance.rs` rebuilds a candidate after a revision conflict and publishes no effects before the durable commit.
