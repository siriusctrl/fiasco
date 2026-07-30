use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use anyhow::{Result, bail};
use async_trait::async_trait;
use fiasco::{
    agent::{
        CompactionOptions,
        runner::{AgentRunner, AgentRunnerConfig, RunRequest, RunnerOptions},
    },
    artifact::{ArtifactPolicy, ArtifactStore},
    events::{NoopEventSink, RuntimeEvent, RuntimeEventKind, SharedEventSink},
    hooks::HookPipeline,
    model::{
        Message, MessageContent, ModelProvider, ModelRequest, ModelResponse, ModelUsage, Role,
        ToolCall, ToolSpec,
    },
    storage::{RunDirStore, RunState},
    tools::{RawToolOutput, ReadTool, Tool, ToolContext, ToolRegistry},
    trajectory::CompactionMessage,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const SUMMARY_TEXT: &str =
    "# Compacted state\n\n## Progress\nThe old marker result was `result-old`.";

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
        let label = arguments
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("missing");
        Ok(RawToolOutput::text(format!("result-{label}")))
    }
}

struct ScriptedCompactionProvider {
    normal_calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
    fail_summary: bool,
    summary_tool_calls_remaining: AtomicUsize,
    fail_post_compaction_once: AtomicBool,
}

impl ScriptedCompactionProvider {
    fn new(fail_summary: bool) -> Arc<Self> {
        Arc::new(Self {
            normal_calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            fail_summary,
            summary_tool_calls_remaining: AtomicUsize::new(0),
            fail_post_compaction_once: AtomicBool::new(false),
        })
    }

    fn with_post_compaction_failure() -> Arc<Self> {
        let provider = Self::new(false);
        provider
            .fail_post_compaction_once
            .store(true, Ordering::SeqCst);
        provider
    }

    fn with_summary_tool_calls(count: usize) -> Arc<Self> {
        Arc::new(Self {
            normal_calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            fail_summary: false,
            summary_tool_calls_remaining: AtomicUsize::new(count),
            fail_post_compaction_once: AtomicBool::new(false),
        })
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl ModelProvider for ScriptedCompactionProvider {
    fn name(&self) -> &str {
        "scripted-compaction"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        let is_summary = request.messages.last().is_some_and(|message| {
            text_content(message).contains("Compact the conversation state before this message")
        });
        self.requests.lock().unwrap().push(request);
        if is_summary {
            if self.fail_summary {
                bail!("intentional summary failure");
            }
            if self
                .summary_tool_calls_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Ok(tool_call_response_with_usage(
                    "compaction-tool",
                    "must-not-run",
                    42,
                ));
            }
            return Ok(ModelResponse::new(
                Message::text(Role::Assistant, SUMMARY_TEXT),
                ModelUsage {
                    input_tokens: Some(42),
                    output_tokens: Some(9),
                    cached_input_tokens: Some(21),
                    reasoning_tokens: Some(4),
                },
            ));
        }

        let index = self.normal_calls.fetch_add(1, Ordering::SeqCst);
        match index {
            0 => Ok(tool_call_response("call-old", "old")),
            1 => Ok(tool_call_response("call-new", "new")),
            2 if self.fail_post_compaction_once.swap(false, Ordering::SeqCst) => {
                bail!("intentional post-compaction failure")
            }
            2 | 3 => Ok(ModelResponse::new(
                Message::text(Role::Assistant, "finished after compaction"),
                ModelUsage {
                    input_tokens: Some(80),
                    output_tokens: Some(5),
                    ..ModelUsage::default()
                },
            )),
            unexpected => bail!("unexpected normal model call {unexpected}"),
        }
    }
}

fn tool_call_response(id: &str, label: &str) -> ModelResponse {
    tool_call_response_with_usage(id, label, 100)
}

fn tool_call_response_with_usage(id: &str, label: &str, input_tokens: u64) -> ModelResponse {
    ModelResponse::new(
        Message::assistant(vec![MessageContent::ToolCall(ToolCall {
            id: id.to_owned(),
            name: "marker".to_owned(),
            arguments: json!({"label": label}).into(),
        })]),
        ModelUsage {
            input_tokens: Some(input_tokens),
            output_tokens: Some(10),
            ..ModelUsage::default()
        },
    )
}

fn runner(
    workspace: &TempDir,
    provider: Arc<ScriptedCompactionProvider>,
    exact_recovery_available: bool,
) -> (Arc<AgentRunner>, RunDirStore) {
    runner_with_compaction(
        workspace,
        provider,
        exact_recovery_available,
        CompactionOptions {
            compact_at_tokens: Some(10),
            context_window_tokens: Some(100_000),
            keep_recent_tokens: 1,
            summary_max_output_tokens: 77,
            history_search_max_matches: 7,
        },
    )
}

fn runner_with_compaction(
    workspace: &TempDir,
    provider: Arc<ScriptedCompactionProvider>,
    exact_recovery_available: bool,
    compaction: CompactionOptions,
) -> (Arc<AgentRunner>, RunDirStore) {
    let store = RunDirStore::new(workspace.path());
    let mut tools = ToolRegistry::default();
    tools.register(Arc::new(MarkerTool)).unwrap();
    if exact_recovery_available {
        tools.register(Arc::new(ReadTool::default())).unwrap();
    }
    let options = RunnerOptions {
        max_output_tokens: Some(64),
        compaction,
        ..RunnerOptions::default()
    };
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider,
        model: "test-model".to_owned(),
        workspace: workspace.path().to_owned(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools,
        artifacts: ArtifactStore::new(ArtifactPolicy::default()),
        store: store.clone(),
        hooks: HookPipeline::new(),
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options,
    });
    (runner, store)
}

#[tokio::test]
async fn estimated_context_window_applies_before_the_first_normal_request() {
    let workspace = TempDir::new().unwrap();
    let provider = ScriptedCompactionProvider::new(false);
    let (runner, store) = runner_with_compaction(
        &workspace,
        provider.clone(),
        true,
        CompactionOptions {
            compact_at_tokens: None,
            context_window_tokens: Some(90),
            keep_recent_tokens: 1,
            summary_max_output_tokens: 77,
            history_search_max_matches: 7,
        },
    );

    let error = runner
        .run(RunRequest::root("stop before overflow"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("context_window_tokens=90"));
    assert!(provider.requests().is_empty());
    let run = only_run_id(&workspace).await;
    assert_eq!(
        store.load_run(&run).await.unwrap().state,
        fiasco::storage::RunState::Open
    );
}

#[tokio::test]
async fn rejected_compaction_preflight_does_not_emit_started() {
    let workspace = TempDir::new().unwrap();
    let provider = ScriptedCompactionProvider::new(false);
    let (runner, store) = runner_with_compaction(
        &workspace,
        provider.clone(),
        true,
        CompactionOptions {
            compact_at_tokens: Some(10),
            context_window_tokens: Some(100_000),
            keep_recent_tokens: 1,
            // This alone exhausts the window, while the normal request keeps
            // its independent 64-token output reservation and can continue.
            summary_max_output_tokens: 100_000,
            history_search_max_matches: 7,
        },
    );

    let result = runner
        .run(RunRequest::root("reject compaction before request"))
        .await
        .unwrap();
    assert_eq!(result.final_output, "finished after compaction");
    assert!(provider.requests().iter().all(|request| {
        !request.messages.last().is_some_and(|message| {
            text_content(message).contains("Compact the conversation state before this message")
        })
    }));

    let events = load_events(&store, &result.run_id).await;
    assert_eq!(count_events(&events, EventClass::Started), 0);
    assert_eq!(count_events(&events, EventClass::Completed), 0);
    assert_eq!(count_events(&events, EventClass::Failed), 1);
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        RuntimeEventKind::CompactionFailed {
            attempt: None,
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
            reasoning_tokens: None,
            ..
        }
    )));
}

#[tokio::test]
async fn resume_after_a_durable_compacted_state_continues_instead_of_finalizing_it() {
    let workspace = TempDir::new().unwrap();
    let provider = ScriptedCompactionProvider::with_post_compaction_failure();
    let (runner, store) = runner(&workspace, provider.clone(), true);

    let error = runner
        .run(RunRequest::root("resume after compaction"))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("post-compaction failure"));
    let run_id = only_run_id(&workspace).await;
    let before_resume = store.load_trajectory(&run_id).await.unwrap();
    assert!(before_resume.last().unwrap().compaction_state().is_some());

    let result = runner.resume(&run_id).await.unwrap();
    assert_eq!(result.final_output, "finished after compaction");
    let trajectory = store.load_trajectory(&run_id).await.unwrap();
    assert_eq!(
        trajectory
            .iter()
            .filter(|record| record.compaction_state().is_some())
            .count(),
        2
    );
    assert_eq!(trajectory.last().unwrap().message.role, Role::Assistant);
    assert!(trajectory.last().unwrap().compaction.is_none());

    let resumed_request = provider.requests().last().unwrap().clone();
    assert!(
        resumed_request
            .messages
            .iter()
            .any(|message| message.visible_text() == SUMMARY_TEXT)
    );
    assert!(resumed_request.messages.iter().any(|message| {
        text_content(message).contains("not a final answer or a request to compact again")
    }));
    assert!(!resumed_request.messages.iter().any(|message| {
        message
            .visible_text()
            .contains("Compact the conversation state before this message")
    }));
}

async fn only_run_id(workspace: &TempDir) -> String {
    tokio::fs::read_dir(workspace.path().join(".fiasco/runs"))
        .await
        .unwrap()
        .next_entry()
        .await
        .unwrap()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn runner_compacts_active_context_but_preserves_raw_trajectory() {
    let workspace = TempDir::new().unwrap();
    let provider = ScriptedCompactionProvider::new(false);
    let (runner, store) = runner(&workspace, provider.clone(), true);

    let result = runner
        .run(RunRequest::root("exercise compaction"))
        .await
        .unwrap();
    assert_eq!(result.final_output, "finished after compaction");
    assert_eq!(
        store.load_run(&result.run_id).await.unwrap().state,
        RunState::Completed
    );

    let all_requests = provider.requests();
    let summary_requests: Vec<_> = all_requests
        .iter()
        .filter(|request| {
            request.messages.last().is_some_and(|message| {
                text_content(message).contains("Compact the conversation state before this message")
            })
        })
        .collect();
    let normal_requests: Vec<_> = all_requests
        .iter()
        .filter(|request| {
            !request.messages.last().is_some_and(|message| {
                text_content(message).contains("Compact the conversation state before this message")
            })
        })
        .collect();
    assert_eq!(summary_requests.len(), 1);
    assert_eq!(normal_requests.len(), 3);

    let summary = summary_requests[0];
    assert_eq!(summary.system, normal_requests[0].system);
    assert_eq!(
        serde_json::to_value(&summary.tools).unwrap(),
        serde_json::to_value(&normal_requests[0].tools).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&summary.messages[0]).unwrap(),
        serde_json::to_value(&normal_requests[0].messages[0]).unwrap()
    );
    assert_eq!(summary.max_output_tokens, Some(77));
    assert_eq!(summary.messages.len(), 4);
    assert!(has_tool_call(&summary.messages[1], "call-old", "marker"));
    assert!(has_tool_result(
        &summary.messages[2],
        "call-old",
        "result-old"
    ));
    assert!(text_content(summary.messages.last().unwrap()).contains("# Compacted state"));
    assert!(
        !summary
            .messages
            .iter()
            .any(|message| has_tool_call(message, "call-new", "marker"))
    );

    for request in &normal_requests {
        let names: Vec<_> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert!(names.contains(&"fiasco"));
        assert!(names.contains(&"marker"));
        assert!(!names.contains(&"history_search"));
        assert!(!names.contains(&"history_read"));
        assert!(!names.contains(&"delegate"));
    }
    let stable_system = &normal_requests[0].system;
    let stable_tools = serde_json::to_value(&normal_requests[0].tools).unwrap();
    for request in &normal_requests[1..] {
        assert_eq!(&request.system, stable_system);
        assert_eq!(serde_json::to_value(&request.tools).unwrap(), stable_tools);
    }
    assert!(!stable_system.contains("history_search"));
    assert!(!stable_system.contains("history_read"));
    assert!(!text_content(&normal_requests[0].messages[0]).contains("history_search"));

    let resumed = normal_requests[2];
    assert_eq!(resumed.messages.len(), 5);
    assert!(text_content(&resumed.messages[0]).contains("exercise compaction"));
    let compacted = text_content(&resumed.messages[1]);
    assert_eq!(resumed.messages[1].role, Role::Assistant);
    assert_eq!(compacted, SUMMARY_TEXT);
    assert_eq!(resumed.messages[2].role, Role::User);
    assert!(text_content(&resumed.messages[2]).contains("not a final answer"));
    assert!(has_tool_call(&resumed.messages[3], "call-new", "marker"));
    assert!(has_tool_result(
        &resumed.messages[4],
        "call-new",
        "result-new"
    ));
    assert!(
        !resumed
            .messages
            .iter()
            .any(|message| has_tool_call(message, "call-old", "marker"))
    );

    let trajectory = store.load_trajectory(&result.run_id).await.unwrap();
    assert_eq!(trajectory.len(), 8);
    assert!(has_tool_call(&trajectory[1].message, "call-old", "marker"));
    assert!(has_tool_result(
        &trajectory[2].message,
        "call-old",
        "result-old"
    ));
    assert!(has_tool_call(&trajectory[3].message, "call-new", "marker"));
    assert!(has_tool_result(
        &trajectory[4].message,
        "call-new",
        "result-new"
    ));

    assert!(matches!(
        trajectory[5].compaction,
        Some(CompactionMessage::Request)
    ));
    assert_eq!(trajectory[5].message_ref, "m6");
    assert_eq!(trajectory[5].message.role, Role::User);
    assert!(text_content(&trajectory[5].message).contains("# Compacted state"));
    let compaction_state = trajectory[6].compaction_state().unwrap();
    assert_eq!(trajectory[6].message_ref, "m7");
    assert_eq!(trajectory[6].message.role, Role::Assistant);
    assert_eq!(trajectory[6].message.visible_text(), SUMMARY_TEXT);
    assert_eq!(
        compaction_state.covered_through_message_ref,
        trajectory[2].message_ref
    );
    assert_eq!(
        compaction_state.first_kept_message_ref,
        trajectory[3].message_ref
    );
    let paths = store.paths(&result.run_id);
    assert!(!paths.directory.join("compactions.jsonl").exists());
    let messages = tokio::fs::read_to_string(&paths.messages).await.unwrap();
    let messages = messages
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(messages[5]["role"], "user");
    assert_eq!(messages[6]["role"], "assistant");
    assert!(messages[5].get("type").is_none());
    assert!(messages[6].get("type").is_none());
    assert_eq!(messages[5]["_fiasco"]["compaction"]["kind"], "request");
    assert_eq!(messages[6]["_fiasco"]["compaction"]["kind"], "state");
    assert!(
        messages[5..7]
            .iter()
            .all(|message| message["_fiasco"].get("checkpoint").is_none())
    );
    assert_eq!(
        messages[6]["_fiasco"]["compaction"]["state"]
            .as_object()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        messages[6]["_fiasco"]["compaction"]["state"]["first_kept_message_ref"],
        trajectory[3].message_ref
    );

    let events = load_events(&store, &result.run_id).await;
    assert_eq!(count_events(&events, EventClass::Started), 1);
    assert_eq!(count_events(&events, EventClass::Completed), 1);
    assert_eq!(count_events(&events, EventClass::Failed), 0);
    let completed = events.iter().find_map(|event| match &event.kind {
        RuntimeEventKind::CompactionCompleted {
            state_message_ref,
            covered_through_message_ref,
            first_kept_message_ref,
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
            attempt,
        } => Some((
            state_message_ref,
            covered_through_message_ref,
            first_kept_message_ref,
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
            attempt,
        )),
        _ => None,
    });
    assert_eq!(
        completed,
        Some((
            &trajectory[6].message_ref,
            &compaction_state.covered_through_message_ref,
            &compaction_state.first_kept_message_ref,
            &Some(42),
            &Some(9),
            &Some(21),
            &Some(4),
            &1,
        ))
    );
}

#[tokio::test]
async fn summary_failure_is_recorded_and_does_not_abort_the_run() {
    let workspace = TempDir::new().unwrap();
    let provider = ScriptedCompactionProvider::new(true);
    let (runner, store) = runner(&workspace, provider.clone(), true);

    let result = runner
        .run(RunRequest::root("survive summary failure"))
        .await
        .unwrap();
    assert_eq!(result.final_output, "finished after compaction");
    assert!(
        store
            .load_trajectory(&result.run_id)
            .await
            .unwrap()
            .iter()
            .all(|record| record.compaction.is_none())
    );

    let requests = provider.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.messages.last().is_some_and(|message| {
                text_content(message).contains("Compact the conversation state before this message")
            }))
            .count(),
        1
    );
    let final_request = requests
        .iter()
        .filter(|request| {
            !request.messages.last().is_some_and(|message| {
                text_content(message).contains("Compact the conversation state before this message")
            })
        })
        .nth(2)
        .unwrap();
    assert!(
        final_request
            .messages
            .iter()
            .any(|message| has_tool_call(message, "call-old", "marker"))
    );

    let events = load_events(&store, &result.run_id).await;
    assert_eq!(count_events(&events, EventClass::Started), 1);
    assert_eq!(count_events(&events, EventClass::Completed), 0);
    assert_eq!(count_events(&events, EventClass::Failed), 1);
}

#[tokio::test]
async fn compaction_tool_call_is_rejected_without_execution() {
    let workspace = TempDir::new().unwrap();
    let provider = ScriptedCompactionProvider::with_summary_tool_calls(2);
    let (runner, store) = runner(&workspace, provider, true);

    let result = runner
        .run(RunRequest::root("reject compaction tool call"))
        .await
        .unwrap();
    assert_eq!(result.final_output, "finished after compaction");
    let trajectory = store.load_trajectory(&result.run_id).await.unwrap();
    assert!(trajectory.iter().all(|record| record.compaction.is_none()));
    assert!(!trajectory.iter().any(|record| {
        has_tool_result(&record.message, "compaction-tool", "result-must-not-run")
    }));

    let events = load_events(&store, &result.run_id).await;
    assert_eq!(count_events(&events, EventClass::Started), 2);
    assert_eq!(count_events(&events, EventClass::Completed), 0);
    assert_eq!(count_events(&events, EventClass::Failed), 2);
    assert_eq!(compaction_attempts(&events, EventClass::Started), [1, 2]);
    assert_eq!(compaction_attempts(&events, EventClass::Failed), [1, 2]);
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        RuntimeEventKind::CompactionFailed { error, .. }
            if error.contains("returned tool calls")
    )));
}

#[tokio::test]
async fn compaction_retries_one_invalid_tool_call_response() {
    let workspace = TempDir::new().unwrap();
    let provider = ScriptedCompactionProvider::with_summary_tool_calls(1);
    let (runner, store) = runner(&workspace, provider.clone(), true);

    let result = runner
        .run(RunRequest::root("retry invalid compaction response"))
        .await
        .unwrap();
    assert_eq!(result.final_output, "finished after compaction");
    let trajectory = store.load_trajectory(&result.run_id).await.unwrap();
    assert!(trajectory.iter().any(|record| record.compaction.is_some()));
    assert!(!trajectory.iter().any(|record| {
        has_tool_result(&record.message, "compaction-tool", "result-must-not-run")
    }));
    assert_eq!(
        provider
            .requests()
            .iter()
            .filter(|request| request.messages.last().is_some_and(|message| {
                text_content(message).contains("Compact the conversation state before this message")
            }))
            .count(),
        2
    );

    let events = load_events(&store, &result.run_id).await;
    assert_eq!(count_events(&events, EventClass::Started), 2);
    assert_eq!(count_events(&events, EventClass::Failed), 1);
    assert_eq!(count_events(&events, EventClass::Completed), 1);
    assert_eq!(compaction_attempts(&events, EventClass::Started), [1, 2]);
    assert_eq!(compaction_attempts(&events, EventClass::Failed), [1]);
    assert_eq!(compaction_attempts(&events, EventClass::Completed), [2]);
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        RuntimeEventKind::CompactionFailed {
            attempt: Some(1),
            input_tokens: Some(42),
            output_tokens: Some(10),
            cached_input_tokens: None,
            reasoning_tokens: None,
            ..
        }
    )));
}

#[tokio::test]
async fn automatic_compaction_requires_an_exact_artifact_reader() {
    let workspace = TempDir::new().unwrap();
    let provider = ScriptedCompactionProvider::new(false);
    let (runner, store) = runner(&workspace, provider.clone(), false);

    let result = runner
        .run(RunRequest::root("keep full context without read or bash"))
        .await
        .unwrap();
    assert_eq!(result.final_output, "finished after compaction");
    assert!(
        store
            .load_trajectory(&result.run_id)
            .await
            .unwrap()
            .iter()
            .all(|record| record.compaction.is_none())
    );

    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| !request.tools.is_empty()));
    let final_request = requests.last().unwrap();
    assert!(final_request.messages.iter().any(|message| has_tool_result(
        message,
        "call-old",
        "result-old"
    )));
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

fn has_tool_call(message: &Message, id: &str, name: &str) -> bool {
    message.content.iter().any(|content| {
        matches!(
            content,
            MessageContent::ToolCall(call) if call.id == id && call.name == name
        )
    })
}

fn has_tool_result(message: &Message, call_id: &str, expected: &str) -> bool {
    message.content.iter().any(|content| {
        matches!(
            content,
            MessageContent::ToolResult {
                call_id: result_call_id,
                content,
                ..
            } if result_call_id == call_id && content.contains(expected)
        )
    })
}

async fn load_events(store: &RunDirStore, run_id: &str) -> Vec<RuntimeEvent> {
    tokio::fs::read_to_string(store.paths(run_id).events)
        .await
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[derive(Clone, Copy)]
enum EventClass {
    Started,
    Completed,
    Failed,
}

fn count_events(events: &[RuntimeEvent], class: EventClass) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                (&event.kind, class),
                (
                    RuntimeEventKind::CompactionStarted { .. },
                    EventClass::Started
                ) | (
                    RuntimeEventKind::CompactionCompleted { .. },
                    EventClass::Completed
                ) | (
                    RuntimeEventKind::CompactionFailed { .. },
                    EventClass::Failed
                )
            )
        })
        .count()
}

fn compaction_attempts(events: &[RuntimeEvent], class: EventClass) -> Vec<usize> {
    events
        .iter()
        .filter_map(|event| match (&event.kind, class) {
            (RuntimeEventKind::CompactionStarted { attempt, .. }, EventClass::Started)
            | (RuntimeEventKind::CompactionCompleted { attempt, .. }, EventClass::Completed) => {
                Some(*attempt)
            }
            (
                RuntimeEventKind::CompactionFailed {
                    attempt: Some(attempt),
                    ..
                },
                EventClass::Failed,
            ) => Some(*attempt),
            _ => None,
        })
        .collect()
}
