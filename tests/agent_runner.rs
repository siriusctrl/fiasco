use std::{
    collections::BTreeSet,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use fiasco::{
    agent::runner::{AgentRunner, AgentRunnerConfig, RunRequest, RunnerOptions},
    artifact::{ArtifactPolicy, ArtifactStore, ResultMetadata},
    events::{NoopEventSink, SharedEventSink},
    hooks::{CommandHook, HookEvent, HookPipeline},
    model::{
        Message, MessageContent, ModelModality, ModelProvider, ModelRequest, ModelResponse,
        ModelUsage, Role, ToolCall, echo::EchoProvider,
    },
    storage::{RunDirStore, RunRecord, RunState},
    tools::{RawToolOutput, Tool, ToolContext, ToolRegistry},
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn text_response(text: impl Into<String>, usage: ModelUsage) -> ModelResponse {
    ModelResponse::new(Message::text(Role::Assistant, text), usage)
}

fn tool_response(calls: Vec<ToolCall>, usage: ModelUsage) -> ModelResponse {
    ModelResponse::new(
        Message::assistant(calls.into_iter().map(MessageContent::ToolCall).collect()),
        usage,
    )
}

fn fiasco_call<I, S>(id: impl Into<String>, words: I, stdin: Option<String>) -> ToolCall
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut arguments = json!({ "command": shell_words::join(words) });
    if let Some(stdin) = stdin {
        arguments["stdin"] = stdin.into();
    }
    ToolCall {
        id: id.into(),
        name: "fiasco".to_owned(),
        arguments: arguments.into(),
    }
}

fn first_user_text(request: &ModelRequest) -> &str {
    request
        .messages
        .iter()
        .find(|message| message.role == Role::User)
        .and_then(|message| {
            message.content.iter().find_map(|content| match content {
                MessageContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
        })
        .unwrap_or_default()
}

fn runtime_handle_id(content: &str) -> Option<String> {
    content
        .split_once("handle=\"")?
        .1
        .split_once('"')
        .map(|(handle, _)| handle.to_owned())
}

struct ResumeProvider {
    calls: Arc<AtomicUsize>,
    require_restart_reminder: bool,
}

#[async_trait]
impl ModelProvider for ResumeProvider {
    fn name(&self) -> &str {
        "resume-scripted"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.require_restart_reminder
            && !request.messages.iter().any(|message| {
                message.content.iter().any(|content| match content {
                    MessageContent::RuntimeReminder { text } => {
                        text.contains("activities and asynchronous tool jobs")
                            && text.contains("side effects may already have occurred")
                    }
                    _ => false,
                })
            })
        {
            bail!("resume request omitted the restart reminder");
        }
        Ok(text_response("resumed", ModelUsage::default()))
    }
}

struct CountingTool(Arc<AtomicUsize>);

#[async_trait]
impl Tool for CountingTool {
    fn spec(&self) -> fiasco::model::ToolSpec {
        fiasco::model::ToolSpec {
            name: "side_effect".to_owned(),
            description: "Count executions".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    async fn execute(&self, _context: ToolContext, _arguments: Value) -> Result<RawToolOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(RawToolOutput::text("executed"))
    }
}

fn resume_runner(
    workspace: &Path,
    store: &RunDirStore,
    provider: ResumeProvider,
    tools: ToolRegistry,
) -> Arc<AgentRunner> {
    AgentRunner::new(AgentRunnerConfig {
        provider: Arc::new(provider),
        model: "scripted".to_owned(),
        workspace: workspace.to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools,
        artifacts: ArtifactStore::default(),
        store: store.clone(),
        hooks: HookPipeline::new(),
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions::default(),
    })
}

async fn create_interrupted_run(store: &RunDirStore, workspace: &Path, run_id: &str) {
    store
        .create_run(
            &RunRecord::new(
                run_id,
                "root",
                "resume me",
                "resume-scripted",
                "scripted",
                workspace.to_path_buf(),
                None,
            )
            .with_provider_resume_fingerprint(
                ResumeProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    require_restart_reminder: false,
                }
                .resume_fingerprint(),
            ),
        )
        .await
        .unwrap();
    store
        .append_message(run_id, &Message::text(Role::User, "resume me"))
        .await
        .unwrap();
}

#[tokio::test]
async fn resume_discards_an_incomplete_tool_turn_and_warns_without_reexecution() {
    let workspace = TempDir::new().unwrap();
    let setup_store = RunDirStore::new(workspace.path());
    create_interrupted_run(&setup_store, workspace.path(), "resume-tool").await;
    setup_store
        .append_messages(
            "resume-tool",
            &[
                Message::assistant(vec![MessageContent::ToolCall(ToolCall {
                    id: "side-effect-call".to_owned(),
                    name: "side_effect".to_owned(),
                    arguments: json!({}).into(),
                })]),
                Message::new(
                    Role::Tool,
                    vec![MessageContent::ToolResult {
                        call_id: "side-effect-call".to_owned(),
                        content: "uncommitted result".to_owned(),
                        is_error: false,
                        metadata: ResultMetadata::empty(),
                    }],
                ),
            ],
        )
        .await
        .unwrap();
    let messages_path = setup_store.paths("resume-tool").messages;
    let bytes = tokio::fs::read(&messages_path).await.unwrap();
    let incomplete_end = bytes
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| (*byte == b'\n').then_some(index + 1))
        .nth(1)
        .unwrap();
    tokio::fs::write(&messages_path, &bytes[..incomplete_end])
        .await
        .unwrap();
    let store = RunDirStore::new(workspace.path());
    assert_eq!(store.load_messages("resume-tool").await.unwrap().len(), 2);
    let model_calls = Arc::new(AtomicUsize::new(0));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools
        .register(Arc::new(CountingTool(tool_calls.clone())))
        .unwrap();
    let runner = resume_runner(
        workspace.path(),
        &store,
        ResumeProvider {
            calls: model_calls.clone(),
            require_restart_reminder: true,
        },
        tools,
    );

    let result = runner.resume("resume-tool").await.unwrap();
    assert_eq!(result.final_output, "resumed");
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0);
    let messages = store.load_messages("resume-tool").await.unwrap();
    assert_eq!(messages.len(), 3);
    assert!(matches!(
        messages[2].content.as_slice(),
        [MessageContent::Text { text }] if text == "resumed"
    ));
}

#[tokio::test]
async fn resume_rejects_changed_model_modalities_before_calling_the_provider() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = ResumeProvider {
        calls: calls.clone(),
        require_restart_reminder: false,
    };
    store
        .create_run(
            &RunRecord::new(
                "resume-modalities",
                "root",
                "resume me",
                provider.name(),
                "scripted",
                workspace.path().to_path_buf(),
                None,
            )
            .with_model_modalities(BTreeSet::from([ModelModality::Text, ModelModality::Image]))
            .with_provider_resume_fingerprint(provider.resume_fingerprint()),
        )
        .await
        .unwrap();
    let runner = resume_runner(
        workspace.path(),
        &store,
        ResumeProvider {
            calls: calls.clone(),
            require_restart_reminder: false,
        },
        ToolRegistry::default(),
    );
    let error = runner.resume("resume-modalities").await.unwrap_err();

    assert!(error.to_string().contains("model modalities"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn resume_always_informs_the_model_even_after_a_durable_assistant_message() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    create_interrupted_run(&store, workspace.path(), "resume-final").await;
    store
        .append_message(
            "resume-final",
            &Message::text(Role::Assistant, "already finished"),
        )
        .await
        .unwrap();
    let model_calls = Arc::new(AtomicUsize::new(0));
    let runner = resume_runner(
        workspace.path(),
        &store,
        ResumeProvider {
            calls: model_calls.clone(),
            require_restart_reminder: false,
        },
        ToolRegistry::default(),
    );

    let result = runner.resume("resume-final").await.unwrap();
    assert_eq!(result.final_output, "resumed");
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    let messages = store.load_messages("resume-final").await.unwrap();
    assert_eq!(messages.len(), 4);
    assert!(messages.iter().any(|message| message.content.iter().any(
        |content| matches!(content, MessageContent::RuntimeReminder { text } if text.contains("activities and asynchronous tool jobs"))
    )));
    assert_eq!(
        store.load_run("resume-final").await.unwrap().state,
        RunState::Completed
    );
    let events = tokio::fs::read_to_string(store.paths("resume-final").events)
        .await
        .unwrap();
    assert!(events.contains("\"type\":\"run_resumed\""));
}

#[tokio::test]
async fn public_resume_rejects_a_child_run_in_favor_of_parent_recovery() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    store
        .create_run(
            &RunRecord::new(
                "child",
                "child",
                "child work",
                "resume-scripted",
                "scripted",
                workspace.path().to_path_buf(),
                Some("parent".to_owned()),
            )
            .with_execution_context("general_task", 0)
            .with_provider_resume_fingerprint(
                ResumeProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    require_restart_reminder: false,
                }
                .resume_fingerprint(),
            ),
        )
        .await
        .unwrap();
    let runner = resume_runner(
        workspace.path(),
        &store,
        ResumeProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            require_restart_reminder: false,
        },
        ToolRegistry::default(),
    );

    let error = runner.resume("child").await.unwrap_err();
    assert!(format!("{error:#}").contains("resume its parent `parent` instead"));
}

struct RuntimeRestartProvider {
    root_run_id: String,
    child_run_id: String,
    root_calls: AtomicUsize,
    child_calls: AtomicUsize,
    release_child: tokio::sync::Notify,
}

#[async_trait]
impl ModelProvider for RuntimeRestartProvider {
    fn name(&self) -> &str {
        "runtime-restart"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        if request.run_id == self.child_run_id {
            self.child_calls.fetch_add(1, Ordering::SeqCst);
            self.release_child.notified().await;
            let visible = request
                .messages
                .iter()
                .map(Message::visible_text)
                .collect::<Vec<_>>();
            if !visible.iter().any(|text| text == "old context") {
                bail!("reused child omitted its complete prior context");
            }
            if !visible.iter().any(|text| text == "fresh request") {
                bail!("reused child omitted the explicit new message");
            }
            if visible.iter().any(|text| text.contains("STALE INPUT")) {
                bail!("reused child replayed stale mailbox input");
            }
            if !request.messages.iter().any(|message| {
                message.content.iter().any(|content| {
                    matches!(
                        content,
                        MessageContent::RuntimeReminder { text }
                            if text.contains("previous fiasco process stopped")
                                && text.contains("mailbox input were discarded")
                    )
                })
            }) {
                bail!("reused child omitted its crash reminder");
            }
            return Ok(text_response("continued child", ModelUsage::default()));
        }

        if request.run_id != self.root_run_id {
            bail!("unexpected run {}", request.run_id);
        }
        let tool_results = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| match content {
                MessageContent::ToolResult {
                    call_id, content, ..
                } => Some((call_id.as_str(), content.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();
        match self.root_calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                if self.child_calls.load(Ordering::SeqCst) != 0 {
                    bail!("root restart launched the child before explicit send_message");
                }
                let restart_reminder = request
                    .messages
                    .iter()
                    .flat_map(|message| &message.content)
                    .find_map(|content| match content {
                        MessageContent::RuntimeReminder { text }
                            if text.contains("activities and asynchronous tool jobs") =>
                        {
                            Some(text)
                        }
                        _ => None,
                    })
                    .context("root restart omitted its crash reminder")?;
                for command in [
                    "`fiasco`",
                    "`agent list`",
                    "`agent inspect`",
                    "`agent send`",
                ] {
                    if !restart_reminder.contains(command) {
                        bail!("root restart reminder omitted command {command}");
                    }
                }
                for retired_name in ["list_handles", "send_message"] {
                    if restart_reminder.contains(retired_name) {
                        bail!("root restart reminder retained old tool name {retired_name}");
                    }
                }
                Ok(tool_response(
                    vec![fiasco_call("list-after-restart", ["agent", "list"], None)],
                    ModelUsage::default(),
                ))
            }
            1 => {
                if self.child_calls.load(Ordering::SeqCst) != 0 {
                    bail!("list_handles launched the old child");
                }
                let listed = tool_results
                    .iter()
                    .find(|(call_id, _)| *call_id == "list-after-restart")
                    .context("list_handles result missing")?
                    .1;
                let listed: Value = serde_json::from_str(listed)?;
                let handles = listed["handles"]
                    .as_array()
                    .context("list_handles response omitted handles")?;
                if handles.len() != 1
                    || handles[0]["handle"] != self.child_run_id
                    || handles[0]["name"] != "old reviewer"
                    || handles[0]["status"] != "idle"
                {
                    bail!("restart listed the wrong handles: {listed}");
                }
                if handles.iter().any(|handle| handle["handle"] == "j_lost") {
                    bail!("process-local tool handle survived restart");
                }
                Ok(tool_response(
                    vec![fiasco_call(
                        "send-old-child",
                        [
                            "agent",
                            "send",
                            &format!("handle={}", self.child_run_id),
                            "mode=followup",
                            "message=fresh request",
                        ],
                        None,
                    )],
                    ModelUsage::default(),
                ))
            }
            2 => {
                let sent = tool_results
                    .iter()
                    .find(|(call_id, _)| *call_id == "send-old-child")
                    .context("send_message result missing")?
                    .1;
                let sent: Value = serde_json::from_str(sent)?;
                if sent["accepted_as"] != "started" {
                    bail!("send_message did not lazily start the old child: {sent}");
                }
                self.release_child.notify_one();
                Ok(tool_response(
                    vec![fiasco_call(
                        "wait-any-after-restart",
                        ["agent", "wait", "handles=[]"],
                        None,
                    )],
                    ModelUsage::default(),
                ))
            }
            3 => {
                let waited = tool_results
                    .iter()
                    .find(|(call_id, _)| *call_id == "wait-any-after-restart")
                    .context("wait-any result missing")?
                    .1;
                let waited: Value = serde_json::from_str(waited)?;
                let waited_handles = waited["handles"]
                    .as_array()
                    .context("wait response omitted handles")?;
                if !waited_handles.iter().any(|handle| {
                    handle["handle"] == self.child_run_id && handle["status"] == "idle"
                }) {
                    bail!("wait-any did not observe the completed child: {waited}");
                }
                if !request.messages.iter().any(|message| {
                    message.content.iter().any(|content| {
                        matches!(
                            content,
                            MessageContent::RuntimeHandle {
                                handle,
                                status,
                                content,
                                ..
                            } if handle == &self.child_run_id
                                && status == "completed"
                                && content == "continued child"
                        )
                    })
                }) {
                    bail!("root did not receive the reused child's result");
                }
                Ok(text_response(
                    "restart flow complete",
                    ModelUsage::default(),
                ))
            }
            unexpected => bail!("unexpected root model call {unexpected}"),
        }
    }
}

#[tokio::test]
async fn restart_recovers_agent_threads_but_not_activity_or_tool_jobs() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    let provider = Arc::new(RuntimeRestartProvider {
        root_run_id: "restart-root".to_owned(),
        child_run_id: "old-child".to_owned(),
        root_calls: AtomicUsize::new(0),
        child_calls: AtomicUsize::new(0),
        release_child: tokio::sync::Notify::new(),
    });
    let fingerprint = provider.resume_fingerprint();
    store
        .create_run(
            &RunRecord::new(
                "restart-root",
                "root",
                "continue after crash",
                provider.name(),
                "scripted",
                workspace.path().to_path_buf(),
                None,
            )
            .with_execution_context("root", 1)
            .with_provider_resume_fingerprint(fingerprint.clone()),
        )
        .await
        .unwrap();
    store
        .append_message(
            "restart-root",
            &Message::text(Role::User, "continue after crash"),
        )
        .await
        .unwrap();
    store
        .append_messages(
            "restart-root",
            &[
                Message::assistant(vec![MessageContent::ToolCall(ToolCall {
                    id: "old-tool-call".to_owned(),
                    name: "slow_output".to_owned(),
                    arguments: json!({}).into(),
                })]),
                Message::new(
                    Role::Tool,
                    vec![MessageContent::ToolResult {
                        call_id: "old-tool-call".to_owned(),
                        content: "<runtime_handle handle=\"j_lost\" kind=\"tool\" name=\"slow_output\">The runtime handle is active.</runtime_handle>".to_owned(),
                        is_error: false,
                        metadata: ResultMetadata::empty(),
                    }],
                ),
            ],
        )
        .await
        .unwrap();
    store
        .create_run(
            &RunRecord::new(
                "old-child",
                "old reviewer",
                "old objective",
                provider.name(),
                "scripted",
                workspace.path().to_path_buf(),
                Some("restart-root".to_owned()),
            )
            .with_execution_context("general_task", 0)
            .with_provider_resume_fingerprint(fingerprint),
        )
        .await
        .unwrap();
    store
        .update_state("old-child", RunState::Open)
        .await
        .unwrap();
    store
        .append_message("old-child", &Message::text(Role::User, "old objective"))
        .await
        .unwrap();
    store
        .append_message("old-child", &Message::text(Role::Assistant, "old context"))
        .await
        .unwrap();
    let unrelated = store.paths("unrelated-old");
    tokio::fs::create_dir_all(&unrelated.directory)
        .await
        .unwrap();
    tokio::fs::write(
        unrelated.metadata,
        r#"{"id":"unrelated-old","parent_run_id":"different-parent"}"#,
    )
    .await
    .unwrap();

    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: provider.clone(),
        model: "scripted".to_owned(),
        workspace: workspace.path().to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools: ToolRegistry::default(),
        artifacts: ArtifactStore::default(),
        store: store.clone(),
        hooks: HookPipeline::new(),
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions {
            max_subagent_depth: 1,
            max_parallel_model_calls: 2,
            handle_wait_timeout_seconds: 2,
            ..RunnerOptions::default()
        },
    });

    let result = runner.resume("restart-root").await.unwrap();
    assert_eq!(result.final_output, "restart flow complete");
    assert_eq!(provider.child_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.load_run("old-child").await.unwrap().state,
        RunState::Open
    );
}

#[tokio::test]
async fn run_end_hook_failure_does_not_downgrade_a_durable_completion() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    let mut hooks = HookPipeline::new();
    hooks.register(CommandHook::new(
        "failing-run-end",
        HookEvent::RunEnd,
        "sh",
        vec!["-c".into(), "exit 7".into()],
    ));
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: Arc::new(EchoProvider),
        model: "echo".to_owned(),
        workspace: workspace.path().to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools: ToolRegistry::default(),
        artifacts: ArtifactStore::default(),
        store: store.clone(),
        hooks,
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions::default(),
    });

    let result = runner.run(RunRequest::root("finish once")).await.unwrap();
    assert_eq!(result.final_output, "received: finish once");
    assert_eq!(
        store.load_run(&result.run_id).await.unwrap().state,
        RunState::Completed
    );
    let events = tokio::fs::read_to_string(store.paths(&result.run_id).events)
        .await
        .unwrap();
    assert!(events.contains("\"type\":\"run_completed\""));
    assert!(!events.contains("\"type\":\"run_failed\""));
}

struct FileProducingProvider;

#[async_trait]
impl ModelProvider for FileProducingProvider {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        let completed = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| match content {
                MessageContent::ToolResult { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        if completed.contains("small-call") {
            Ok(ModelResponse::new(
                Message {
                    role: Role::Assistant,
                    reasoning_content: Some("finish reasoning".to_owned()),
                    content: vec![MessageContent::Text {
                        text: "finished".to_owned(),
                    }],
                },
                ModelUsage {
                    input_tokens: Some(12),
                    output_tokens: Some(2),
                    cached_input_tokens: Some(8),
                    reasoning_tokens: Some(3),
                },
            ))
        } else if completed.contains("large-call") {
            Ok(tool_response(
                vec![ToolCall {
                    id: "small-call".to_owned(),
                    name: "small_output".to_owned(),
                    arguments: json!({}).into(),
                }],
                ModelUsage::default(),
            ))
        } else {
            let call = ToolCall {
                id: "large-call".to_owned(),
                name: "large_output".to_owned(),
                arguments: json!({}).into(),
            };
            Ok(ModelResponse::new(
                Message {
                    role: Role::Assistant,
                    reasoning_content: Some("tool reasoning".to_owned()),
                    content: vec![MessageContent::ToolCall(call)],
                },
                ModelUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(1),
                    cached_input_tokens: Some(6),
                    reasoning_tokens: Some(2),
                },
            ))
        }
    }
}

struct LargeOutputTool;

#[async_trait]
impl Tool for LargeOutputTool {
    fn spec(&self) -> fiasco::model::ToolSpec {
        fiasco::model::ToolSpec {
            name: "large_output".to_owned(),
            description: "Produce a large deterministic result".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    async fn execute(&self, _context: ToolContext, _arguments: Value) -> Result<RawToolOutput> {
        Ok(RawToolOutput::text("x".repeat(512)))
    }
}

struct SmallOutputTool;

#[async_trait]
impl Tool for SmallOutputTool {
    fn spec(&self) -> fiasco::model::ToolSpec {
        fiasco::model::ToolSpec {
            name: "small_output".to_owned(),
            description: "Produce a small deterministic result".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    async fn execute(&self, _context: ToolContext, _arguments: Value) -> Result<RawToolOutput> {
        Ok(RawToolOutput::text("small"))
    }
}

#[tokio::test]
async fn runner_spills_a_large_result_without_affecting_the_next_small_result() {
    let workspace = TempDir::new().unwrap();
    let mut tools = ToolRegistry::default();
    tools.register(Arc::new(LargeOutputTool)).unwrap();
    tools.register(Arc::new(SmallOutputTool)).unwrap();
    let store = RunDirStore::new(workspace.path());
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: Arc::new(FileProducingProvider),
        model: "scripted".to_owned(),
        workspace: workspace.path().to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools,
        artifacts: ArtifactStore::new(ArtifactPolicy {
            inline_limit_bytes: 64,
            preview_head_bytes: 16,
            preview_tail_bytes: 16,
        }),
        store: store.clone(),
        hooks: HookPipeline::new(),
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions::default(),
    });

    let result = runner
        .run(RunRequest::root("test artifacts"))
        .await
        .unwrap();
    assert_eq!(result.final_output, "finished");
    assert_eq!(
        store.load_run(&result.run_id).await.unwrap().state,
        RunState::Completed
    );

    let messages = store.load_messages(&result.run_id).await.unwrap();
    assert_eq!(messages.len(), 6);
    let stored_lines = tokio::fs::read_to_string(store.paths(&result.run_id).messages)
        .await
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        stored_lines[1..5]
            .iter()
            .all(|line| line.get("_fiasco").is_none())
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.reasoning_content.is_some())
            .count(),
        2
    );
    let tool_results = messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            MessageContent::ToolResult {
                call_id,
                content,
                metadata,
                ..
            } => Some((call_id.as_str(), content.as_str(), metadata)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_results.len(), 2);
    assert_eq!(tool_results[0].0, "large-call");
    assert!(tool_results[0].1.contains("[Tool output]"));
    assert!(tool_results[0].1.contains("artifact: .fiasco/runs/"));
    assert!(tool_results[0].1.contains("truncated: true"));
    assert!(tool_results[0].2.artifact.is_some());
    assert_eq!(tool_results[1].0, "small-call");
    assert_eq!(tool_results[1].1, "small");
    assert!(tool_results[1].2.artifact.is_none());
    let events = tokio::fs::read_to_string(store.paths(&result.run_id).events)
        .await
        .unwrap();
    assert!(events.contains("\"cached_input_tokens\":8"));
    assert!(events.contains("\"reasoning_tokens\":3"));
    assert!(
        std::fs::read_dir(store.paths(&result.run_id).artifacts)
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("large-call-"))
    );
}

struct DelegatingProvider;

#[async_trait]
impl ModelProvider for DelegatingProvider {
    fn name(&self) -> &str {
        "delegating"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        let first_user = first_user_text(&request);
        let has_result = request
            .messages
            .iter()
            .any(|message| message.role == Role::Tool);
        if first_user == "child two" {
            bail!("scripted child failure");
        }
        if first_user == "delegate work" && !has_result {
            return Ok(tool_response(
                vec![
                    fiasco_call(
                        "delegate-one",
                        ["agent", "start", "name=child_one", "prompt=child one"],
                        None,
                    ),
                    fiasco_call(
                        "delegate-two",
                        ["agent", "start", "name=child_two", "prompt=child two"],
                        None,
                    ),
                ],
                ModelUsage::default(),
            ));
        }
        Ok(text_response(
            format!("done: {first_user}"),
            ModelUsage::default(),
        ))
    }
}

#[tokio::test]
async fn subagents_reuse_the_runner_and_report_failed_children() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: Arc::new(DelegatingProvider),
        model: "scripted".to_owned(),
        workspace: workspace.path().to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools: ToolRegistry::default(),
        artifacts: ArtifactStore::default(),
        store: store.clone(),
        hooks: HookPipeline::new(),
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions::default(),
    });

    let parent = runner.run(RunRequest::root("delegate work")).await.unwrap();
    assert_eq!(parent.final_output, "done: delegate work");

    let run_root = workspace.path().join(".fiasco/runs");
    let mut children = Vec::new();
    for entry in std::fs::read_dir(&run_root).unwrap() {
        let id = entry.unwrap().file_name().to_string_lossy().into_owned();
        if id != parent.run_id {
            children.push(store.load_run(&id).await.unwrap());
        }
    }
    assert_eq!(children.len(), 2);
    assert!(
        children
            .iter()
            .all(|child| child.parent_run_id.as_deref() == Some(&parent.run_id))
    );
    assert!(children.iter().all(|child| child.state == RunState::Open));
    let events = tokio::fs::read_to_string(store.paths(&parent.run_id).events)
        .await
        .unwrap();
    assert!(events.contains("\"type\":\"agent_activity_failed\""));
    let messages = store.load_messages(&parent.run_id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .flat_map(|message| &message.content)
            .filter(|content| matches!(content, MessageContent::RuntimeHandle { .. }))
            .count(),
        2
    );
    let terminal_messages = messages
        .iter()
        .filter(|message| {
            message.role == Role::User
                && message
                    .content
                    .iter()
                    .any(|content| matches!(content, MessageContent::RuntimeHandle { .. }))
        })
        .collect::<Vec<_>>();
    assert!(!terminal_messages.is_empty());
    assert!(terminal_messages.len() <= 2);
    assert!(terminal_messages.iter().all(|message| {
        message
            .content
            .iter()
            .all(|content| matches!(content, MessageContent::RuntimeHandle { .. }))
    }));
    let terminal_results = messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            MessageContent::RuntimeHandle {
                status,
                content,
                metadata,
                ..
            } => Some((
                status.as_str(),
                content.as_str(),
                metadata.artifact.as_ref(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        terminal_results
            .iter()
            .all(|(_, _, artifact)| artifact.is_none())
    );
    assert!(terminal_results.iter().any(|(status, content, _)| {
        *status == "completed" && content.contains("done: child one")
    }));
    assert!(terminal_results.iter().any(|(status, content, _)| {
        *status == "failed" && content.contains("scripted child failure")
    }));
    assert!(Path::new(&store.paths(&parent.run_id).final_output).is_file());
}

struct LastStepBackgroundProvider;

#[async_trait]
impl ModelProvider for LastStepBackgroundProvider {
    fn name(&self) -> &str {
        "last-step-background"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        let first_user = first_user_text(&request);
        if first_user == "slow child" {
            tokio::time::sleep(Duration::from_millis(50)).await;
            return Ok(text_response("child result", ModelUsage::default()));
        }
        let has_delegate_result = request.messages.iter().any(|message| {
            message.content.iter().any(|content| {
                matches!(content, MessageContent::ToolResult { call_id, .. } if call_id == "delegate-edge")
            })
        });
        let has_background_result = request.messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|content| matches!(content, MessageContent::RuntimeHandle { .. }))
        });
        if !has_delegate_result {
            return Ok(tool_response(
                vec![fiasco_call(
                    "delegate-edge",
                    ["agent", "start", "name=slow_child", "prompt=slow child"],
                    None,
                )],
                ModelUsage::default(),
            ));
        }
        Ok(text_response(
            if has_background_result {
                "parent consumed child result"
            } else {
                "premature final"
            },
            ModelUsage::default(),
        ))
    }
}

#[tokio::test]
async fn background_completion_gets_a_reconciliation_call_without_a_step_cap() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: Arc::new(LastStepBackgroundProvider),
        model: "scripted".to_owned(),
        workspace: workspace.path().to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools: ToolRegistry::default(),
        artifacts: ArtifactStore::default(),
        store: store.clone(),
        hooks: HookPipeline::new(),
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions {
            max_parallel_model_calls: 2,
            ..RunnerOptions::default()
        },
    });

    let result = runner.run(RunRequest::root("edge parent")).await.unwrap();
    assert_eq!(result.final_output, "parent consumed child result");
    let messages = store.load_messages(&result.run_id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .count(),
        3
    );
}

struct SlowOutputTool;

#[async_trait]
impl Tool for SlowOutputTool {
    fn spec(&self) -> fiasco::model::ToolSpec {
        fiasco::model::ToolSpec {
            name: "slow_output".to_owned(),
            description: "Return a delayed test result".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    async fn execute(&self, _context: ToolContext, _arguments: Value) -> Result<RawToolOutput> {
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        Ok(RawToolOutput::text("x".repeat(512)))
    }
}

struct WaitingProvider;

#[async_trait]
impl ModelProvider for WaitingProvider {
    fn name(&self) -> &str {
        "waiting"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        let tool_results = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| match content {
                MessageContent::ToolResult {
                    call_id, content, ..
                } => Some((call_id.as_str(), content.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();
        if tool_results
            .iter()
            .any(|(call_id, _)| *call_id == "wait-call")
        {
            return Ok(text_response("joined", ModelUsage::default()));
        }
        if let Some((_, content)) = tool_results
            .iter()
            .find(|(call_id, _)| *call_id == "slow-call")
        {
            let handle =
                runtime_handle_id(content).context("runtime handle notice omitted handle")?;
            return Ok(tool_response(
                vec![fiasco_call(
                    "wait-call",
                    ["agent", "wait", &format!("handles={}", json!([handle]))],
                    None,
                )],
                ModelUsage::default(),
            ));
        }
        Ok(tool_response(
            vec![ToolCall {
                id: "slow-call".to_owned(),
                name: "slow_output".to_owned(),
                arguments: json!({}).into(),
            }],
            ModelUsage::default(),
        ))
    }
}

#[tokio::test]
async fn wait_joins_a_background_tool_without_duplicate_result_injection() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    let mut tools = ToolRegistry::default();
    tools.register(Arc::new(SlowOutputTool)).unwrap();
    let mut hooks = HookPipeline::new();
    hooks.register(CommandHook::new(
        "record-before",
        HookEvent::ToolBefore,
        "sh",
        vec!["-c".into(), "tee -a hooks.log".into()],
    ));
    hooks.register(CommandHook::new(
        "record-after",
        HookEvent::ToolAfter,
        "sh",
        vec!["-c".into(), "tee -a hooks.log".into()],
    ));
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: Arc::new(WaitingProvider),
        model: "scripted".to_owned(),
        workspace: workspace.path().to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools,
        artifacts: ArtifactStore::new(ArtifactPolicy {
            inline_limit_bytes: 256,
            preview_head_bytes: 32,
            preview_tail_bytes: 32,
        }),
        store: store.clone(),
        hooks,
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions {
            foreground_tool_timeout_seconds: 1,
            ..RunnerOptions::default()
        },
    });

    let result = runner.run(RunRequest::root("wait for tool")).await.unwrap();
    assert_eq!(result.final_output, "joined");
    let messages = store.load_messages(&result.run_id).await.unwrap();
    let acknowledgement_handle = messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|content| match content {
            MessageContent::ToolResult {
                call_id, content, ..
            } if call_id == "slow-call" => runtime_handle_id(content),
            _ => None,
        })
        .unwrap();
    let (terminal_handle, artifact_path) = messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|content| match content {
            MessageContent::RuntimeHandle {
                handle, metadata, ..
            } => Some((handle.clone(), metadata.artifact.as_ref()?.path.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(terminal_handle, acknowledgement_handle);
    assert!(artifact_path.contains("slow-call"));
    let reloaded = RunDirStore::new(workspace.path())
        .load_messages(&result.run_id)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_vec(&reloaded).unwrap(),
        serde_json::to_vec(&messages).unwrap()
    );
    assert!(
        !tokio::fs::try_exists(store.paths(&result.run_id).directory.join("tasks"))
            .await
            .unwrap()
    );
    let events = tokio::fs::read_to_string(store.paths(&result.run_id).events)
        .await
        .unwrap();
    assert!(events.contains("\"type\":\"tool_started\""));
    assert!(events.contains("\"name\":\"slow_output\""));
    assert!(events.contains("\"type\":\"artifact_created\""));
    let hooks = tokio::fs::read_to_string(workspace.path().join("hooks.log"))
        .await
        .unwrap();
    assert!(hooks.contains("\"name\":\"slow_output\""));
    assert!(hooks.contains("\"call_id\":\"slow-call\""));
}

struct LongLoopProvider(Arc<AtomicUsize>);

#[async_trait]
impl ModelProvider for LongLoopProvider {
    fn name(&self) -> &str {
        "long-loop"
    }

    async fn complete(
        &self,
        _request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        let call = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        if call <= 40 {
            return Ok(tool_response(
                vec![ToolCall {
                    id: format!("long-call-{call}"),
                    name: "side_effect".to_owned(),
                    arguments: json!({}).into(),
                }],
                ModelUsage::default(),
            ));
        }
        Ok(text_response(
            "finished after 40 tools",
            ModelUsage::default(),
        ))
    }
}

#[tokio::test]
async fn agent_loop_has_no_model_step_cap() {
    let workspace = TempDir::new().unwrap();
    let model_calls = Arc::new(AtomicUsize::new(0));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools
        .register(Arc::new(CountingTool(tool_calls.clone())))
        .unwrap();
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: Arc::new(LongLoopProvider(model_calls.clone())),
        model: "scripted".to_owned(),
        workspace: workspace.path().to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools,
        artifacts: ArtifactStore::default(),
        store: RunDirStore::new(workspace.path()),
        hooks: HookPipeline::new(),
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions {
            max_subagent_depth: 0,
            ..RunnerOptions::default()
        },
    });

    let result = runner.run(RunRequest::root("keep going")).await.unwrap();
    assert_eq!(result.final_output, "finished after 40 tools");
    assert_eq!(model_calls.load(Ordering::SeqCst), 41);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 40);
}

struct SteeringGateTool;

#[async_trait]
impl Tool for SteeringGateTool {
    fn spec(&self) -> fiasco::model::ToolSpec {
        fiasco::model::ToolSpec {
            name: "steering_gate".to_owned(),
            description: "Hold a child tool batch briefly".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    async fn execute(&self, _context: ToolContext, _arguments: Value) -> Result<RawToolOutput> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(RawToolOutput::text("gate complete"))
    }
}

struct SteeringProvider {
    child_started: Arc<tokio::sync::Notify>,
    verified: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl ModelProvider for SteeringProvider {
    fn name(&self) -> &str {
        "steering"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        if first_user_text(&request) == "child steer target" {
            let steer_index = request.messages.iter().position(|message| {
                message.role == Role::User && message.visible_text() == "take the steered path"
            });
            if let Some(steer_index) = steer_index {
                let tool_index = request
                    .messages
                    .iter()
                    .position(|message| {
                        message.content.iter().any(|content| {
                            matches!(
                                content,
                                MessageContent::ToolResult { call_id, .. }
                                    if call_id == "child-gate-call"
                            )
                        })
                    })
                    .context("steered child request omitted the completed tool result")?;
                if steer_index <= tool_index {
                    bail!("steering was inserted before the child's tool batch completed");
                }
                self.verified
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                return Ok(text_response(
                    "steered child complete",
                    ModelUsage::default(),
                ));
            }
            self.child_started.notify_one();
            return Ok(tool_response(
                vec![ToolCall {
                    id: "child-gate-call".to_owned(),
                    name: "steering_gate".to_owned(),
                    arguments: json!({}).into(),
                }],
                ModelUsage::default(),
            ));
        }

        let tool_results = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| match content {
                MessageContent::ToolResult {
                    call_id, content, ..
                } => Some((call_id.as_str(), content.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();
        if request.messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|content| matches!(content, MessageContent::RuntimeHandle { .. }))
        }) {
            return Ok(text_response(
                "parent received steered child",
                ModelUsage::default(),
            ));
        }
        if tool_results
            .iter()
            .any(|(call_id, _)| *call_id == "steer-call")
        {
            let handle = tool_results
                .iter()
                .find(|(call_id, _)| *call_id == "delegate-steered-child")
                .and_then(|(_, content)| runtime_handle_id(content))
                .context("delegate result omitted handle")?;
            return Ok(tool_response(
                vec![fiasco_call(
                    "wait-steered-child",
                    ["agent", "wait", &format!("handles={}", json!([handle]))],
                    None,
                )],
                ModelUsage::default(),
            ));
        }
        if let Some((_, content)) = tool_results
            .iter()
            .find(|(call_id, _)| *call_id == "delegate-steered-child")
        {
            self.child_started.notified().await;
            let handle = runtime_handle_id(content).context("delegate result omitted handle")?;
            return Ok(tool_response(
                vec![fiasco_call(
                    "steer-call",
                    [
                        "agent",
                        "send",
                        &format!("handle={handle}"),
                        "mode=steer",
                        "message=take the steered path",
                    ],
                    None,
                )],
                ModelUsage::default(),
            ));
        }
        Ok(tool_response(
            vec![fiasco_call(
                "delegate-steered-child",
                [
                    "agent",
                    "start",
                    "name=steer_target",
                    "prompt=child steer target",
                ],
                None,
            )],
            ModelUsage::default(),
        ))
    }
}

#[tokio::test]
async fn steer_is_delivered_after_the_childs_current_tool_batch_before_its_next_model_call() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    let child_started = Arc::new(tokio::sync::Notify::new());
    let verified = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut tools = ToolRegistry::default();
    tools.register(Arc::new(SteeringGateTool)).unwrap();
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: Arc::new(SteeringProvider {
            child_started,
            verified: verified.clone(),
        }),
        model: "scripted".to_owned(),
        workspace: workspace.path().to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools,
        artifacts: ArtifactStore::default(),
        store: store.clone(),
        hooks: HookPipeline::new(),
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions {
            max_parallel_model_calls: 2,
            ..RunnerOptions::default()
        },
    });

    let result = runner.run(RunRequest::root("steer child")).await.unwrap();
    assert_eq!(result.final_output, "parent received steered child");
    assert!(verified.load(std::sync::atomic::Ordering::SeqCst));
    let events = tokio::fs::read_to_string(store.paths(&result.run_id).events)
        .await
        .unwrap();
    assert!(events.contains("\"type\":\"agent_message_queued\""));
    assert!(events.contains("\"mode\":\"steer\""));
}

struct FollowupProvider {
    child_started: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ModelProvider for FollowupProvider {
    fn name(&self) -> &str {
        "followup"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        if first_user_text(&request) == "child followup target" {
            let followup_index = request.messages.iter().position(|message| {
                message.role == Role::User && message.visible_text() == "run the second analysis"
            });
            if let Some(followup_index) = followup_index {
                let first_result_index = request
                    .messages
                    .iter()
                    .position(|message| {
                        message.role == Role::Assistant
                            && message.visible_text() == "first activity complete"
                    })
                    .context("follow-up activity omitted the first activity result")?;
                if followup_index <= first_result_index {
                    bail!("follow-up was inserted before the first activity completed");
                }
                return Ok(text_response(
                    "second activity complete",
                    ModelUsage::default(),
                ));
            }
            let gate_done = request.messages.iter().any(|message| {
                message.content.iter().any(|content| {
                    matches!(
                        content,
                        MessageContent::ToolResult { call_id, .. }
                            if call_id == "followup-gate-call"
                    )
                })
            });
            if gate_done {
                return Ok(text_response(
                    "first activity complete",
                    ModelUsage::default(),
                ));
            }
            self.child_started.notify_one();
            return Ok(tool_response(
                vec![ToolCall {
                    id: "followup-gate-call".to_owned(),
                    name: "steering_gate".to_owned(),
                    arguments: json!({}).into(),
                }],
                ModelUsage::default(),
            ));
        }

        let tool_results = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| match content {
                MessageContent::ToolResult {
                    call_id, content, ..
                } => Some((call_id.as_str(), content.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();
        let handle = tool_results
            .iter()
            .find(|(call_id, _)| *call_id == "delegate-followup-child")
            .and_then(|(_, content)| runtime_handle_id(content));
        if let Some((_, content)) = tool_results
            .iter()
            .find(|(call_id, _)| *call_id == "close-followup-child")
        {
            let closed: Value = serde_json::from_str(content)?;
            if closed["status"] != "closed" {
                bail!("close did not close the reusable agent: {closed}");
            }
            return Ok(text_response(
                "parent completed reusable agent flow",
                ModelUsage::default(),
            ));
        }
        if let Some((_, content)) = tool_results
            .iter()
            .find(|(call_id, _)| *call_id == "list-followup-child")
        {
            let listed: Value = serde_json::from_str(content)?;
            let listed_agent = listed["handles"]
                .as_array()
                .and_then(|handles| handles.first())
                .context("list_handles omitted the reusable agent")?;
            if listed_agent["handle"].as_str() != handle.as_deref()
                || listed_agent["status"] != "idle"
            {
                bail!("list_handles returned the wrong reusable agent state: {listed}");
            }
            return Ok(tool_response(
                vec![fiasco_call(
                    "close-followup-child",
                    [
                        "agent",
                        "close",
                        &handle.context("delegate result omitted handle")?,
                    ],
                    None,
                )],
                ModelUsage::default(),
            ));
        }
        let output_count = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter(|content| match content {
                MessageContent::RuntimeHandle {
                    handle: output_handle,
                    ..
                } => Some(output_handle) == handle.as_ref(),
                _ => false,
            })
            .count();
        if output_count == 2 {
            return Ok(tool_response(
                vec![fiasco_call("list-followup-child", ["agent", "list"], None)],
                ModelUsage::default(),
            ));
        }
        if tool_results
            .iter()
            .any(|(call_id, _)| *call_id == "wait-followup-child")
        {
            return Ok(text_response(
                "continue supervising the reusable agent",
                ModelUsage::default(),
            ));
        }
        if let Some((_, content)) = tool_results
            .iter()
            .find(|(call_id, _)| *call_id == "send-followup-child")
        {
            let sent: Value = serde_json::from_str(content)?;
            if sent["accepted_as"] != "queued_followup" || sent["requested_mode"] != "followup" {
                bail!("send_message did not queue the followup: {sent}");
            }
            return Ok(tool_response(
                vec![fiasco_call(
                    "wait-followup-child",
                    [
                        "agent",
                        "wait",
                        &format!(
                            "handles={}",
                            json!([handle.context("delegate result omitted handle")?])
                        ),
                    ],
                    None,
                )],
                ModelUsage::default(),
            ));
        }
        if let Some(handle) = handle {
            self.child_started.notified().await;
            return Ok(tool_response(
                vec![fiasco_call(
                    "send-followup-child",
                    [
                        "agent",
                        "send",
                        &format!("handle={handle}"),
                        "mode=followup",
                        "message=run the second analysis",
                    ],
                    None,
                )],
                ModelUsage::default(),
            ));
        }
        Ok(tool_response(
            vec![fiasco_call(
                "delegate-followup-child",
                [
                    "agent",
                    "start",
                    "name=followup_target",
                    "prompt=child followup target",
                ],
                None,
            )],
            ModelUsage::default(),
        ))
    }
}

#[tokio::test]
async fn followup_reuses_one_child_then_list_and_close_preserve_its_outputs() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    let child_started = Arc::new(tokio::sync::Notify::new());
    let mut tools = ToolRegistry::default();
    tools.register(Arc::new(SteeringGateTool)).unwrap();
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: Arc::new(FollowupProvider { child_started }),
        model: "scripted".to_owned(),
        workspace: workspace.path().to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools,
        artifacts: ArtifactStore::default(),
        store: store.clone(),
        hooks: HookPipeline::new(),
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions {
            max_parallel_model_calls: 2,
            ..RunnerOptions::default()
        },
    });

    let result = runner
        .run(RunRequest::root("follow up with one child"))
        .await
        .unwrap();
    assert_eq!(result.final_output, "parent completed reusable agent flow");
    let child_run_id = std::fs::read_dir(workspace.path().join(".fiasco/runs"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .find(|run_id| run_id != &result.run_id)
        .unwrap();
    assert_eq!(
        store.load_run(&child_run_id).await.unwrap().state,
        RunState::Closed
    );
    let parent_events = tokio::fs::read_to_string(store.paths(&result.run_id).events)
        .await
        .unwrap();
    assert_eq!(
        parent_events
            .matches("\"type\":\"agent_activity_completed\"")
            .count(),
        2
    );
    assert_eq!(
        parent_events.matches("\"type\":\"agent_closed\"").count(),
        1
    );
    let child_events = tokio::fs::read_to_string(store.paths(&child_run_id).events)
        .await
        .unwrap();
    assert_eq!(
        child_events
            .matches("\"type\":\"run_activity_completed\"")
            .count(),
        2
    );
    assert_eq!(child_events.matches("\"type\":\"run_started\"").count(), 1);
    assert_eq!(child_events.matches("\"type\":\"run_resumed\"").count(), 1);
    assert!(!child_events.contains("\"type\":\"run_completed\""));
    let child_messages = store.load_messages(&child_run_id).await.unwrap();
    assert!(child_messages.iter().all(|message| {
        message.content.iter().all(|content| {
            !matches!(
                content,
                MessageContent::RuntimeReminder { text }
                    if text.contains("previous fiasco process stopped")
            )
        })
    }));
    assert_eq!(
        child_messages
            .iter()
            .filter(|message| {
                message.role == Role::User && message.visible_text() == "run the second analysis"
            })
            .count(),
        1
    );
    let parent_messages = store.load_messages(&result.run_id).await.unwrap();
    let outputs = parent_messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            MessageContent::RuntimeHandle {
                handle, content, ..
            } if handle == &child_run_id => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        outputs,
        ["first activity complete", "second activity complete"]
    );
    assert!(
        !tokio::fs::try_exists(store.paths(&result.run_id).directory.join("tasks"))
            .await
            .unwrap()
    );
}

struct StopThenSendProvider {
    child_background_started: Arc<tokio::sync::Notify>,
    wait_calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for StopThenSendProvider {
    fn name(&self) -> &str {
        "stop-then-send"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        if first_user_text(&request) == "child stop target" {
            if request.messages.iter().any(|message| {
                message.role == Role::User && message.visible_text() == "resume immediately"
            }) {
                return Ok(text_response(
                    "resumed activity complete",
                    ModelUsage::default(),
                ));
            }
            let tool_results = request
                .messages
                .iter()
                .flat_map(|message| &message.content)
                .filter_map(|content| match content {
                    MessageContent::ToolResult {
                        call_id, content, ..
                    } => Some((call_id.as_str(), content.as_str())),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if let Some((_, content)) = tool_results
                .iter()
                .find(|(call_id, _)| *call_id == "child-hang-call")
            {
                let handle = runtime_handle_id(content).context("hanging result omitted handle")?;
                self.child_background_started.notify_one();
                return Ok(tool_response(
                    vec![fiasco_call(
                        "wait-child-hang",
                        ["agent", "wait", &format!("handles={}", json!([handle]))],
                        None,
                    )],
                    ModelUsage::default(),
                ));
            }
            return Ok(tool_response(
                vec![ToolCall {
                    id: "child-hang-call".to_owned(),
                    name: "hanging".to_owned(),
                    arguments: json!({}).into(),
                }],
                ModelUsage::default(),
            ));
        }

        let tool_results = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| match content {
                MessageContent::ToolResult {
                    call_id, content, ..
                } => Some((call_id.as_str(), content.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();
        let handle = tool_results
            .iter()
            .find(|(call_id, _)| *call_id == "delegate-stop-child")
            .and_then(|(_, content)| runtime_handle_id(content));
        if let Some((_, content)) = tool_results
            .iter()
            .find(|(call_id, _)| *call_id == "close-stopped-child")
        {
            let closed: Value = serde_json::from_str(content)?;
            if closed["status"] != "closed" {
                bail!("close did not close the stopped child: {closed}");
            }
            return Ok(text_response(
                "stop then immediate send completed",
                ModelUsage::default(),
            ));
        }
        let completed_after_stop = request.messages.iter().any(|message| {
            message.content.iter().any(|content| {
                matches!(
                    content,
                    MessageContent::RuntimeHandle {
                        handle: output_handle,
                        status,
                        ..
                    } if Some(output_handle) == handle.as_ref() && status == "completed"
                )
            })
        });
        if completed_after_stop {
            return Ok(tool_response(
                vec![fiasco_call(
                    "close-stopped-child",
                    [
                        "agent",
                        "close",
                        &handle.context("delegate result omitted handle")?,
                    ],
                    None,
                )],
                ModelUsage::default(),
            ));
        }
        if let Some((_, content)) = tool_results
            .iter()
            .find(|(call_id, _)| *call_id == "send-stopped-child")
        {
            let sent: Value = serde_json::from_str(content)?;
            if sent["accepted_as"] != "started"
                || !matches!(sent["status"].as_str(), Some("queued" | "running"))
            {
                bail!("send_message did not restart the stopped child: {sent}");
            }
            let wait = self.wait_calls.fetch_add(1, Ordering::SeqCst);
            return Ok(tool_response(
                vec![fiasco_call(
                    format!("wait-stopped-child-{wait}"),
                    [
                        "agent",
                        "wait",
                        &format!(
                            "handles={}",
                            json!([handle.context("delegate result omitted handle")?])
                        ),
                    ],
                    None,
                )],
                ModelUsage::default(),
            ));
        }
        if tool_results
            .iter()
            .any(|(call_id, _)| call_id.starts_with("wait-stopped-child-"))
        {
            let wait = self.wait_calls.fetch_add(1, Ordering::SeqCst);
            return Ok(tool_response(
                vec![fiasco_call(
                    format!("wait-stopped-child-{wait}"),
                    [
                        "agent",
                        "wait",
                        &format!(
                            "handles={}",
                            json!([handle.context("delegate result omitted handle")?])
                        ),
                    ],
                    None,
                )],
                ModelUsage::default(),
            ));
        }
        if let Some((_, content)) = tool_results
            .iter()
            .find(|(call_id, _)| *call_id == "stop-child")
        {
            let stopped: Value = serde_json::from_str(content)?;
            if stopped["status"] != "idle" {
                bail!("stop returned the wrong reusable state: {stopped}");
            }
            return Ok(tool_response(
                vec![fiasco_call(
                    "send-stopped-child",
                    [
                        "agent",
                        "send",
                        &format!(
                            "handle={}",
                            handle.context("delegate result omitted handle")?
                        ),
                        "mode=followup",
                        "message=resume immediately",
                    ],
                    None,
                )],
                ModelUsage::default(),
            ));
        }
        if let Some(handle) = handle {
            self.child_background_started.notified().await;
            return Ok(tool_response(
                vec![fiasco_call("stop-child", ["agent", "stop", &handle], None)],
                ModelUsage::default(),
            ));
        }
        Ok(tool_response(
            vec![fiasco_call(
                "delegate-stop-child",
                [
                    "agent",
                    "start",
                    "name=stop_target",
                    "prompt=child stop target",
                ],
                None,
            )],
            ModelUsage::default(),
        ))
    }
}

#[tokio::test]
async fn stop_then_immediate_send_waits_for_child_cleanup_before_reuse() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    let mut tools = ToolRegistry::default();
    tools.register(Arc::new(HangingTool)).unwrap();
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: Arc::new(StopThenSendProvider {
            child_background_started: Arc::new(tokio::sync::Notify::new()),
            wait_calls: AtomicUsize::new(0),
        }),
        model: "scripted".to_owned(),
        workspace: workspace.path().to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools,
        artifacts: ArtifactStore::default(),
        store: store.clone(),
        hooks: HookPipeline::new(),
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions {
            foreground_tool_timeout_seconds: 1,
            handle_wait_timeout_seconds: 1,
            max_parallel_model_calls: 2,
            ..RunnerOptions::default()
        },
    });

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        runner.run(RunRequest::root("stop and immediately reuse child")),
    )
    .await
    .expect("stop and immediate send flow timed out")
    .unwrap();
    assert_eq!(result.final_output, "stop then immediate send completed");
    let child_run_id = std::fs::read_dir(workspace.path().join(".fiasco/runs"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .find(|run_id| run_id != &result.run_id)
        .unwrap();
    assert_eq!(
        store.load_run(&child_run_id).await.unwrap().state,
        RunState::Closed
    );
    let outputs = store
        .load_messages(&result.run_id)
        .await
        .unwrap()
        .into_iter()
        .flat_map(|message| message.content)
        .filter_map(|content| match content {
            MessageContent::RuntimeHandle {
                handle,
                status,
                content,
                ..
            } if handle == child_run_id => Some((status, content)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        outputs,
        [
            (
                "cancelled".to_owned(),
                "agent activity was stopped by the parent; any incomplete trailing tool turn was discarded"
                    .to_owned(),
            ),
            (
                "completed".to_owned(),
                "resumed activity complete".to_owned()
            ),
        ]
    );
}

struct HangingTool;

#[derive(Default)]
struct YieldingSerializedEventSink {
    lock: tokio::sync::Mutex<()>,
    tool_starts: AtomicUsize,
}

#[async_trait]
impl fiasco::events::EventSink for YieldingSerializedEventSink {
    async fn emit(&self, event: &fiasco::events::RuntimeEvent) -> Result<()> {
        let _guard = self.lock.lock().await;
        if matches!(
            event.kind,
            fiasco::events::RuntimeEventKind::ToolStarted { .. }
        ) && self.tool_starts.fetch_add(1, Ordering::SeqCst) == 1
        {
            tokio::task::yield_now().await;
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for HangingTool {
    fn spec(&self) -> fiasco::model::ToolSpec {
        fiasco::model::ToolSpec {
            name: "hanging".to_owned(),
            description: "Wait until cancelled".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    async fn execute(&self, _context: ToolContext, _arguments: Value) -> Result<RawToolOutput> {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(RawToolOutput::text("too late"))
    }
}

struct FailingParentProvider;

#[async_trait]
impl ModelProvider for FailingParentProvider {
    fn name(&self) -> &str {
        "failing-parent"
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        if request
            .messages
            .iter()
            .any(|message| message.role == Role::Tool)
        {
            bail!("scripted parent failure");
        }
        Ok(tool_response(
            vec![
                ToolCall {
                    id: "hanging-call-1".to_owned(),
                    name: "hanging".to_owned(),
                    arguments: json!({}).into(),
                },
                ToolCall {
                    id: "hanging-call-2".to_owned(),
                    name: "hanging".to_owned(),
                    arguments: json!({}).into(),
                },
            ],
            ModelUsage::default(),
        ))
    }
}

#[tokio::test]
async fn parent_failure_aborts_process_local_tool_jobs_without_persisting_them() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    let mut tools = ToolRegistry::default();
    tools.register(Arc::new(HangingTool)).unwrap();
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: Arc::new(FailingParentProvider),
        model: "scripted".to_owned(),
        workspace: workspace.path().to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools,
        artifacts: ArtifactStore::default(),
        store: store.clone(),
        hooks: HookPipeline::new(),
        memory: None,
        // The second direct future yields while holding an event-sink lock.
        // Promotion must resume every pending future before any promotion
        // awaits handle events from the same sink.
        extra_events: Arc::new(YieldingSerializedEventSink::default()),
        options: RunnerOptions {
            foreground_tool_timeout_seconds: 0,
            ..RunnerOptions::default()
        },
    });

    let error = tokio::time::timeout(
        Duration::from_secs(2),
        runner.run(RunRequest::root("fail after promotion")),
    )
    .await
    .expect("multi-call promotion deadlocked on an in-flight tool event")
    .unwrap_err();
    assert!(format!("{error:#}").contains("scripted parent failure"));
    let run_root = workspace.path().join(".fiasco/runs");
    let parent_dir = std::fs::read_dir(&run_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(!parent_dir.join("tasks").exists());
}

struct SlowModelProvider;

#[async_trait]
impl ModelProvider for SlowModelProvider {
    fn name(&self) -> &str {
        "slow-model"
    }

    async fn complete(
        &self,
        _request: ModelRequest,
        _events: SharedEventSink,
    ) -> Result<ModelResponse> {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(text_response("too late", ModelUsage::default()))
    }
}

#[tokio::test]
async fn model_requests_have_a_runtime_deadline() {
    let workspace = TempDir::new().unwrap();
    let store = RunDirStore::new(workspace.path());
    let runner = AgentRunner::new(AgentRunnerConfig {
        provider: Arc::new(SlowModelProvider),
        model: "scripted".to_owned(),
        workspace: workspace.path().to_path_buf(),
        skill_catalog: String::new(),
        mcp_catalog: String::new(),
        tools: ToolRegistry::default(),
        artifacts: ArtifactStore::default(),
        store: store.clone(),
        hooks: HookPipeline::new(),
        memory: None,
        extra_events: Arc::new(NoopEventSink),
        options: RunnerOptions {
            model_request_deadline_seconds: 1,
            ..RunnerOptions::default()
        },
    });

    let error = runner
        .run(RunRequest::root("wait forever"))
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("model request deadline exceeded 1 seconds"),
        "{error:#}"
    );
    let run_id = std::fs::read_dir(workspace.path().join(".fiasco/runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .into_owned();
    assert_eq!(store.load_run(&run_id).await.unwrap().state, RunState::Open);
}
