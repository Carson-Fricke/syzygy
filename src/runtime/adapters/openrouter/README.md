# OpenRouter adapter

`syzygy-adapter-openrouter` implements the Syzygy conversation provider over
streaming OpenRouter chat completions. Construct it with the selected profile's model,
an API key, the host's tool catalog, instructions, and optional reasoning effort.
The terminal app reads the key from `OPENROUTER_API_KEY`.

The adapter advertises JSON function tools with names scoped to their original
namespace. It uses the shared SSE and completion assemblers, retains native
reasoning fields for the selected model, and replays settled results in call
order. Truncated streams and incomplete function calls cannot authorize tools.
Free-form custom tools require a provider that supports them.

Native replay is bound to the model and a fingerprint of the API key. The raw key
is used only for HTTP authorization. Use the original key to resume a chat or
start a new chat under a replacement key.

The loopback tests cover the outgoing request, fragmented tool arguments,
opaque reasoning, correlated results, visible text deltas, and interruption.
See the [OpenRouter tool protocol](https://openrouter.ai/docs/guides/features/tool-calling)
and [reasoning replay](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens).
