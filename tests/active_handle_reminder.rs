use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use fiasco::{
    agent::{
        CompactionOptions,
        runner::{AgentRunner, AgentRunnerConfig, RunRequest, RunnerOptions},
    },
    artifact::ArtifactStore,
    events::{NoopEventSink, SharedEventSink},
    hooks::HookPipeline,
    model::{
        Message, MessageContent, ModelProvider, ModelRequest, ModelResponse, ModelUsage, Role,
        ToolCall,
    },
    storage::RunDirStore,
    tools::{ReadTool, ToolRegistry},
};
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Notify;

const ACTIVE_MARKER: &str = "<active-runtime-handles>";
const COMPACTION_INSTRUCTION: &str = "Compact the conversation state before this message";

struct CompactedTaskProvider {
    root_run_id: Mutex<Option<String>>,
    child_handle: Mutex<Option<String>>,
    root_calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
    release_child: Notify,
}

impl CompactedTaskProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            root_run_id: Mutex::new(None),
            child_handle: Mutex::new(None),
            root_calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            release_child: Notify::new(),
        })
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl ModelProvider for CompactedTaskProvider {
    fn name(&self) -> &str {
        "compacted-active-task"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        let is_compaction = request
            .messages
            .last()
            .is_some_and(|message| message.visible_text().contains(COMPACTION_INSTRUCTION));
        self.requests.lock().unwrap().push(request.clone());
        if is_compaction {
            return Ok(text_response(
                "# Compacted state\n\nA review task is already delegated.",
                20,
            ));
        }

        let root_run_id = self
            .root_run_id
            .lock()
            .unwrap()
            .get_or_insert_with(|| request.run_id.clone())
            .clone();
        if request.run_id != root_run_id {
            self.release_child.notified().await;
            return Ok(text_response("child review completed", 10));
        }

        match self.root_calls.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(command_response(
                "delegate-review",
                "agent start name='review existing work' prompt=-".to_owned(),
                Some("Inspect only, then report. Do not edit or delegate.".to_owned()),
                1_000,
            )),
            1 => {
                let handle = delegate_handle(&request)?;
                *self.child_handle.lock().unwrap() = Some(handle.clone());
                Ok(command_response(
                    "status-review",
                    shell_words::join(["agent", "list", &format!("handles={}", json!([handle]))]),
                    None,
                    1_000,
                ))
            }
            2 => {
                self.release_child.notify_one();
                let handle = self
                    .child_handle
                    .lock()
                    .unwrap()
                    .clone()
                    .context("child handle was not recorded before compaction")?;
                Ok(command_response(
                    "wait-review",
                    shell_words::join(["agent", "wait", &format!("handles={}", json!([handle]))]),
                    None,
                    10,
                ))
            }
            3 => Ok(text_response("parent completed", 10)),
            unexpected => bail!("unexpected root model call {unexpected}"),
        }
    }
}

#[tokio::test]
async fn active_handle_reminder_survives_compaction_then_disappears_after_delivery() {
    let workspace = TempDir::new().unwrap();
    let provider = CompactedTaskProvider::new();
    let store = RunDirStore::new(workspace.path());
    let mut tools = ToolRegistry::default();
    tools.register(Arc::new(ReadTool::default())).unwrap();
    let runner = runner(
        workspace.path(),
        provider.clone(),
        store.clone(),
        tools,
        RunnerOptions {
            max_parallel_model_calls: 2,
            handle_wait_timeout_seconds: 2,
            max_output_tokens: Some(1_024),
            compaction: CompactionOptions {
                compact_at_tokens: Some(100),
                context_window_tokens: Some(100_000),
                keep_recent_tokens: 1,
                summary_max_output_tokens: 512,
                history_search_max_matches: 10,
            },
            ..RunnerOptions::default()
        },
    );

    let result = runner
        .run(RunRequest::root("delegate one review and finish"))
        .await
        .unwrap();
    assert_eq!(result.final_output, "parent completed");

    let requests = provider.requests();
    let root_normal = requests
        .iter()
        .filter(|request| {
            request.run_id == result.run_id
                && !request
                    .messages
                    .last()
                    .is_some_and(|message| message.visible_text().contains(COMPACTION_INSTRUCTION))
        })
        .collect::<Vec<_>>();
    assert_eq!(root_normal.len(), 4);
    assert!(!request_contains(root_normal[0], ACTIVE_MARKER));
    assert!(request_contains(root_normal[1], ACTIVE_MARKER));
    assert!(request_contains(
        root_normal[2],
        "Do not start duplicate work; use an available handle control"
    ));
    assert!(!request_contains(
        root_normal[2],
        "Do not call `delegate` again"
    ));
    assert!(request_contains(
        root_normal[2],
        "kind=\"agent\" name=\"review existing work\" state=\"running\""
    ));
    assert!(request_contains(root_normal[2], "# Compacted state"));
    assert_eq!(runtime_reminder_count(root_normal[2]), 2);
    assert!(!request_contains(root_normal[3], ACTIVE_MARKER));

    let durable = store.load_messages(&result.run_id).await.unwrap();
    assert!(
        durable
            .iter()
            .all(|message| !message_contains(message, ACTIVE_MARKER))
    );
    assert!(durable.iter().any(|message| {
        message.content.iter().any(|content| {
            matches!(
                content,
                MessageContent::RuntimeHandle {
                    kind,
                    status,
                    ..
                } if kind == "agent" && status == "completed"
            )
        })
    }));
}

fn delegate_handle(request: &ModelRequest) -> Result<String> {
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|content| match content {
            MessageContent::ToolResult {
                call_id, content, ..
            } if call_id == "delegate-review" => runtime_handle_id(content),
            _ => None,
        })
        .context("delegate result omitted runtime handle")
}

fn runtime_handle_id(content: &str) -> Option<String> {
    content
        .split_once("handle=\"")?
        .1
        .split_once('"')
        .map(|(handle, _)| handle.to_owned())
}

fn runner(
    workspace: &std::path::Path,
    provider: Arc<dyn ModelProvider>,
    store: RunDirStore,
    tools: ToolRegistry,
    options: RunnerOptions,
) -> Arc<AgentRunner> {
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
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options,
    })
}

fn command_response(
    id: &str,
    command: String,
    stdin: Option<String>,
    input_tokens: u64,
) -> ModelResponse {
    let mut arguments = json!({"command": command});
    if let Some(stdin) = stdin {
        arguments["stdin"] = stdin.into();
    }
    ModelResponse::new(
        Message::assistant(vec![MessageContent::ToolCall(ToolCall {
            id: id.to_owned(),
            name: "fiasco".to_owned(),
            arguments: arguments.into(),
        })]),
        ModelUsage {
            input_tokens: Some(input_tokens),
            output_tokens: Some(10),
            ..ModelUsage::default()
        },
    )
}

fn text_response(text: &str, input_tokens: u64) -> ModelResponse {
    ModelResponse::new(
        Message::text(Role::Assistant, text),
        ModelUsage {
            input_tokens: Some(input_tokens),
            output_tokens: Some(10),
            ..ModelUsage::default()
        },
    )
}

fn request_contains(request: &ModelRequest, expected: &str) -> bool {
    request
        .messages
        .iter()
        .any(|message| message_contains(message, expected))
}

fn message_contains(message: &Message, expected: &str) -> bool {
    message.content.iter().any(|content| match content {
        MessageContent::RuntimeReminder { text } | MessageContent::Text { text } => {
            text.contains(expected)
        }
        _ => false,
    })
}

fn runtime_reminder_count(request: &ModelRequest) -> usize {
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|content| matches!(content, MessageContent::RuntimeReminder { .. }))
        .count()
}
