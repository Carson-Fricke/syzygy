//! Local coding tools. The host supplies approval for each edit and command.

use crate::conversation::{FunctionTool, Tool, ToolCall, ToolInput, ToolNamespace, ToolOutput};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::NamedTempFile;

const FILE_LIMIT: usize = 1024 * 1024;
const OUTPUT_LIMIT: usize = 32 * 1024;

fn function(name: &str, description: &str, properties: Value) -> Tool {
    let required: Vec<_> = properties.as_object().unwrap().keys().cloned().collect();
    Tool::Function(FunctionTool {
        name: name.into(),
        description: description.into(),
        strict: true,
        parameters: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    })
}

/// The shared tool descriptions advertised to the chat's provider.
pub fn catalog() -> ToolNamespace {
    let string = json!({"type":"string"});
    ToolNamespace {
        name: "workspace".into(),
        description: "Local coding tools. All paths are relative to the chat workspace.".into(),
        tools: vec![
            function(
                "list",
                "List up to 200 entries in a directory.",
                json!({"path":string}),
            ),
            function(
                "read",
                "Read UTF-8 file lines. start_line starts at 1.",
                json!({"path":string, "start_line":{"type":"integer","minimum":1}}),
            ),
            function(
                "edit",
                "Replace one exact, unique occurrence in a UTF-8 file. An empty before creates a new file.",
                json!({"path":string,"before":string,"after":string}),
            ),
            function(
                "command",
                "Run an approved executable with individual arguments in the workspace. Commands run with the user's OS permissions.",
                json!({"program":string,"args":{"type":"array","items":string}}),
            ),
        ],
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn path_in(root: &Path, value: &str, create: bool) -> io::Result<PathBuf> {
    let relative = Path::new(value);
    if value.is_empty()
        || (cfg!(windows) && value.contains(':'))
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(invalid("provide a relative path inside the workspace"));
    }
    let path = root.join(relative);
    if create {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("missing parent"))?
            .canonicalize()?;
        if !parent.starts_with(root) {
            return Err(invalid("path leaves workspace"));
        }
        return Ok(parent.join(
            path.file_name()
                .ok_or_else(|| invalid("missing filename"))?,
        ));
    }
    let path = path.canonicalize()?;
    if !path.starts_with(root) {
        return Err(invalid("path leaves workspace"));
    }
    Ok(path)
}

fn read_text(path: &Path) -> io::Result<String> {
    if !fs::metadata(path)?.is_file() {
        return Err(invalid("expected regular file"));
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("expected regular file"));
    }
    let mut text = String::new();
    file.take((FILE_LIMIT + 1) as u64)
        .read_to_string(&mut text)?;
    if text.len() > FILE_LIMIT {
        return Err(invalid("file exceeds one MiB"));
    }
    Ok(text)
}

fn string<'a>(v: &'a Value, key: &str) -> io::Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| invalid(&format!("expected string for {key}")))
}

/// A proposed change or command, presented to the host before applying its effects.
#[derive(Debug)]
pub enum Approval<'a> {
    Edit {
        path: &'a Path,
        before: &'a str,
        after: &'a str,
    },
    Command {
        directory: &'a Path,
        program: &'a str,
        args: &'a [&'a str],
    },
}

enum Failure {
    Known(io::Error),
    Uncertain(io::Error),
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Known(error)
    }
}

/// Execute one admitted call in the canonical workspace directory recorded by the
/// host. An error indicates an uncertain effect that must be reconciled before resuming.
pub fn execute(
    root: &Path,
    call: &ToolCall,
    approve: &mut impl FnMut(Approval<'_>) -> io::Result<bool>,
) -> Result<ToolOutput, String> {
    let ToolInput::Function { arguments } = &call.input else {
        return Ok(ToolOutput::Text("Unsupported tool type".into()));
    };
    if call.namespace.as_deref() != Some("workspace") {
        return Ok(ToolOutput::Text("Unknown tool namespace".into()));
    }
    let args: Value = match serde_json::from_str(arguments) {
        Ok(value) => value,
        Err(e) => return Ok(ToolOutput::Text(format!("Invalid arguments: {e}"))),
    };
    // A lost acknowledgement after an external effect must remain uncertain.
    let result = perform(root, &call.name, &args, approve);
    match result {
        Ok(value) => Ok(ToolOutput::Text(value.to_string())),
        Err(Failure::Uncertain(e)) => Err(e.to_string()),
        Err(Failure::Known(e)) => Ok(ToolOutput::Text(json!({"error":e.to_string()}).to_string())),
    }
}

fn perform(
    root: &Path,
    name: &str,
    args: &Value,
    approve: &mut impl FnMut(Approval<'_>) -> io::Result<bool>,
) -> Result<Value, Failure> {
    if root.canonicalize()? != root || !root.is_dir() {
        return Err(invalid("workspace must be its original canonical directory").into());
    }
    match name {
        "list" => {
            let path = path_in(root, string(args, "path")?, false)?;
            let mut entries = Vec::new();
            for entry in fs::read_dir(path)?.take(201) {
                let entry = entry?;
                entries.push(entry.file_name().to_string_lossy().into_owned());
            }
            let truncated = entries.len() > 200;
            entries.truncate(200);
            entries.sort();
            Ok(json!({"entries":entries, "truncated":truncated}))
        }
        "read" => {
            let start = args["start_line"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| invalid("invalid start_line"))?;
            let file = read_text(&path_in(root, string(args, "path")?, false)?)?;
            let mut lines = Vec::new();
            let mut size = 0;
            let mut next = None;
            for (i, line) in file.lines().enumerate().skip(start - 1) {
                if line.len() >= OUTPUT_LIMIT {
                    return Err(invalid("single line exceeds the output limit").into());
                }
                if size + line.len() + 1 > OUTPUT_LIMIT || lines.len() >= 200 {
                    next = Some(i + 1);
                    break;
                }
                size += line.len() + 1;
                lines.push(line);
            }
            Ok(json!({"start_line":start,"lines":lines,"next_line":next}))
        }
        "edit" => {
            let before = string(args, "before")?;
            let after = string(args, "after")?;
            let target = string(args, "path")?;
            let path = path_in(root, target, before.is_empty())?;
            let prior = if before.is_empty() {
                if path.try_exists()? {
                    return Err(invalid("file already exists").into());
                }
                String::new()
            } else {
                let old = read_text(&path)?;
                if old.matches(before).count() != 1 {
                    return Err(invalid("expected one exact match").into());
                }
                old
            };
            if !approve(Approval::Edit {
                path: &path,
                before,
                after,
            })? {
                return Ok(json!({"denied":true}));
            }
            let content = if before.is_empty() {
                after.into()
            } else {
                prior.replacen(before, after, 1)
            };
            if content.len() > FILE_LIMIT {
                return Err(invalid("edit exceeds one MiB").into());
            }
            if path_in(root, target, before.is_empty())? != path {
                return Err(invalid("file or directory changed since preview").into());
            }
            let mut tmp =
                NamedTempFile::new_in(path.parent().ok_or_else(|| invalid("missing parent"))?)?;
            tmp.write_all(content.as_bytes())?;
            if !before.is_empty() {
                if read_text(&path)? != prior {
                    return Err(invalid("file changed since preview").into());
                }
                tmp.as_file()
                    .set_permissions(fs::metadata(&path)?.permissions())?;
            }
            tmp.as_file().sync_all()?;
            if before.is_empty() {
                tmp.persist_noclobber(&path).map_err(io::Error::from)?;
            } else {
                tmp.persist(&path).map_err(io::Error::from)?;
            }
            #[cfg(unix)]
            File::open(path.parent().expect("validated parent directory"))
                .and_then(|directory| directory.sync_all())
                .map_err(Failure::Uncertain)?;
            Ok(json!({"written":content.len()}))
        }
        "command" => {
            let program = string(args, "program")?;
            let argv = args["args"]
                .as_array()
                .ok_or_else(|| invalid("expected args array"))?
                .iter()
                .map(|s| {
                    s.as_str()
                        .ok_or_else(|| invalid("expected string argument"))
                })
                .collect::<io::Result<Vec<_>>>()?;
            if !approve(Approval::Command {
                directory: root,
                program,
                args: &argv,
            })? {
                return Ok(json!({"denied":true}));
            }
            if root.canonicalize()? != root {
                return Err(invalid("workspace directory changed since preview").into());
            }
            run_command(root, program, &argv)
        }
        _ => Err(invalid("unknown workspace tool").into()),
    }
}

fn captured(file: File) -> io::Result<(String, bool)> {
    use std::io::{Seek, SeekFrom};
    let mut file = file;
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take((OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > OUTPUT_LIMIT;
    bytes.truncate(OUTPUT_LIMIT);
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

fn run_command(root: &Path, program: &str, args: &[&str]) -> Result<Value, Failure> {
    let out = tempfile::tempfile()?;
    let err = tempfile::tempfile()?;
    let mut child = Command::new(program)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(err.try_clone()?)
        .spawn()?;
    let result = collect_output(&mut child, out, err);
    if result.is_err() && !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
        let _ = child.wait();
    }
    result.map_err(Failure::Uncertain)
}

fn collect_output(child: &mut Child, out: File, err: File) -> io::Result<Value> {
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= Duration::from_secs(120)
            || out.metadata()?.len() + err.metadata()?.len() > 16 * 1024 * 1024
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "command interrupted after its time or output limit; verify external effects before resolving",
            ));
        }
        thread::sleep(Duration::from_millis(50));
    };
    let (stdout, out_cut) = captured(out)?;
    let (stderr, err_cut) = captured(err)?;
    Ok(
        json!({"exit_code":status.code(),"stdout":stdout,"stderr":stderr,"truncated":out_cut||err_cut}),
    )
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
