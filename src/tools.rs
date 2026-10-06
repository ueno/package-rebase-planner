//! Filesystem tools exposed to the reconciliation agent.

use crate::agent::Session;
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use goose_agent::{
    operation::{Emitter, messages_since_kickoff},
    tool::ToolProvider,
};
use goose_providers::conversation::message::MessageContent;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, JsonObject, Tool,
};
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct Assessment {
    pub ty: CommitType,
    pub impact: usize,
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Recommendation {
    /// The commit should be tested.
    Test {
        /// Passing criteria, in a single sentence.
        criteria: String,
    },
    /// The commit should be documented.
    Document {
        /// Proposed text.
        text: String,
    },
    /// The commit should be reverted.
    Revert {
        /// Reason for the reversion.
        reason: String,
    },
}

impl fmt::Display for Recommendation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        match self {
            Self::Test { criteria } => write!(f, "adding test: {criteria}"),
            Self::Document { text } => write!(f, "adding documentation: \"{text}\""),
            Self::Revert { reason } => write!(f, "reverting because: {reason}"),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Workspace {
    /// Repository root, set when the log should be readable (`prp assess`).
    pub root: PathBuf,
    pub assessments: HashMap<String, Assessment>,
    pub recommendations: HashMap<String, Vec<Recommendation>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReadFileParams {
    /// Git commit hash.
    pub commit: String,
    /// Relative path to the file.
    pub path: PathBuf,
    /// Line number to start reading from (1-based).
    #[schemars(range(min = 1))]
    pub line: Option<usize>,
    /// Maximum number of lines to read.
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReadCommitParams {
    /// Git commit hash.
    pub commit: String,
    /// Line number to start reading from (1-based).
    #[schemars(range(min = 1))]
    pub line: Option<usize>,
    /// Maximum number of lines to read.
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListCommitsParams {
    /// Git commit hash.
    pub commit: String,
    /// The number of commits (default 3).
    pub count: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListCommitsOutput {
    /// Adjacent commits.
    pub commits: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CommitType {
    /// The commit adds a new feature.
    Feat,
    /// The commit represents a bug fix.
    Fix,
    /// The commit refactors the exisiting code including tests.
    Refactor,
    /// The commit adds tests or extends the existing tests.
    Test,
    /// The commit only adds documentation for the existing code
    Docs,
    /// The commit only touches administrative files, such as GitLab CI (.gitlab-ci.yml).
    Chore,
    /// The commit only changes the coding style.
    Style,
    /// The commit is mainly for performance improvements.
    Perf,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WriteAssessmentParams {
    /// Git commit hash.
    pub commit: String,
    /// Commit type.
    #[serde(rename = "type")]
    pub ty: CommitType,
    /// Impact of the commit.
    pub impact: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReadAssessmentParams {
    /// Git commit hash.
    pub commit: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReadAssessmentOutput {
    pub assessment: Option<Assessment>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FinishParams {
    /// The summary of the task, in prose.
    pub summary: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WriteRecommendationsParams {
    /// Git commit hash.
    pub commit: String,
    /// The recommended actions.
    pub recommendations: Vec<Recommendation>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReadRecommendationsParams {
    /// Git commit hash.
    pub commit: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReadRecommendationsOutput {
    /// The recommended actions.
    pub recommendations: Option<Vec<Recommendation>>,
}

fn parse_args<T: serde::de::DeserializeOwned>(arguments: &Option<JsonObject>) -> Result<T> {
    let value = arguments
        .as_ref()
        .map(Clone::clone)
        .map(Value::Object)
        .ok_or_else(|| anyhow!("Missing arguments"))?;
    Ok(serde_json::from_value(value)?)
}

fn apply_line_limit(content: &str, line: Option<usize>, limit: Option<usize>) -> String {
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    if let Some(line) = line
        && line.saturating_sub(1) > lines.len()
    {
        return "".to_string();
    }
    let start = line.unwrap_or(0).saturating_sub(1).min(lines.len());
    let end = (start + limit.unwrap_or(1000)).min(lines.len());
    lines[start..end].concat()
}

impl Workspace {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if !root.is_dir() {
            bail!("directory {} does not exist", root.display());
        }
        let root = root
            .canonicalize()
            .with_context(|| format!("could not resolve {}", root.display()))?;
        Ok(Self {
            root,
            ..Default::default()
        })
    }

    pub fn read_file(
        &mut self,
        revision: &str,
        path: &Path,
        line: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String> {
        let content = crate::git::read_file(&self.root, revision, path)?;
        let content = apply_line_limit(&content, line, limit);
        Ok(content)
    }

    pub fn read_commit(
        &mut self,
        revision: &str,
        line: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String> {
        let content = crate::git::read_commit(&self.root, revision)?;
        let content = apply_line_limit(&content, line, limit);
        Ok(content)
    }

    pub fn list_commits(&mut self, commit: &str, count: usize) -> Result<Vec<(String, String)>> {
        let commits = crate::git::list_adjacent_commits(&self.root, commit, count)?;
        Ok(commits)
    }

    pub fn write_assessment(&mut self, commit: &str, ty: CommitType, impact: usize) {
        self.assessments.insert(
            commit.to_string(),
            Assessment {
                ty,
                impact,
                rationale: None,
            },
        );
    }

    pub fn read_assessment(&mut self, commit: &str) -> Option<&Assessment> {
        self.assessments.get(commit)
    }

    pub fn write_recommendations(&mut self, commit: &str, recommendations: &[Recommendation]) {
        self.recommendations
            .entry(commit.to_string())
            .or_default()
            .append(&mut recommendations.to_vec());
    }

    pub fn read_recommendations(&mut self, commit: &str) -> Option<&Vec<Recommendation>> {
        self.recommendations.get(commit)
    }
}

struct Tools {
    tools: Vec<Tool>,
}

fn commit_matches(this: &Option<JsonObject>, other: &Option<JsonObject>) -> bool {
    match (this.as_ref(), other.as_ref()) {
        (Some(this), Some(other)) => this.get("commit") == other.get("commit"),
        _ => false,
    }
}

#[async_trait]
impl ToolProvider<Session> for Tools {
    async fn tools(&self, _session: &Session) -> Result<Vec<Tool>> {
        Ok(self.tools.clone())
    }

    async fn call(
        &self,
        session: &Session,
        _request_id: &str,
        call: CallToolRequestParams,
        _emit: &Emitter,
    ) -> Result<CallToolResult, ErrorData> {
        if !self.tools.iter().any(|tool| tool.name == call.name) {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "Error: unsupported tool call: {}",
                &call.name,
            ))]));
        }

        let messages = messages_since_kickoff(&session.conversation).map_err(|error| {
            ErrorData::internal_error(
                format!("unable to retrieve messages from conversation: {error}"),
                None,
            )
        })?;
        let count = messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(MessageContent::as_tool_request)
            .filter_map(|request| request.tool_call.as_ref().ok())
            .filter(|other| {
                call.name == other.name && commit_matches(&call.arguments, &other.arguments)
            })
            .count();
        if count >= 3 {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "Error: {} called {count} times on the same commit",
                &call.name,
            ))]));
        }

        session.reporter.tool_call(&call);

        let result = dispatch(&session, &call).unwrap_or_else(|error| {
            let text = format!("Error: {error:#}");
            CallToolResult::error(vec![ContentBlock::text(text)])
        });

        session.reporter.tool_result(&result);

        Ok(result)
    }
}

fn tool(name: &'static str, description: &'static str, input_schema: Schema) -> Tool {
    let input_schema = serde_json::to_value(input_schema)
        .expect("schema serialization should succeed")
        .as_object()
        .expect("schema should serialize to an object")
        .clone();
    Tool::new(name, description, input_schema)
}

/// Tools for answering an assessment question
pub fn assess_tool_provider() -> Arc<dyn ToolProvider<Session>> {
    Arc::new(Tools {
        tools: vec![
            tool(
                "read_file",
                "Read file at revision.",
                schema_for!(ReadFileParams),
            ),
            tool(
                "read_commit",
                "Read a commit's full contents, including its frontmatter.",
                schema_for!(ReadCommitParams),
            ),
            tool(
                "list_commits",
                "List adjacent commits, up to given count.",
                schema_for!(ListCommitsParams),
            ),
            tool(
                "write_assessment",
                "Classify the given commit as `type` and grade the impact of it as `impact`.",
                schema_for!(WriteAssessmentParams),
            ),
            tool(
                "read_assessment",
                "Read an existing assessment of the given commit, if any.",
                schema_for!(ReadAssessmentParams),
            ),
            tool(
                "finish",
                "Call when you have completed the task. Summarize what you did and why.",
                schema_for!(FinishParams),
            )
            .with_output_schema::<()>(),
        ],
    })
}

/// Tools for answering a planning question
pub fn plan_tool_provider() -> Arc<dyn ToolProvider<Session>> {
    Arc::new(Tools {
        tools: vec![
            tool(
                "read_commit",
                "Read a commit's full contents, including its frontmatter.",
                schema_for!(ReadCommitParams),
            ),
            tool(
                "list_commits",
                "List adjacent commits, up to given count.",
                schema_for!(ListCommitsParams),
            )
            .with_output_schema::<ListCommitsOutput>(),
            tool(
                "read_assessment",
                "Read an existing assessment of the given commit, if any.",
                schema_for!(ReadAssessmentParams),
            )
            .with_output_schema::<ReadAssessmentOutput>(),
            tool(
                "write_recommendations",
                "Write recommended actions for the given commit.",
                schema_for!(WriteRecommendationsParams),
            ),
            tool(
                "read_recommendations",
                "Read existing recommendions for the given commit, if any.",
                schema_for!(ReadRecommendationsParams),
            )
            .with_output_schema::<ReadRecommendationsOutput>(),
            tool(
                "finish",
                "Call when you have completed the task. Summarize what you did and why.",
                schema_for!(FinishParams),
            ),
        ],
    })
}

fn dispatch(session: &Session, call: &CallToolRequestParams) -> Result<CallToolResult> {
    match call.name.to_string().as_str() {
        "read_file" => {
            let params = parse_args::<ReadFileParams>(&call.arguments)?;
            let mut workspace = session.workspace.lock().unwrap();
            let text =
                workspace.read_file(&params.commit, &params.path, params.line, params.limit)?;
            let text = if text.is_empty() {
                "No more lines".to_string()
            } else {
                text
            };
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
        }
        "read_commit" => {
            let params = parse_args::<ReadCommitParams>(&call.arguments)?;
            let mut workspace = session.workspace.lock().unwrap();
            let text = workspace.read_commit(&params.commit, params.line, params.limit)?;
            let text = if text.is_empty() {
                "No more lines".to_string()
            } else {
                text
            };
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
        }
        "list_commits" => {
            let params = parse_args::<ListCommitsParams>(&call.arguments)?;
            let mut workspace = session.workspace.lock().unwrap();
            let commits = workspace.list_commits(&params.commit, params.count.unwrap_or(3))?;
            Ok(CallToolResult::structured(serde_json::json!(commits)))
        }
        "write_assessment" => {
            let params = parse_args::<WriteAssessmentParams>(&call.arguments)?;
            let mut workspace = session.workspace.lock().unwrap();
            workspace.write_assessment(&params.commit, params.ty, params.impact);
            let text = format!(
                "Assessed {} as {:?}:{}.",
                &params.commit, params.ty, params.impact
            );
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
        }
        "read_assessment" => {
            let params = parse_args::<ReadAssessmentParams>(&call.arguments)?;
            let mut workspace = session.workspace.lock().unwrap();
            Ok(CallToolResult::structured(serde_json::json!(
                workspace.read_assessment(&params.commit)
            )))
        }
        "write_recommendations" => {
            let params = parse_args::<WriteRecommendationsParams>(&call.arguments)?;
            let mut workspace = session.workspace.lock().unwrap();
            workspace.write_recommendations(&params.commit, &params.recommendations);
            let recommendations: Vec<_> = params
                .recommendations
                .iter()
                .map(|recommendation| format!("- {recommendation}"))
                .collect();
            let text = format!(
                "Recommended for {}:\n{}",
                &params.commit,
                recommendations.join("\n"),
            );
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
        }
        "read_recommendations" => {
            let params = parse_args::<ReadRecommendationsParams>(&call.arguments)?;
            let mut workspace = session.workspace.lock().unwrap();
            Ok(CallToolResult::structured(serde_json::json!(
                workspace.read_recommendations(&params.commit)
            )))
        }
        "finish" => {
            let params = parse_args::<FinishParams>(&call.arguments)?;
            session.reporter.finished(&params.summary);
            session.record.lock().unwrap().result = Some(params.summary.clone());
            Ok(CallToolResult::success(vec![]))
        }
        other => bail!("unknown tool `{other}`"),
    }
}
