use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use anyhow::{Result, bail};
use async_trait::async_trait;
use fiasco::{
    agent::runner::{AgentRunner, AgentRunnerConfig, RunRequest, RunnerOptions},
    artifact::ArtifactStore,
    events::{NoopEventSink, SharedEventSink},
    hooks::HookPipeline,
    memory::MemoryPaths,
    model::{
        Message, MessageContent, ModelProvider, ModelRequest, ModelResponse, ModelUsage, Role,
        ToolCall, ToolSpec,
    },
    storage::RunDirStore,
    tools::{BashTool, RawToolOutput, ReadTool, Tool, ToolContext, ToolRegistry, WriteTool},
    trajectory::TrajectoryMessage,
};
use serde::Serialize;
use serde_json::{Value, json};
use tempfile::TempDir;

struct MarkerTool;

#[async_trait]
impl Tool for MarkerTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "marker".to_owned(),
            description: "Return a labelled marker".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {"label": {"type": "string"}},
                "required": ["label"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, _context: ToolContext, arguments: Value) -> Result<RawToolOutput> {
        Ok(RawToolOutput::text(arguments.to_string()))
    }
}

struct OptionalCommandTool {
    name: &'static str,
}

#[async_trait]
impl Tool for OptionalCommandTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.to_owned(),
            description: format!("Test-only {} command", self.name),
            input_schema: json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, _context: ToolContext, _arguments: Value) -> Result<RawToolOutput> {
        Ok(RawToolOutput::text("unused"))
    }
}

#[derive(Default)]
struct CapturingFinalProvider {
    requests: Mutex<Vec<ModelRequest>>,
}

#[async_trait]
impl ModelProvider for CapturingFinalProvider {
    fn name(&self) -> &str {
        "capturing-final"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        self.requests.lock().unwrap().push(request);
        Ok(final_response("done"))
    }
}

#[derive(Default)]
struct NoCompactedHistoryProvider {
    normal_calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
}

#[async_trait]
impl ModelProvider for NoCompactedHistoryProvider {
    fn name(&self) -> &str {
        "no-compacted-history"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        self.requests.lock().unwrap().push(request);
        match self.normal_calls.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(ModelResponse::new(
                Message::assistant(vec![MessageContent::ToolCall(ToolCall {
                    id: "call-empty-history".to_owned(),
                    name: "fiasco".to_owned(),
                    arguments: json!({"command": "history search pattern=anything"}).into(),
                })]),
                ModelUsage::default(),
            )),
            1 => Ok(final_response("empty history confirmed")),
            unexpected => bail!("unexpected no-compacted-history model call {unexpected}"),
        }
    }
}

#[derive(Default)]
struct ProfileContractProvider {
    requests: Mutex<Vec<ModelRequest>>,
}

#[async_trait]
impl ModelProvider for ProfileContractProvider {
    fn name(&self) -> &str {
        "profile-contract"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        let prompt = user_prompt(&request).unwrap_or_default().to_owned();
        let already_delegated = request.messages.iter().any(|message| {
            has_tool_result(message, "delegate-delegating")
                || has_tool_result(message, "delegate-leaf")
                || has_tool_result(message, "delegate-depth-zero")
        });
        self.requests.lock().unwrap().push(request);

        match prompt.as_str() {
            "root profile contract" if !already_delegated => Ok(delegate_response(
                "delegate-delegating",
                "delegating profile contract",
            )),
            "delegating profile contract" if !already_delegated => {
                Ok(delegate_response("delegate-leaf", "leaf profile contract"))
            }
            "leaf profile contract" if !already_delegated => Ok(delegate_response(
                "delegate-depth-zero",
                "must not start beyond the depth limit",
            )),
            "root profile contract" | "delegating profile contract" | "leaf profile contract" => {
                Ok(final_response(&format!("finished {prompt}")))
            }
            unexpected => bail!("unexpected profile-contract prompt `{unexpected}`"),
        }
    }
}

fn final_response(text: &str) -> ModelResponse {
    ModelResponse::new(Message::text(Role::Assistant, text), ModelUsage::default())
}

fn delegate_response(id: &str, prompt: &str) -> ModelResponse {
    ModelResponse::new(
        Message::assistant(vec![MessageContent::ToolCall(ToolCall {
            id: id.to_owned(),
            name: "fiasco".to_owned(),
            arguments: json!({
                "command": "agent start name=delegated_child prompt=-",
                "stdin": prompt,
            })
            .into(),
        })]),
        ModelUsage::default(),
    )
}

#[tokio::test]
async fn two_identical_root_runs_have_byte_identical_stable_prefixes() {
    let workspace = TempDir::new().unwrap();
    let provider = Arc::new(CapturingFinalProvider::default());
    let runner = runner(
        workspace.path(),
        provider.clone(),
        None,
        RunnerOptions::default(),
    );

    runner
        .run(RunRequest::root("identical root prompt"))
        .await
        .unwrap();
    runner
        .run(RunRequest::root("identical root prompt"))
        .await
        .unwrap();

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| tool_names(request).contains(&"fiasco"))
    );
    assert_eq!(
        serialized(&requests[0].system),
        serialized(&requests[1].system)
    );
    assert_eq!(
        serialized(&requests[0].tools),
        serialized(&requests[1].tools)
    );
    assert_eq!(
        serialized(&requests[0].messages[0]),
        serialized(&requests[1].messages[0])
    );
    let names = tool_names(&requests[0]);
    assert_eq!(names, ["bash", "fiasco", "marker", "read", "write"]);
    for tool_name in [
        "`delegate`",
        "history_search",
        "history_read",
        "list_handles",
        "send_message",
    ] {
        assert!(
            !requests[0].system.contains(tool_name),
            "stable system prompt names tool `{tool_name}`"
        );
    }
    let reminder = text_content(&requests[0].messages[0]);
    assert!(!reminder.contains("<context-management>"));
    assert!(!reminder.contains("<tool-guidance>"));
    assert!(!reminder.contains("history_search"));
}

#[tokio::test]
async fn fiasco_schema_and_catalog_are_independent_of_the_base_tool_registry() {
    let workspace = TempDir::new().unwrap();
    let provider = Arc::new(CapturingFinalProvider::default());
    let mut tools = ToolRegistry::default();
    tools.register(Arc::new(MarkerTool)).unwrap();
    tools
        .register(Arc::new(OptionalCommandTool { name: "web_search" }))
        .unwrap();
    tools
        .register(Arc::new(OptionalCommandTool { name: "mcp" }))
        .unwrap();
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: provider.clone(),
        model: "test-model".to_owned(),
        workspace: workspace.path().to_owned(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools,
        artifacts: ArtifactStore::default(),
        store: RunDirStore::new(workspace.path()),
        hooks: HookPipeline::new(),
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions::default(),
    });

    runner
        .run(RunRequest::root("static fiasco schema"))
        .await
        .unwrap();

    let requests = provider.requests.lock().unwrap();
    let fiasco = requests[0]
        .tools
        .iter()
        .find(|tool| tool.name == "fiasco")
        .unwrap();
    assert_eq!(
        fiasco.input_schema.pointer("/required"),
        Some(&json!(["command"]))
    );
    assert!(fiasco.input_schema["properties"]["stdin"].is_object());
    assert_eq!(fiasco.input_schema["additionalProperties"], false);
    assert!(fiasco.description.contains("agent start"));
    assert!(fiasco.description.contains("web search"));
    assert!(fiasco.description.contains("mcp <source> <tool>"));
    assert!(!requests[0].tools.iter().any(|tool| tool.name == "delegate"));
    assert!(
        !requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == "web_search")
    );
    assert!(!requests[0].tools.iter().any(|tool| tool.name == "mcp"));
}

#[tokio::test]
async fn fixed_profiles_expose_exact_schema_sets_at_depth_two() {
    let workspace = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let memory = MemoryPaths::new(home.path(), workspace.path());
    let provider = Arc::new(ProfileContractProvider::default());
    let options = RunnerOptions {
        max_subagent_depth: 2,
        general_task: fiasco::agent::GeneralTaskProfile {
            model: None,
            max_output_tokens: Some(4_096),
        },
        ..RunnerOptions::default()
    };
    let runner = runner(workspace.path(), provider.clone(), Some(memory), options);

    let root_result = runner
        .run(RunRequest::root("root profile contract"))
        .await
        .unwrap();
    let root_record = runner.store().load_run(&root_result.run_id).await.unwrap();
    assert_eq!(root_record.profile, "root");
    assert_eq!(root_record.remaining_delegation_depth, 2);
    let requests = provider.requests.lock().unwrap();
    let root = requests_for_prompt(&requests, "root profile contract");
    let delegating = requests_for_prompt(&requests, "delegating profile contract");
    let leaf = requests_for_prompt(&requests, "leaf profile contract");
    assert!(!root.is_empty());
    assert!(!delegating.is_empty());
    assert!(!leaf.is_empty());

    assert_profile_tools(&root, &["bash", "fiasco", "marker", "read", "write"]);
    assert_profile_tools(&delegating, &["bash", "fiasco", "marker", "read", "write"]);
    assert_profile_tools(&leaf, &["bash", "fiasco", "marker", "read", "write"]);
    assert_eq!(serialized(&root[0].tools), serialized(&delegating[0].tools));
    assert_eq!(serialized(&root[0].tools), serialized(&leaf[0].tools));
    assert_eq!(
        serialized(&root[0].system),
        serialized(&delegating[0].system)
    );
    assert_eq!(serialized(&root[0].system), serialized(&leaf[0].system));
    let root_reminder = text_content(&root[0].messages[0]);
    let delegating_reminder = text_content(&delegating[0].messages[0]);
    let leaf_reminder = text_content(&leaf[0].messages[0]);
    for reminder in [&root_reminder, &delegating_reminder, &leaf_reminder] {
        assert!(reminder.contains("<memory>\nThese ordinary Markdown roots"));
        assert!(reminder.contains("\nuser:"));
        assert!(reminder.contains("project:"));
        assert!(!reminder.contains("memory_update"));
    }
    assert!(root_reminder.contains("profile: root\nremaining delegation depth: 2"));
    assert!(delegating_reminder.contains("profile: general_task\nremaining delegation depth: 1"));
    assert!(leaf_reminder.contains("profile: general_task\nremaining delegation depth: 0"));
    assert!(leaf.iter().any(|request| {
        request.messages.iter().any(|message| {
            message.content.iter().any(|content| {
                matches!(
                    content,
                    MessageContent::ToolResult {
                        call_id,
                        content,
                        is_error: true,
                        ..
                    } if call_id == "delegate-depth-zero"
                        && content.contains("remaining delegation depth is 0")
                )
            })
        })
    }));
}

#[tokio::test]
async fn general_task_inherits_the_root_output_limit_by_default() {
    for expected_limit in [None, Some(8_192)] {
        let workspace = TempDir::new().unwrap();
        let provider = Arc::new(ProfileContractProvider::default());
        let options = RunnerOptions {
            max_subagent_depth: 1,
            max_output_tokens: expected_limit,
            ..RunnerOptions::default()
        };
        let runner = runner(workspace.path(), provider.clone(), None, options);

        runner
            .run(RunRequest::root("root profile contract"))
            .await
            .unwrap();

        let requests = provider.requests.lock().unwrap();
        let root = requests_for_prompt(&requests, "root profile contract");
        let child = requests_for_prompt(&requests, "delegating profile contract");
        assert!(!root.is_empty());
        assert!(!child.is_empty());
        assert!(
            root.iter()
                .chain(child.iter())
                .all(|request| request.max_output_tokens == expected_limit)
        );
    }
}

#[tokio::test]
async fn history_search_before_compaction_returns_an_empty_result() {
    let workspace = TempDir::new().unwrap();
    let provider = Arc::new(NoCompactedHistoryProvider::default());
    let store = RunDirStore::new(workspace.path());
    let runner = runner_with_store(
        workspace.path(),
        provider.clone(),
        None,
        RunnerOptions {
            max_subagent_depth: 0,
            ..RunnerOptions::default()
        },
        store.clone(),
    );

    let result = runner
        .run(RunRequest::root("search before any compaction"))
        .await
        .unwrap();
    assert_eq!(result.final_output, "empty history confirmed");
    let trajectory = store.load_trajectory(&result.run_id).await.unwrap();
    let output = tool_result_content(&trajectory, "call-empty-history").unwrap();
    let output: Value = serde_json::from_str(output).unwrap();
    assert_eq!(output["matches"], json!([]));
    assert_eq!(output["truncated"], false);
    assert!(output["instruction"].is_null());
    assert!(trajectory.iter().all(|record| record.compaction.is_none()));
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| tool_names(request).contains(&"fiasco"))
    );
    assert!(
        text_content(&requests[0].messages[0])
            .contains("profile: root\nremaining delegation depth: 0")
    );
    assert_eq!(
        serialized(&requests[0].system),
        serialized(&requests[1].system)
    );
    assert_eq!(
        serialized(&requests[0].tools),
        serialized(&requests[1].tools)
    );
}

fn runner(
    workspace: &std::path::Path,
    provider: Arc<dyn ModelProvider>,
    memory: Option<MemoryPaths>,
    options: RunnerOptions,
) -> Arc<AgentRunner> {
    runner_with_store(
        workspace,
        provider,
        memory,
        options,
        RunDirStore::new(workspace),
    )
}

fn runner_with_store(
    workspace: &std::path::Path,
    provider: Arc<dyn ModelProvider>,
    memory: Option<MemoryPaths>,
    options: RunnerOptions,
    store: RunDirStore,
) -> Arc<AgentRunner> {
    let mut tools = ToolRegistry::default();
    tools.register(Arc::new(BashTool)).unwrap();
    tools.register(Arc::new(MarkerTool)).unwrap();
    tools.register(Arc::new(ReadTool::default())).unwrap();
    tools.register(Arc::new(WriteTool::default())).unwrap();
    AgentRunner::new(AgentRunnerConfig {
        provider,
        model: "test-model".to_owned(),
        workspace: workspace.to_owned(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools,
        artifacts: ArtifactStore::default(),
        store,
        hooks: HookPipeline::new(),
        memory,
        extra_events: Arc::new(NoopEventSink),
        options,
    })
}

fn serialized<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn user_prompt(request: &ModelRequest) -> Option<&str> {
    request
        .messages
        .first()?
        .content
        .iter()
        .find_map(|content| match content {
            MessageContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
}

fn requests_for_prompt<'a>(requests: &'a [ModelRequest], expected: &str) -> Vec<&'a ModelRequest> {
    requests
        .iter()
        .filter(|request| user_prompt(request) == Some(expected))
        .collect()
}

fn assert_profile_tools(requests: &[&ModelRequest], expected: &[&str]) {
    let first_system = serialized(&requests[0].system);
    let first_tools = serialized(&requests[0].tools);
    let first_initial_message = serialized(&requests[0].messages[0]);
    for request in requests {
        assert_eq!(tool_names(request), expected);
        assert_eq!(serialized(&request.system), first_system);
        assert_eq!(serialized(&request.tools), first_tools);
        assert_eq!(serialized(&request.messages[0]), first_initial_message);
    }
}

fn tool_names(request: &ModelRequest) -> Vec<&str> {
    request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect()
}

fn text_content(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            MessageContent::RuntimeReminder { text } | MessageContent::Text { text } => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_tool_result(message: &Message, call_id: &str) -> bool {
    message.content.iter().any(|content| {
        matches!(
            content,
            MessageContent::ToolResult {
                call_id: result_call_id,
                ..
            } if result_call_id == call_id
        )
    })
}

fn tool_result_content<'a>(
    trajectory: &'a [TrajectoryMessage],
    expected_call_id: &str,
) -> Option<&'a str> {
    trajectory
        .iter()
        .flat_map(|record| &record.message.content)
        .find_map(|content| match content {
            MessageContent::ToolResult {
                call_id, content, ..
            } if call_id == expected_call_id => Some(content.as_str()),
            _ => None,
        })
}
