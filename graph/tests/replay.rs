use indexmap::IndexMap;
use thesis_workflow_graph::{AcceptedCompletion, Destination, Node, Outcome, Topology, replay};

fn activity(name: &str, route: &str, targets: &[(&str, &str)]) -> Node {
    Node {
        name: name.into(),
        inputs: vec!["input".into()],
        routes: IndexMap::from([(
            route.into(),
            targets
                .iter()
                .map(|(node, port)| Destination {
                    node: (*node).into(),
                    port: (*port).into(),
                })
                .collect(),
        )]),
        outcome: None,
    }
}

fn receipt(sequence: u64, phase: &str, route: &str, attempt: &str) -> AcceptedCompletion {
    AcceptedCompletion {
        sequence,
        from_phase: phase.into(),
        route: route.into(),
        attempt_id: attempt.into(),
    }
}

#[test]
fn retained_attempt_identity_and_ordered_join_references_survive_replay() {
    let nodes = Topology::from([
        (
            "start".into(),
            activity("start", "open", &[("right", "input"), ("left", "input")]),
        ),
        ("left".into(), activity("left", "done", &[("end", "left")])),
        (
            "right".into(),
            activity("right", "done", &[("end", "right")]),
        ),
        (
            "end".into(),
            Node {
                name: "end".into(),
                inputs: vec!["left".into(), "right".into()],
                routes: IndexMap::new(),
                outcome: Some(Outcome::Completed),
            },
        ),
    ]);
    let mut history = vec![receipt(
        1,
        "start",
        "open",
        "attempt-2979656710c680ea7eeb2697f18ebd07",
    )];
    let opened = replay("execution", "start", &nodes, &history).unwrap();
    assert_eq!(
        opened.positions.keys().collect::<Vec<_>>(),
        ["right", "left"]
    );
    history.push(receipt(
        2,
        "right",
        "done",
        &opened.positions["right"].attempt_id,
    ));
    let waiting = replay("execution", "start", &nodes, &history).unwrap();
    assert!(!waiting.terminal_ready);
    assert_eq!(waiting.pending["end"].arrived, ["right"]);
    assert_eq!(waiting.pending["end"].required, ["left", "right"]);
    history.push(receipt(
        3,
        "left",
        "done",
        &opened.positions["left"].attempt_id,
    ));
    let joined = replay("execution", "start", &nodes, &history).unwrap();
    let terminal = &joined.positions["end"];
    assert_eq!(
        terminal.inputs.keys().collect::<Vec<_>>(),
        ["left", "right"]
    );
    assert_eq!(terminal.inputs["left"], 3);
    assert_eq!(terminal.inputs["right"], 2);
    assert_eq!(terminal.history, [1]);
    assert_eq!(terminal.dependencies, [1, 2, 3]);
    assert!(!joined.has_open_fanout);
    assert!(joined.terminal_ready);
}
