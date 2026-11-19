# OpenAI Responses adapter

`syzygy-adapter-openai-responses` connects a local host to the OpenAI Responses
API using Sign in with ChatGPT. Hosts that import the `runtime` crate can
enable the `responses` feature and use `runtime::responses`.

- `ChatGpt::sign_in` starts a loopback listener and gives the host an authorization URL
  to open in the system browser. It validates the PKCE exchange, the signed OpenAI
  identity, and the plan-usage grant before returning a `Session`.
- `AccountStore` keeps one protected local account registration and serializes
  sign-in and token refresh. Hosts with an existing credential store can supply
  a stable `HostId` and atomically replace the serialized `Session` under a lock.
- Construct a `ResponsesProvider` with the account, model and request options.
  Pass it to `runtime::conversation::Timeline::run` together with the host's tool
  executor. The timeline durably admits a completed reply and claims each tool
  before dispatch. Native output, including encrypted reasoning, stays attached
  to the reply for replay on the same verified account and model.
- A host with its own durable turn coordinator can render settled history with
  `PreparedRequest::from_history`, audit the exact bytes, and pass the request
  to `ChatGpt::stream`. Dropping the stream closes the response. A terminal
  reply is a candidate for admission against that host's current snapshot.
  Deltas can be displayed while the response is in progress.
- The adapter accepts a missing HTTP `Content-Type` only when the first decoded
  SSE event is `response.created` with a response ID. It rejects explicitly
  incompatible media types and requires `response.completed` for a reply.

## Local example

Choose a writable host ID file outside the repository. The example retains the
host ID in that file and holds the OAuth grant in memory for the process.

```text
cargo run -p syzygy-adapter-openai-responses --example chatgpt -- <host-id-file> --models
cargo run -p syzygy-adapter-openai-responses --example chatgpt -- <host-id-file> <model-slug> "Use the research tool to check trial-7."
```

Open the sign-in URL printed by the example and authorize ChatGPT plan usage.
The model list depends on the account. [OpenAI sign-in and plan usage](https://developers.openai.com/siwc/token-sharing-open-source)

## Wire verification

`tests/capture_codex.py` starts the pinned Codex App Server with a loopback
Responses endpoint and synthetic credentials. It records the model, core HTTP
options, namespaced tool schema, ordered user and replay items, and text deltas.
The fixture drops generated result item IDs and null optional reasoning content.
Syzygy's loopback test checks both outgoing requests against that fixture and
consumes the same SSE sequence. Separate cases exercise custom and parallel
results, OAuth renewal and validation, and interrupted, failed, and cancelled
streams.

```text
python src/runtime/adapters/openai-responses/tests/capture_codex.py src/runtime/adapters/openai-responses/tests/codex_wire.json
cargo test --locked -p syzygy-adapter-openai-responses
```

The reference uses Codex's documented [ChatGPT-plan App Server configuration](https://developers.openai.com/siwc/token-sharing-open-source/codex-app-server).
