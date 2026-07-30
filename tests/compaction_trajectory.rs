use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
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
    storage::RunDirStore,
    tools::{RawToolOutput, ReadTool, Tool, ToolContext, ToolRegistry},
    trajectory::TrajectoryMessage,
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

struct InspectableTrajectoryProvider {
    normal_calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
}

impl InspectableTrajectoryProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            normal_calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl ModelProvider for InspectableTrajectoryProvider {
    fn name(&self) -> &str {
        "inspectable-trajectory"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        let is_summary = request.messages.last().is_some_and(|message| {
            text_content(message).contains("Compact the conversation state before this message")
        });
        self.requests.lock().unwrap().push(request.clone());
        if is_summary {
            return Ok(ModelResponse::new(
                Message::text(Role::Assistant, SUMMARY_TEXT),
                ModelUsage {
                    input_tokens: Some(42),
                    output_tokens: Some(9),
                    cached_input_tokens: Some(3),
                    reasoning_tokens: None,
                },
            ));
        }

        match self.normal_calls.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(tool_call_response("call-old", "old", 100)),
            // Scripted usage crosses the threshold before the next normal call,
            // after both marker pairs are durable.
            1 => Ok(tool_call_response("call-new", "new", 20_000)),
            2 => Ok(command_tool_call_response(
                "call-history-search",
                "history search pattern=result-old".to_owned(),
                80,
            )),
            3 => Ok(command_tool_call_response(
                "call-history-read",
                shell_words::join([
                    "history",
                    "read",
                    &format!("ref={}", history_match_ref(&request)?),
                    "before=1",
                    "after=1",
                ]),
                100,
            )),
            4 => Ok(ModelResponse::new(
                Message::text(Role::Assistant, "finished after history recovery"),
                ModelUsage {
                    input_tokens: Some(120),
                    output_tokens: Some(6),
                    cached_input_tokens: Some(60),
                    reasoning_tokens: None,
                },
            )),
            unexpected => bail!("unexpected inspectable normal model call {unexpected}"),
        }
    }
}

fn tool_call_response(id: &str, label: &str, input_tokens: u64) -> ModelResponse {
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

fn command_tool_call_response(id: &str, command: String, input_tokens: u64) -> ModelResponse {
    ModelResponse::new(
        Message::assistant(vec![MessageContent::ToolCall(ToolCall {
            id: id.to_owned(),
            name: "fiasco".to_owned(),
            arguments: json!({"command": command}).into(),
        })]),
        ModelUsage {
            input_tokens: Some(input_tokens),
            output_tokens: Some(10),
            cached_input_tokens: Some(input_tokens / 2),
            reasoning_tokens: None,
        },
    )
}

fn history_match_ref(request: &ModelRequest) -> Result<String> {
    let search_result = request
        .messages
        .iter()
        .rev()
        .flat_map(|message| &message.content)
        .find_map(|content| match content {
            MessageContent::ToolResult {
                call_id, content, ..
            } if call_id == "call-history-search" => Some(content),
            _ => None,
        });
    let Some(search_result) = search_result else {
        bail!("history_read request was prepared without a history_search result")
    };
    let record: Value = serde_json::from_str(search_result)?;
    if let Some(message_ref) = record["matches"][0]["ref"].as_str() {
        return Ok(message_ref.to_owned());
    }
    bail!("history_search result did not contain a message ref")
}

fn runner(
    workspace: &Path,
    provider: Arc<InspectableTrajectoryProvider>,
) -> (Arc<AgentRunner>, RunDirStore) {
    let store = RunDirStore::new(workspace);
    let mut tools = ToolRegistry::default();
    tools.register(Arc::new(MarkerTool)).unwrap();
    tools.register(Arc::new(ReadTool::default())).unwrap();
    let options = RunnerOptions {
        max_subagent_depth: 0,
        max_output_tokens: Some(4_096),
        compaction: CompactionOptions {
            compact_at_tokens: Some(10_000),
            context_window_tokens: Some(30_000),
            keep_recent_tokens: 1,
            summary_max_output_tokens: 77,
            history_search_max_matches: 7,
        },
        ..RunnerOptions::default()
    };
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider,
        model: "trajectory-model".to_owned(),
        workspace: workspace.to_owned(),
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
async fn retained_trajectory_exercises_search_and_read_after_one_compaction() {
    let retained_workspace = std::env::var_os("FIASCO_TRAJECTORY_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let temporary_workspace = retained_workspace
        .is_none()
        .then(|| TempDir::new().unwrap());
    let workspace = retained_workspace
        .as_deref()
        .unwrap_or_else(|| temporary_workspace.as_ref().unwrap().path());
    if retained_workspace.is_some() {
        tokio::fs::create_dir_all(workspace).await.unwrap();
    }

    let provider = InspectableTrajectoryProvider::new();
    let (runner, store) = runner(workspace, provider.clone());
    let result = runner
        .run(RunRequest::root("exercise retained compaction trajectory"))
        .await
        .unwrap();
    assert_eq!(result.final_output, "finished after history recovery");

    let requests = provider.requests();
    assert_eq!(requests.len(), 6);
    assert!(requests.iter().all(|request| !request.tools.is_empty()));

    let normal_requests = requests
        .iter()
        .filter(|request| {
            !request.messages.last().is_some_and(|message| {
                text_content(message).contains("Compact the conversation state before this message")
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(normal_requests.len(), 5);
    let stable_system = &normal_requests[0].system;
    let stable_tools = serde_json::to_value(&normal_requests[0].tools).unwrap();
    let stable_initial_message = serde_json::to_value(&normal_requests[0].messages[0]).unwrap();
    for request in &normal_requests[1..] {
        assert_eq!(&request.system, stable_system);
        assert_eq!(serde_json::to_value(&request.tools).unwrap(), stable_tools);
        assert_eq!(
            serde_json::to_value(&request.messages[0]).unwrap(),
            stable_initial_message
        );
    }

    assert!(!request_contains(normal_requests[0], "# Compacted state"));
    assert!(!request_contains(normal_requests[1], "# Compacted state"));
    assert!(request_contains(normal_requests[2], SUMMARY_TEXT));
    assert!(request_contains(
        normal_requests[2],
        "not a final answer or a request to compact again"
    ));
    assert!(!request_contains(
        normal_requests[2],
        "Compact the conversation state before this message"
    ));
    assert!(
        !normal_requests[2]
            .messages
            .iter()
            .any(|message| has_tool_result(message, "call-old", "result-old"))
    );
    assert!(
        normal_requests[2]
            .messages
            .iter()
            .any(|message| has_tool_result(message, "call-new", "result-new"))
    );

    let trajectory = store.load_trajectory(&result.run_id).await.unwrap();
    let search_output = tool_result_content(&trajectory, "call-history-search").unwrap();
    assert!(search_output.contains(r#""matches":[{"#));
    assert!(search_output.contains(r#""ref":"m3"#));
    assert!(search_output.contains("result-old"));
    let recovered_ref = history_match_ref(normal_requests[3]).unwrap();
    assert!(has_tool_call(
        &normal_requests[3].messages[5],
        "call-history-search",
        "fiasco"
    ));
    assert!(has_tool_call(
        &normal_requests[4].messages[7],
        "call-history-read",
        "fiasco"
    ));
    let read_output = tool_result_content(&trajectory, "call-history-read").unwrap();
    assert!(read_output.lines().any(|line| {
        serde_json::from_str::<Value>(line).is_ok_and(|record| record["message"]["role"] == "tool")
    }));
    assert!(read_output.contains("result-old"));

    let compacted_state = trajectory
        .iter()
        .find_map(|record| record.compaction_state())
        .unwrap();
    assert_eq!(recovered_ref, compacted_state.covered_through_message_ref);
    let events = load_events(&store, &result.run_id).await;
    assert_eq!(count_events(&events, EventClass::Started), 1);
    assert_eq!(count_events(&events, EventClass::Completed), 1);
    assert_eq!(count_events(&events, EventClass::Failed), 0);

    if retained_workspace.is_some() {
        write_trajectory_capture(&store, &result.run_id, &requests, &trajectory)
            .await
            .unwrap();
        let capture = tokio::fs::canonicalize(store.paths(&result.run_id).directory)
            .await
            .unwrap();
        eprintln!("retained compaction trajectory: {}", capture.display());
    }
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

fn request_contains(request: &ModelRequest, expected: &str) -> bool {
    request
        .messages
        .iter()
        .any(|message| text_content(message).contains(expected))
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

async fn write_trajectory_capture(
    store: &RunDirStore,
    run_id: &str,
    requests: &[ModelRequest],
    trajectory: &[TrajectoryMessage],
) -> Result<()> {
    let paths = store.paths(run_id);
    let request_directory = paths.directory.join("requests");
    let history_directory = paths.directory.join("history");
    tokio::fs::create_dir_all(&request_directory).await?;
    tokio::fs::create_dir_all(&history_directory).await?;

    let request_files = [
        "01-model-before-compaction-old.json",
        "02-model-before-compaction-new.json",
        "03-compaction-state.json",
        "04-model-after-compaction-search.json",
        "05-model-after-history-search.json",
        "06-model-after-history-read.json",
    ];
    for (request, file_name) in requests.iter().zip(request_files) {
        let snapshot = json!({
            "run_id": request.run_id,
            "model": request.model,
            "system": request.system,
            "provider_neutral_messages": request.messages,
            "tools": request.tools,
            "max_output_tokens": request.max_output_tokens,
        });
        tokio::fs::write(
            request_directory.join(file_name),
            serde_json::to_vec_pretty(&snapshot)?,
        )
        .await?;
    }

    let search_output = tool_result_content(trajectory, "call-history-search")
        .ok_or_else(|| anyhow::anyhow!("missing retained history_search result"))?;
    let read_output = tool_result_content(trajectory, "call-history-read")
        .ok_or_else(|| anyhow::anyhow!("missing retained history_read result"))?;
    tokio::fs::write(history_directory.join("history_search.json"), search_output).await?;
    tokio::fs::write(history_directory.join("history_read.jsonl"), read_output).await?;

    let normal_requests = requests
        .iter()
        .filter(|request| {
            !request.messages.last().is_some_and(|message| {
                text_content(message).contains("Compact the conversation state before this message")
            })
        })
        .collect::<Vec<_>>();
    let system = &normal_requests[0].system;
    let tools = serde_json::to_value(&normal_requests[0].tools)?;
    let initial_message = serde_json::to_value(&normal_requests[0].messages[0])?;
    let comparison = json!({
        "normal_request_count": normal_requests.len(),
        "compaction_request_number": 3,
        "first_post_compaction_request_number": 4,
        "evidence": {
            "scope": "Captured internal ModelRequest snapshots under provider_neutral_messages; durable messages.jsonl stores the same provider-neutral content. This is not proof of live provider KV-cache reuse.",
            "usage_and_cache_numbers": "Deterministic values emitted by the scripted test provider, not live provider measurements."
        },
        "same_system": normal_requests.iter().all(|request| &request.system == system),
        "same_tool_schemas": normal_requests.iter().all(|request| serde_json::to_value(&request.tools).ok().as_ref() == Some(&tools)),
        "same_initial_runtime_message": normal_requests.iter().all(|request| serde_json::to_value(&request.messages[0]).ok().as_ref() == Some(&initial_message)),
        "run_files": [
            "run.json",
            "messages.jsonl",
            "events.jsonl",
            "final.md",
        ],
        "request_files": request_files,
        "history_files": [
            "history/history_search.json",
            "history/history_read.jsonl",
        ],
    });
    tokio::fs::write(
        paths.directory.join("capture.json"),
        serde_json::to_vec_pretty(&comparison)?,
    )
    .await?;
    Ok(())
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
