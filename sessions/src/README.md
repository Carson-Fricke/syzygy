# Session policy source

- `lib.rs` tracks operator input, bounded provider recovery, and repeated tool schema failures.
- `output.rs` retains partial output, coalesces provisional frames, and advances their UTF-8 offsets after a commit.
- `context.rs` plans correlated evidence groups and budgeted context windows.
