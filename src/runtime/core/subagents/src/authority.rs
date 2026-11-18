//! Trusted scope and lifecycle decisions for anchored phase deliveries.

#[derive(Debug)]
pub struct AgentScope {
    pub chat_id: String,
    pub parent_id: Option<String>,
    pub execution_id: Option<String>,
    pub attempt_id: Option<String>,
    pub status: String,
    pub phase: Option<String>,
    pub phase_executor: bool,
}

pub enum PhaseDelivery {
    Current,
    Ended,
    IntentChanged,
}

#[must_use]
pub fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

/// A participant can receive delivery only while its named phase executor
/// owns the same attempt under the supplied active workflow execution.
pub fn phase_delivery(
    receiver: Option<&AgentScope>,
    owner: Option<&AgentScope>,
    target: (&str, &str, &str),
    active_phases: &[String],
    execution_running: bool,
    same_intent: bool,
) -> PhaseDelivery {
    let (chat_id, execution_id, attempt_id) = target;
    let (Some(receiver), Some(owner)) = (receiver, owner) else {
        return PhaseDelivery::Ended;
    };
    let matches_attempt = |agent: &AgentScope| {
        agent.chat_id == chat_id
            && agent.execution_id.as_deref() == Some(execution_id)
            && agent.attempt_id.as_deref() == Some(attempt_id)
            && !terminal(&agent.status)
    };
    if !execution_running
        || !matches_attempt(receiver)
        || !matches_attempt(owner)
        || !owner.phase_executor
        || owner.parent_id != receiver.parent_id
        || !owner
            .phase
            .as_ref()
            .is_some_and(|phase| active_phases.contains(phase))
    {
        PhaseDelivery::Ended
    } else if !same_intent {
        PhaseDelivery::IntentChanged
    } else {
        PhaseDelivery::Current
    }
}

pub fn accepts_unsolicited_input(kind: &str, status: &str, accepts_input: bool) -> bool {
    !terminal(status) && (kind == "conversation" || (status == "needs_input" && accepts_input))
}
