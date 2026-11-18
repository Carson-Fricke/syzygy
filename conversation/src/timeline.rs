use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::{
    Error, Event, ModelItem, ModelReply, OutputDelta, Provider, Record, RunOutcome, Store,
    ToolCall, ToolOutput, ToolResult,
};

const PAGE_SIZE: usize = 128;
const MAX_MODEL_STEPS: usize = 32;

pub struct Timeline<S> {
    store: S,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Empty,
    Runnable,
    Complete,
    Uncertain { call_id: String },
}

impl<S: Store> Timeline<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn chats(&self) -> Result<Vec<String>, Error> {
        self.store.chats()
    }

    pub fn events(&self, chat: &str, after: u64, limit: usize) -> Result<Vec<Record>, Error> {
        self.store.read_after(chat, after, limit)
    }

    pub fn binding(&self, chat: &str) -> Result<Option<Value>, Error> {
        let first = self.store.read_after(chat, 0, 1)?.pop();
        Ok(match first.map(|record| record.event) {
            Some(Event::Bound { binding }) => Some(binding),
            _ => None,
        })
    }

    /// Bind credentials-free host settings before the first turn. Subsequent
    /// opens verify the original binding, including concurrent first opens.
    pub fn bind(&mut self, chat: &str, binding: &Value) -> Result<(), Error> {
        require_id(chat)?;
        let event = Event::Bound {
            binding: binding.clone(),
        };
        match self.store.compare_and_append(chat, 0, "", &event) {
            Err(Error::Conflict) if self.binding(chat)?.as_ref() == Some(binding) => Ok(()),
            result => result.map(|_| ()),
        }
    }

    pub fn status(&self, chat: &str) -> Result<Status, Error> {
        let state = self.load(chat)?;
        Ok(match state.next() {
            Next::Empty => Status::Empty,
            Next::Complete => Status::Complete,
            Next::Uncertain(call_id) => Status::Uncertain {
                call_id: call_id.to_owned(),
            },
            Next::Model | Next::Tool(_) => Status::Runnable,
        })
    }

    /// Atomic input admission. The same turn identity and text returns the
    /// original sequence, even if a worker has already advanced that turn.
    pub fn start(&mut self, chat: &str, turn_id: &str, text: &str) -> Result<u64, Error> {
        require_id(chat)?;
        require_id(turn_id)?;
        if text.trim().is_empty() {
            return Err(Error::Invalid("user input is empty".into()));
        }
        let mut state = self.load(chat)?;
        if let Some(previous) = state.input_sequence(turn_id, text)? {
            return Ok(previous);
        }
        let event = Event::User {
            text: text.to_owned(),
        };
        match self.record(chat, &mut state, turn_id, event) {
            Err(Error::Conflict) => self
                .load(chat)?
                .input_sequence(turn_id, text)?
                .ok_or(Error::Conflict),
            result => result,
        }
    }

    /// Resume from the last durable transition. The executor receives a call
    /// only after this worker successfully committed its claim. Known refusals
    /// and known failures can return a model-visible ToolOutput; an executor
    /// error leaves the claim outstanding for explicit reconciliation.
    pub fn run(
        &mut self,
        chat: &str,
        provider: &mut impl Provider,
        execute: &mut impl FnMut(&ToolCall) -> Result<ToolOutput, String>,
        emit: &mut dyn FnMut(OutputDelta),
    ) -> Result<RunOutcome, Error> {
        let mut state = self.load(chat)?;
        let turn_id = state.turn.clone();
        let mut model_steps = 0;
        loop {
            match state.next() {
                Next::Empty => return Err(Error::Invalid("no user turn to run".into())),
                Next::Complete => return Ok(RunOutcome::Complete),
                Next::Uncertain(call_id) => {
                    return Ok(RunOutcome::Uncertain {
                        call_id: call_id.to_owned(),
                    });
                }
                Next::Model => {
                    if model_steps >= MAX_MODEL_STEPS {
                        return Err(Error::StepLimit);
                    }
                    model_steps += 1;
                    let reply = provider
                        .respond(&state.model, emit)
                        .map_err(|error| Error::Provider(Box::new(error)))?;
                    self.record(chat, &mut state, &turn_id, Event::Assistant { reply })?;
                }
                Next::Tool(call) => {
                    let call = call.clone();
                    let call_id = call.call_id.clone();
                    self.record(
                        chat,
                        &mut state,
                        &turn_id,
                        Event::ToolStarted {
                            call_id: call_id.clone(),
                        },
                    )?;
                    let Ok(output) = execute(&call) else {
                        return Ok(RunOutcome::Uncertain { call_id });
                    };
                    let result = ToolResult { call_id, output };
                    self.record(chat, &mut state, &turn_id, Event::ToolFinished { result })?;
                }
            }
        }
    }

    /// Settle a claimed invocation using its externally established outcome.
    /// The caller must coordinate with any live executor and must not run the
    /// operation again to discover its result.
    pub fn resolve(&mut self, chat: &str, call_id: &str, output: ToolOutput) -> Result<(), Error> {
        let mut state = self.load(chat)?;
        let result = ToolResult {
            call_id: call_id.to_owned(),
            output,
        };
        if state.check_settled(&result)? {
            return Ok(());
        }
        let turn_id = state.turn.clone();
        match self.record(
            chat,
            &mut state,
            &turn_id,
            Event::ToolFinished {
                result: result.clone(),
            },
        ) {
            Err(Error::Conflict) if self.load(chat)?.check_settled(&result)? => Ok(()),
            other => other.map(|_| ()),
        }
    }

    fn load(&self, chat: &str) -> Result<State, Error> {
        let mut state = State::default();
        loop {
            let page = self.store.read_after(chat, state.sequence, PAGE_SIZE)?;
            let count = page.len();
            for record in page {
                if state.sequence.checked_add(1) != Some(record.sequence) {
                    return Err(Error::Invalid(
                        "non-contiguous conversation timeline".into(),
                    ));
                }
                state.apply(&record.turn_id, record.event)?;
            }
            if count < PAGE_SIZE {
                return Ok(state);
            }
        }
    }

    fn record(
        &mut self,
        chat: &str,
        state: &mut State,
        turn_id: &str,
        event: Event,
    ) -> Result<u64, Error> {
        let expected = state.sequence;
        state.apply(turn_id, event.clone())?;
        let sequence = self
            .store
            .compare_and_append(chat, expected, turn_id, &event)?;
        if sequence != state.sequence {
            return Err(Error::Invalid(
                "store acknowledged a different timeline position".into(),
            ));
        }
        Ok(sequence)
    }
}

enum Next<'a> {
    Empty,
    Complete,
    Model,
    Tool(&'a ToolCall),
    Uncertain(&'a str),
}

#[derive(Default)]
struct State {
    sequence: u64,
    turn: String,
    model: Vec<ModelItem>,
    // Index into model and original durable sequence for each accepted user turn.
    turns: HashMap<String, (usize, u64)>,
    call_ids: HashSet<String>,
    reply_ids: HashSet<String>,
    claimed: Option<String>,
}

impl State {
    fn input_sequence(&self, id: &str, text: &str) -> Result<Option<u64>, Error> {
        let Some(&(index, sequence)) = self.turns.get(id) else {
            return Ok(None);
        };
        match &self.model[index] {
            ModelItem::User(saved) if saved == text => Ok(Some(sequence)),
            _ => Err(Error::Conflict),
        }
    }

    fn next(&self) -> Next<'_> {
        if let Some(claim) = &self.claimed {
            return Next::Uncertain(claim);
        }
        match self.model.last() {
            None => Next::Empty,
            Some(ModelItem::User(_) | ModelItem::Developer(_)) => Next::Model,
            Some(ModelItem::Assistant { reply, results }) => {
                if reply.calls.is_empty() {
                    Next::Complete
                } else if let Some(call) = reply.calls.get(results.len()) {
                    Next::Tool(call)
                } else {
                    Next::Model
                }
            }
        }
    }

    fn check_settled(&self, result: &ToolResult) -> Result<bool, Error> {
        let previous = self
            .model
            .iter()
            .filter_map(|item| match item {
                ModelItem::Assistant { results, .. } => Some(results.as_slice()),
                ModelItem::User(_) | ModelItem::Developer(_) => None,
            })
            .flatten()
            .find(|previous| previous.call_id == result.call_id);
        match previous {
            Some(previous) if previous == result => Ok(true),
            Some(_) => Err(Error::Conflict),
            None => Ok(false),
        }
    }

    fn apply(&mut self, turn_id: &str, event: Event) -> Result<(), Error> {
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("conversation sequence exhausted".into()))?;
        match event {
            Event::Bound { .. } if self.sequence == 0 && turn_id.is_empty() => {}
            Event::User { text } => {
                require_id(turn_id)?;
                if !matches!(self.next(), Next::Complete | Next::Empty) {
                    return Err(Error::Invalid(
                        "finish the pending turn before starting another".into(),
                    ));
                }
                if text.trim().is_empty() || self.turns.contains_key(turn_id) {
                    return Err(Error::Conflict);
                }
                self.turns
                    .insert(turn_id.to_owned(), (self.model.len(), sequence));
                self.model.push(ModelItem::User(text));
                self.turn = turn_id.to_owned();
            }
            _ if self.turn.is_empty() || self.turn != turn_id => return Err(Error::Conflict),
            Event::Assistant { reply } if matches!(self.next(), Next::Model) => {
                self.add_reply(reply)?;
            }
            Event::ToolStarted { call_id } if matches!(self.next(), Next::Tool(call) if call.call_id == call_id) =>
            {
                self.claimed = Some(call_id);
            }
            Event::ToolFinished { result } if self.claimed.as_deref() == Some(&result.call_id) => {
                let Some(ModelItem::Assistant { results, .. }) = self.model.last_mut() else {
                    return Err(Error::Conflict);
                };
                results.push(result);
                self.claimed = None;
            }
            _ => {
                return Err(Error::Invalid(
                    "event does not follow the current turn state".into(),
                ));
            }
        }
        self.sequence = sequence;
        Ok(())
    }

    fn add_reply(&mut self, reply: ModelReply) -> Result<(), Error> {
        require_id(&reply.id)?;
        if !self.reply_ids.insert(reply.id.clone()) {
            return Err(Error::Invalid("provider reused a response identity".into()));
        }
        crate::validate_calls(&reply.calls, self.call_ids.iter().map(String::as_str))?;
        self.call_ids
            .extend(reply.calls.iter().map(|call| call.call_id.clone()));
        self.model.push(ModelItem::Assistant {
            reply,
            results: Vec::new(),
        });
        Ok(())
    }
}

fn require_id(id: &str) -> Result<(), Error> {
    if id.trim().is_empty() {
        Err(Error::Invalid(
            "conversation identities must be nonempty".into(),
        ))
    } else {
        Ok(())
    }
}
