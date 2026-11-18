//! Interactive local sign-in and one Responses turn with a sample read tool.

use std::{
    error::Error,
    fs::OpenOptions,
    io::{self, Read, Write},
    path::Path,
};

use serde_json::{Value, json};
use syzygy_conversation::{RunOutcome, SqliteStore, Timeline, ToolCall, ToolInput, ToolOutput};
use syzygy_responses::{
    ChatGpt, FunctionTool, HostId, RequestOptions, ResponsesProvider, Tool, ToolNamespace,
};
use syzygy_runtime_contracts::OutputChannel;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let usage = "usage: chatgpt HOST_ID_FILE (--models | MODEL PROMPT)";
    let location = args.next().ok_or(usage)?;
    let model = args
        .next()
        .and_then(|arg| arg.into_string().ok())
        .ok_or(usage)?;
    let prompt = args
        .next()
        .map(|arg| arg.into_string().map_err(|_| usage))
        .transpose()?;
    if args.next().is_some() || (model == "--models") == prompt.is_some() {
        return Err(usage.into());
    }
    let host = host_id(Path::new(&location))?;
    let client = ChatGpt::new()?;
    let mut session = client.sign_in(&host, None, |url| {
        eprintln!("Continue with ChatGPT in your browser:\n{url}");
        Ok(())
    })?;
    let Some(prompt) = prompt else {
        for model in client.models(&session)? {
            println!("{}\t{}", model.slug, model.display_name);
        }
        return Ok(());
    };

    let options = RequestOptions {
        tools: vec![ToolNamespace {
            name: "research".into(),
            description: "Read sample trial results".into(),
            tools: vec![Tool::Function(FunctionTool {
                name: "read_trial".into(),
                description: "Read one sample trial by ID".into(),
                strict: true,
                parameters: json!({
                    "type":"object",
                    "properties":{"trial":{"type":"string"}},
                    "required":["trial"],
                    "additionalProperties":false
                }),
            })],
        }],
        ..RequestOptions::default()
    };
    if session.needs_refresh() {
        client.refresh(&mut session)?;
    }
    let mut provider = ResponsesProvider::new(&client, &session, &model, &options)
        .with_instructions("Use the research tool to read sample trial results when needed.");
    let mut timeline = Timeline::new(SqliteStore::open(":memory:")?);
    timeline.start("sample", "first", &prompt)?;
    let outcome = timeline.run("sample", &mut provider, &mut lookup, &mut |delta| {
        if delta.channel == OutputChannel::Text {
            print!("{}", delta.text);
            let _ = io::stdout().flush();
        }
    })?;
    match outcome {
        RunOutcome::Complete => {
            println!();
            Ok(())
        }
        RunOutcome::Uncertain { call_id } => {
            Err(format!("tool outcome requires reconciliation: {call_id}").into())
        }
    }
}

fn lookup(call: &ToolCall) -> Result<ToolOutput, String> {
    let ToolInput::Function { arguments } = &call.input else {
        return Err("unexpected custom tool call".into());
    };
    if call.namespace.as_deref() != Some("research") || call.name != "read_trial" {
        return Err("unexpected tool call".into());
    }
    let arguments: Value = serde_json::from_str(arguments).map_err(|error| error.to_string())?;
    let output = if arguments["trial"] == "trial-7" {
        json!({"trial":"trial-7", "passed":true})
    } else {
        json!({"error":"unknown sample trial"})
    };
    Ok(ToolOutput::Text(output.to_string()))
}

fn host_id(path: &Path) -> Result<HostId, Box<dyn Error>> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.lock()?;
    let mut existing = String::new();
    file.read_to_string(&mut existing)?;
    if !existing.is_empty() {
        return Ok(serde_json::from_str(&existing)?);
    }
    let host = HostId::default();
    serde_json::to_writer(&mut file, &host)?;
    file.sync_all()?;
    Ok(host)
}
