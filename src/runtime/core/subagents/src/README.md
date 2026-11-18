# Subagent hierarchy source

- `lib.rs` returns a root-first traversal while ignoring missing roots and visiting each identity once.
- `authority.rs` checks current phase scope and subagent input eligibility.
