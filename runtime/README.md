# Runtime

The `runtime` Rust crate exposes the portable workflow graph, callback execution,
inbox, contracts, provider projection, scheduling, tool policy, and storage
policies. Its dependency graph uses Rust crates. Hosts compose persistence,
workers, and provider transports around these APIs.

## Workflow callbacks

`callbacks::WorkflowCallback` accepts a `CallbackRequest` and returns a JSON
object. An in-process Rust closure can implement it directly. `ProcessCallback`
runs a configured program with an argument vector and a deadline. Use a bundle
directory when the command needs paths relative to its workflow files.

The command reads one UTF-8 JSON document from stdin until EOF. The envelope has
`invocationId`, `operation`, an optional `phaseAttemptId`, and `payload`. The
runtime sets `payload.protocolVersion` to 2. The payload for each operation is
the same data passed to an in-process workflow extension.

Write a single JSON document to stdout with the matching `invocationId` and
either a `result` object or an `error` object with a nonempty `message`. Write
diagnostics to stderr. An `execute` result contains a `completion` object. The
host validates and accepts that proposed transition through its workflow store.
The subprocess adapter bounds reply size to four MiB and waits for successful
exit before accepting the reply.

`JournaledCallback` reserves a durable invocation identity before calling the
command. A retry joins its retained reply when available. A timeout, failed
reply, or mismatched acknowledgement leaves a retained invocation requiring
reconciliation. The callback invocation ID identifies the phase call. A harness
submission inside that call retains its own canonical job ID.

`FileInvocationJournal` gives local hosts durable request claims and terminal
receipts. Replicated hosts can implement `InvocationJournal` using their shared
persistence. File journals should reside in the local application's durable
state directory.
