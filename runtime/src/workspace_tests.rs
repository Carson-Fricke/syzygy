use super::*;
use tempfile::tempdir;

fn invoke(
    root: &Path,
    name: &str,
    arguments: Value,
    mut approve: impl FnMut(Approval<'_>) -> io::Result<bool>,
) -> Value {
    let call = ToolCall {
        name: name.to_owned(),
        namespace: Some("workspace".into()),
        call_id: "call".into(),
        input: ToolInput::Function {
            arguments: arguments.to_string(),
        },
    };
    let ToolOutput::Text(text) = execute(root, &call, &mut approve).unwrap() else {
        panic!("expected a tool receipt");
    };
    serde_json::from_str(&text).unwrap()
}

#[test]
fn edit_requires_approval_respects_changed_input_and_paginates_reads() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let file = root.join("hello.rs");
    let create = json!({"path":"hello.rs","before":"","after":"first\nsecond\n"});
    assert_eq!(
        invoke(&root, "edit", create.clone(), |_| Ok(false))["denied"],
        true
    );
    assert!(!file.exists());
    assert_eq!(
        invoke(&root, "edit", create.clone(), |action| {
            let Approval::Edit {
                path,
                before,
                after,
            } = action
            else {
                panic!("expected an edit");
            };
            assert_eq!(path, file);
            assert_eq!(before, "");
            assert_eq!(after, "first\nsecond\n");
            Ok(true)
        })["written"],
        13
    );
    assert!(
        invoke(&root, "edit", create, |_| panic!(
            "cannot clobber an existing file"
        ))["error"]
            .is_string()
    );
    let read = json!({"path":"hello.rs","start_line":2});
    assert_eq!(
        invoke(&root, "read", read, |_| panic!("reading should not prompt"))["lines"],
        json!(["second"])
    );
    let edit = json!({"path":"hello.rs","before":"first","after":"changed"});
    assert!(
        invoke(&root, "edit", edit.clone(), |_| {
            fs::write(&file, "concurrently changed")?;
            Ok(true)
        })["error"]
            .as_str()
            .unwrap()
            .contains("changed since preview")
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "concurrently changed");
    fs::write(&file, "first").unwrap();
    assert_eq!(invoke(&root, "edit", edit, |_| Ok(true))["written"], 7);
}

#[test]
fn paths_and_read_budget_remain_scoped_to_the_workspace() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("ws");
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    fs::write(tmp.path().join("outside"), "private").unwrap();
    for bad in [
        "../outside".to_owned(),
        tmp.path().join("outside").display().to_string(),
    ] {
        let result = invoke(&root, "read", json!({"path":bad,"start_line":1}), |_| {
            Ok(true)
        });
        assert!(result["error"].is_string());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(tmp.path(), root.join("linked")).unwrap();
        let result = invoke(
            &root,
            "read",
            json!({"path":"linked/outside","start_line":1}),
            |_| Ok(true),
        );
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("leaves workspace")
        );
        let result = invoke(
            &root,
            "edit",
            json!({"path":"linked/new","before":"","after":"oops"}),
            |_| panic!("invalid before approval"),
        );
        assert!(result["error"].is_string());
    }
    fs::write(root.join("line"), "x".repeat(OUTPUT_LIMIT)).unwrap();
    let too_long = invoke(&root, "read", json!({"path":"line","start_line":1}), |_| {
        Ok(true)
    });
    assert!(too_long["error"].is_string());
    fs::write(root.join("line"), "x\n".repeat(201)).unwrap();
    let page = invoke(&root, "read", json!({"path":"line","start_line":1}), |_| {
        Ok(true)
    });
    assert_eq!(page["next_line"], 201);
    assert_eq!(page["lines"].as_array().unwrap().len(), 200);
}

#[test]
fn executable_and_arguments_require_approval_and_record_exit_status() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let command = json!({"program":"rustc","args":["--version"]});
    let denied = invoke(&root, "command", command.clone(), |action| {
        let Approval::Command {
            directory,
            program,
            args,
        } = action
        else {
            panic!("expected a command");
        };
        assert_eq!(directory, root);
        assert_eq!(program, "rustc");
        assert_eq!(args, &["--version"]);
        Ok(false)
    });
    assert_eq!(denied["denied"], true);
    let executed = invoke(&root, "command", command, |_| Ok(true));
    assert_eq!(executed["exit_code"], 0, "{executed}");
    assert!(executed["stdout"].as_str().unwrap().contains("rustc"));
    let missing = invoke(
        &root,
        "command",
        json!({"program":"nonexistent-syzygy-test-executable","args":[]}),
        |_| Ok(true),
    );
    assert!(missing["error"].is_string());
}
