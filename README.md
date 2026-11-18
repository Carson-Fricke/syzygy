# Syzygy

Syzygy is a Rust coding harness and an embeddable agent runtime. The terminal
app keeps chats in SQLite and can use ChatGPT subscription or OpenRouter
profiles. FCA embeds the same runtime and supplies its application persistence.

## Run the coding app

```text
cargo install --locked --path src/app
syzygy login
syzygy models
syzygy chat --model MODEL_SLUG --workspace .
```

Open the link from `syzygy login` in your browser, then pick a slug from
`syzygy models`. The terminal prints a chat ID. Use `syzygy list`, `syzygy show ID`,
and `syzygy resume ID` to return to it. Enter `/quit` to leave an interactive
chat. Workspace reads are available automatically. Edits show an exact
replacement preview. Edits and commands require approval in an interactive
terminal. Approved commands run with the current OS user's permissions.

The default data directory is `~/.syzygy`. Set `SYZYGY_HOME` to use another
private directory. It contains the SQLite chat log and a separately locked
account grant. Windows protects the grant with current-user DPAPI. Unix uses
an owner-only directory and file. `syzygy logout` forgets the local grant.

## Configure agents

Save agent descriptions in `SYZYGY_HOME/agents.json` or pass
`--config PATH`. In this example the primary agent uses a ChatGPT subscription
and the research agent uses OpenRouter.

```json
{
  "profiles": {
    "plan": {"provider": "chatgpt", "model": "gpt-5.4", "reasoning_effort": "medium"},
    "router": {"provider": "openrouter", "model": "deepseek/deepseek-v4.1-flash"}
  },
  "agents": {
    "primary": {"model_profile": "plan"},
    "research": {"model_profile": "router"}
  }
}
```

Use model slugs available to your account. Start the selected agent with
`syzygy chat --agent research --workspace .`. OpenRouter profiles read
`OPENROUTER_API_KEY` from the environment. A new chat records its provider,
model, and workspace. A resumed chat checks its original ChatGPT account or
OpenRouter key before executing pending work. After rotating an OpenRouter key,
start a new chat.

When an interrupted tool call has no recorded outcome, inspect the workspace
or job system and record the verified result with
`syzygy resolve ID CALL_ID --result "verified outcome"`, then resume.

## Embed the runtime

The terminal app lives in `src/app/`. The [runtime](src/runtime/core/) exposes
the shared execution APIs. Its core groups the model contracts, agent
configuration, turn engine, and workflow policies. Concrete model providers,
SQLite, and workspace tools live together in
[`src/runtime/adapters/`](src/runtime/adapters/).

The workflow callback contract supports Rust callables and local executable
commands. See the [runtime documentation](src/runtime/core/README.md) for the JSON
protocol, invocation retention, and timeout behavior.

Use `runtime::config::AgentCatalog` to resolve agent model profiles.
`runtime::contracts::model` defines the shared provider and tool interface.
`runtime::conversation` drives ordered events, turn execution, and tool claims.
Hosts implement its storage and tool execution ports.
Features `responses` and `openrouter` expose their provider crates through
`runtime::responses` and `runtime::openrouter`. The `sqlite` feature exposes the
local conversation store in `runtime::sqlite`. The `workspace-tools` feature exposes file listing,
reading, editing, and command execution through `runtime::workspace`, with
approval supplied by the host.

Run the workspace checks from this repository root.

```text
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
```

Initial source copied from FCA commit `36212e98`.
