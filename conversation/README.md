# Conversation runtime

`Timeline` drives one active turn per chat over an ordered durable event log. A
host supplies a `Store`, a model `Provider`, and an executor that makes the
authorization decision for each tool. The optional `sqlite` feature supplies a
local SQLite store with write-ahead logging.

Use `bind` to associate a chat with its workspace and provider settings. Use a
stable input ID with `start`. An input event is itself the durable work record.
Retrying the same input joins its original position. A reused ID with different
text fails. A backend with a separate worker queue must commit both records
together.

`run` gives the provider the accepted conversation and its prior native replay
evidence. It accepts replies only against the original cursor and checks tool
IDs against the full conversation before any tool runs. For each tool, it first
commits a claim, invokes the host executor, then records the correlated result.
Known denials and handled errors are ordinary tool outputs. A process interruption
or uncertain executor error leaves the claim visible. On resume, reconcile the
external operation using its stable identity and pass the established outcome
to `resolve` before continuing. A competing worker can also have an active claim.

`events` pages forward from the last observed sequence. Streamed provider deltas
are provisional until the provider has completed and its reply has been accepted.
