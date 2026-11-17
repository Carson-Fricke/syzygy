use std::collections::HashSet;

use thesis_worker_execution::{Candidate, ReadyQueue};

fn work(
    key: &str,
    resource: &str,
    created_at: &str,
    payload: &'static str,
) -> Candidate<&'static str> {
    Candidate {
        workspace_id: "workspace".into(),
        key: key.into(),
        resource: resource.into(),
        created_at: created_at.into(),
        payload,
    }
}

#[test]
fn repeated_notifications_keep_contention_delays_and_free_slots_run_in_order() {
    let mut queue = ReadyQueue::default();
    queue.retry(work("001", "chat-a", "01", "retry"), 10.0);
    queue.observe(work("001", "chat-a", "01", "refreshed retry"));
    queue.observe(work("004", "chat-b", "03", "later"));
    queue.observe(work("002", "chat-a", "02", "busy"));
    queue.observe(work("003", "chat-b", "02", "oldest ready"));
    let active = HashSet::from([("workspace".into(), "chat-a".into())]);

    let ready = queue.take_ready(9.0, active, 3);
    assert_eq!(
        ready,
        [(("workspace".into(), "chat-b".into()), "oldest ready")]
    );
    assert_eq!(queue.take_ready(9.0, HashSet::new(), 1)[0].1, "busy");
    assert_eq!(
        queue.take_ready(10.0, HashSet::new(), 1)[0].1,
        "refreshed retry"
    );
    assert_eq!(queue.take_ready(10.0, HashSet::new(), 1)[0].1, "later");
    assert!(queue.is_empty());
}
