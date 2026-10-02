//! The agent loop, driven by `goose_agent`'s `StateMachine`.
//!
//! Operations are ours because goose's are not public API yet. The machine tries
//! each step in order per iteration; the first that applies produces effects and
//! the loop repeats.
//!
//! One sharp edge: `StateMachine::step` always reaches the inference step and
//! does not consult `Inference::applies`, so a prose reply with no tool calls
//! would loop forever without [`EndTurnOperation`] ahead of it.

use crate::model::{Backend, Usage};
use crate::progress::Reporter;
use crate::tools::{self, Workspace};
use anyhow::Result;
use async_trait::async_trait;
use goose_agent::machine::{EffectHandler, MachineSession, SessionLoader, StateMachine, Step};
use goose_agent::operation::{
    ConversationEffect, Emitter, Inference, InferenceInput, Operation, OperationResult, applied,
    ends_turn, not_applicable, yielded,
};
use goose_agent::tool::{ToolOperation, ToolProvider};
use goose_providers::conversation::Conversation;
use goose_providers::conversation::message::Message;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// What the agent is for. Currently only assess and plan are defined.
#[derive(Clone, Copy)]
pub struct Role {
    /// Tools offered to the model.
    pub tools: fn() -> Arc<dyn ToolProvider<Session>>,
    /// Instructions.
    pub system: &'static str,
    /// Give up after this many turns.
    pub max_turns: usize,
    /// Start warning the model when this many turns remain.
    pub warn_at: usize,
    /// How to phrase the warning. `{remaining}` is substituted.
    pub warning: &'static str,
    /// What running out means for this role.
    pub exhausted: &'static str,
}

pub const ASSESS: Role = Role {
    tools: tools::assess_tool_provider,
    system: crate::prompts::ASSESS,
    max_turns: 32,
    warn_at: 4,
    exhausted: "I could not assess the commit.",
    warning: "You have {remaining} retries left before this run is stopped. \
              Write your assessment even if you are not completely sure. \
              Then complete the task by calling `finish`.",
};

pub const PLAN: Role = Role {
    tools: tools::plan_tool_provider,
    system: crate::prompts::PLAN,
    max_turns: 32,
    warn_at: 4,
    exhausted: "I could not come up with an action plan.",
    warning: "You have {remaining} retries left before this run is stopped. \
              Write your plan even if you are not completely sure. \
              Then complete the task by calling `finish`.",
};

#[derive(Debug)]
pub struct Outcome {
    /// The terminal tool's result: a summary of changes.
    pub result: String,
    pub usage: Usage,
    pub turns: usize,
}

#[derive(Default)]
pub struct Record {
    pub result: Option<String>,
    usage: Usage,
    turns: usize,
    /// Remaining-turn count at the last warning, so each is issued once.
    warned_at: Option<usize>,
}

/// Everything a step may touch. `StateMachine` hands `&Session` to each
/// operation, so shared mutable state lives behind mutexes.
pub struct Session {
    id: String,
    conversation: Conversation,
    pub workspace: Arc<Mutex<Workspace>>,
    pub record: Arc<Mutex<Record>>,
    pub reporter: Arc<Reporter>,
    role: Role,
}

impl MachineSession for Session {
    fn id(&self) -> &str {
        &self.id
    }
    fn conversation(&self) -> Option<&Conversation> {
        Some(&self.conversation)
    }
}

/// Owns the conversation between machine iterations: `load` is called at the top
/// of each pass and `apply_effects` at the end, so the transcript lives here
/// rather than being threaded through the loop.
struct Runtime {
    conversation: Mutex<Conversation>,
    workspace: Arc<Mutex<Workspace>>,
    record: Arc<Mutex<Record>>,
    reporter: Arc<Reporter>,
    role: Role,
}

#[async_trait]
impl SessionLoader<Session> for Runtime {
    async fn load(&self, session_id: &str) -> Result<Session> {
        Ok(Session {
            id: session_id.to_string(),
            conversation: self.conversation.lock().unwrap().clone(),
            workspace: self.workspace.clone(),
            record: self.record.clone(),
            reporter: self.reporter.clone(),
            role: self.role,
        })
    }
}

#[async_trait]
impl EffectHandler<Session, ConversationEffect> for Runtime {
    async fn apply_effects(
        &self,
        _session: &Session,
        effects: &mut [ConversationEffect],
        _emit: &Emitter,
    ) -> Result<()> {
        let mut conversation = self.conversation.lock().unwrap();
        for effect in effects {
            match effect {
                ConversationEffect::AppendMessage(message) => conversation.push(message.clone()),
                ConversationEffect::ReplaceConversation(replacement) => {
                    *conversation = replacement.clone()
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Stop before an agent runs away, and warn it as the limit approaches.
///
/// Hitting the limit is a failure: the run is discarded and the user waited for
/// nothing. Telling the model how many turns remain lets it land somewhere
/// useful instead.
/// Without this a small model will happily search until it exhausts its context.
struct MaxTurnsOperation;

#[async_trait]
impl Operation<Session, ConversationEffect> for MaxTurnsOperation {
    fn name(&self) -> &'static str {
        "max_turns"
    }

    async fn run(
        &self,
        session: &Session,
        conversation: &Conversation,
        _emit: &Emitter,
    ) -> Result<OperationResult<ConversationEffect>> {
        let role = session.role;
        let mut record = session.record.lock().unwrap();
        if record.turns >= role.max_turns {
            return {
                if record.result.is_none() {
                    record.result = Some(role.exhausted.to_string());
                }
                drop(record);
                yielded()
            };
        }

        let remaining = role.max_turns - record.turns;
        if remaining > role.warn_at || record.warned_at == Some(remaining) {
            return not_applicable();
        }
        drop(record);

        // Never nag a model that has stopped asking for tools. Warning after a
        // prose reply restarts a finished conversation, and the model — having
        // already answered — spends the rest of its budget saying so.
        if ends_turn(conversation.messages()) {
            return not_applicable();
        }
        let mut record = session.record.lock().unwrap();
        // Once per turn, not once per run: a model that ignores the first
        // warning should see the number fall.
        record.warned_at = Some(remaining);
        drop(record);

        let text = role.warning.replace("{remaining}", &remaining.to_string());
        session.reporter.warn(&text);
        applied([ConversationEffect::AppendMessage(
            Message::user().agent_visible_content().with_text(text),
        )])
    }
}

/// The model called the terminal tool: record its result and stop.
struct TerminalOperation;

#[async_trait]
impl Operation<Session, ConversationEffect> for TerminalOperation {
    fn name(&self) -> &'static str {
        "terminal"
    }

    async fn run(
        &self,
        session: &Session,
        _conversation: &Conversation,
        _emit: &Emitter,
    ) -> Result<OperationResult<ConversationEffect>> {
        if session.record.lock().unwrap().result.is_some() {
            yielded()
        } else {
            not_applicable()
        }
    }
}

/// The model replied with prose and no tool calls, so it has nothing further to
/// do. Take its words as the result rather than discarding them.
struct EndTurnOperation;

#[async_trait]
impl Operation<Session, ConversationEffect> for EndTurnOperation {
    fn name(&self) -> &'static str {
        "end_turn"
    }

    async fn run(
        &self,
        session: &Session,
        conversation: &Conversation,
        _emit: &Emitter,
    ) -> Result<OperationResult<ConversationEffect>> {
        if !ends_turn(conversation.messages()) {
            return not_applicable();
        }
        let mut record = session.record.lock().unwrap();
        if record.result.is_none() {
            let text = conversation
                .messages()
                .last()
                .map(|message| message.as_concat_text())
                .unwrap_or_default();
            record.result = Some(text.trim().to_string());
        }
        yielded()
    }
}

/// Ask the model what to do next.
struct InferenceRunner {
    backend: Arc<dyn Backend>,
}

#[async_trait]
impl Operation<Session, ConversationEffect> for InferenceRunner {
    fn name(&self) -> &'static str {
        "inference"
    }
}

#[async_trait]
impl Inference<Session, ConversationEffect> for InferenceRunner {
    fn applies(&self, conversation: &Conversation) -> bool {
        !ends_turn(conversation.messages())
    }

    async fn infer(
        &self,
        session: &Session,
        conversation: &Conversation,
        input: InferenceInput,
        _emit: &Emitter,
    ) -> Result<OperationResult<ConversationEffect>> {
        let next_turn = session.record.lock().unwrap().turns + 1;
        session.reporter.thinking(next_turn);

        let (reply, usage) = self
            .backend
            .complete(session.role.system, conversation.messages(), &input.tools)
            .await?;
        session.reporter.model_text(&reply.as_concat_text());

        let mut record = session.record.lock().unwrap();
        record.turns += 1;
        if let Some(input_tokens) = usage.input_tokens {
            *record.usage.input_tokens.get_or_insert(0) += input_tokens;
        }
        if let Some(output_tokens) = usage.output_tokens {
            *record.usage.output_tokens.get_or_insert(0) += output_tokens;
        }
        drop(record);

        applied([ConversationEffect::AppendMessage(reply)])
    }
}

pub struct Agent {
    backend: Arc<dyn Backend>,
    role: Role,
    pub reporter: Arc<Reporter>,
}

impl Agent {
    pub fn new(backend: Arc<dyn Backend>, role: Role) -> Self {
        Self {
            backend,
            role,
            reporter: Arc::new(Reporter::silent()),
        }
    }

    /// Run until the model finishes, gives up, or exceeds its turn limit.
    ///
    /// Takes the workspace by value and hands it back, so a caller can inspect
    /// what happened without sharing mutable state with the loop.
    pub fn run(
        &self,
        session_id: &str,
        workspace: Arc<Mutex<Workspace>>,
        opening: &str,
    ) -> Result<Outcome> {
        let record = Arc::new(Mutex::new(Record::default()));

        let runtime = Runtime {
            conversation: Mutex::new(Conversation::new_unvalidated([
                Message::user().with_text(opening)
            ])),
            workspace: workspace.clone(),
            record: record.clone(),
            reporter: self.reporter.clone(),
            role: self.role,
        };

        let steps: Vec<Step<'_, Session, ConversationEffect>> = vec![
            Step::Operation(Arc::new(MaxTurnsOperation)),
            Step::Operation(Arc::new(TerminalOperation)),
            Step::Operation(Arc::new(EndTurnOperation)),
            Step::Operation(Arc::new(
                ToolOperation::new().with_provider((self.role.tools)()),
            )),
            Step::Inference(Arc::new(InferenceRunner {
                backend: self.backend.clone(),
            })),
        ];

        let cancel = CancellationToken::new();
        let machine = StateMachine::new(steps, cancel.clone());
        let (tx, mut rx) = mpsc::channel(64);
        let emitter = Emitter::new(tx, cancel);

        // The machine is async; the CLI is not.
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let result = machine.run(&runtime, session_id, &emitter).await;
                rx.close();
                while rx.recv().await.is_some() {}
                result
            })?;

        // `record` holds clones of these, so copy the data out.
        let (result, usage, turns) = {
            let record = record.lock().unwrap();
            (record.result.clone(), record.usage, record.turns)
        };

        Ok(Outcome {
            result: result.unwrap_or_else(|| "(no result)".to_string()),
            usage,
            turns,
        })
    }
}
