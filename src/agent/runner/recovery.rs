use std::sync::Arc;

use anyhow::{Result, ensure};

use crate::{
    model::{Message, MessageContent, Role},
    storage::{RunDirStore, RunState},
    trajectory::TrajectoryMessage,
};

use super::{AgentRunner, RunRequest, RunResult, lifecycle::RunMode};
use crate::agent::{
    compaction::estimate_message_tokens,
    handle::{AgentMailbox, HandleOutputNotice},
};

const RESTART_REMINDER: &str = "<runtime-reminder>\nThe previous fiasco process stopped. Any incomplete trailing tool turn was discarded; activities and asynchronous tool jobs from that process stopped and were not resumed. Mailbox input and undelivered results were also discarded. Existing agent threads keep their complete messages and remain available through the `fiasco` commands `agent list`, `agent inspect`, and an explicit `agent send`. Workspace or external side effects may already have occurred, so inspect current state before retrying operations.\n</runtime-reminder>";

impl AgentRunner {
    pub async fn resume(self: &Arc<Self>, run_id: impl Into<String>) -> Result<RunResult> {
        self.start_existing_run(run_id.into(), None, None, None)
            .await
    }

    pub(crate) async fn run_child_activity(
        self: &Arc<Self>,
        run_id: String,
        expected_parent_run_id: &str,
        mailbox: AgentMailbox,
        cleanup_done: tokio::sync::oneshot::Sender<()>,
    ) -> Result<RunResult> {
        self.start_existing_run(
            run_id,
            Some(expected_parent_run_id),
            Some(mailbox),
            Some(cleanup_done),
        )
        .await
    }

    async fn start_existing_run(
        self: &Arc<Self>,
        run_id: String,
        expected_parent_run_id: Option<&str>,
        mailbox: Option<AgentMailbox>,
        cleanup_done: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<RunResult> {
        let lease = self.store.acquire_run_lease(&run_id).await?;
        let record = self.store.load_run(&run_id).await?;
        match expected_parent_run_id {
            Some(parent_run_id) => ensure!(
                record.parent_run_id.as_deref() == Some(parent_run_id),
                "child run `{run_id}` does not belong to parent `{parent_run_id}`"
            ),
            None => ensure!(
                record.parent_run_id.is_none(),
                "run `{run_id}` is a child run; resume its parent `{}` instead",
                record.parent_run_id.as_deref().unwrap_or_default()
            ),
        }
        ensure!(
            !matches!(record.state, RunState::Completed | RunState::Closed),
            "run `{run_id}` is already {:?}",
            record.state
        );
        ensure!(
            record.cwd == self.workspace,
            "run `{run_id}` belongs to workspace {}, not {}",
            record.cwd.display(),
            self.workspace.display()
        );
        ensure!(
            record.provider == self.provider.name(),
            "run `{run_id}` used provider `{}` but current provider is `{}`",
            record.provider,
            self.provider.name()
        );
        record.verify_provider_resume_fingerprint(&self.provider.resume_fingerprint())?;
        let request = RunRequest::from_stored(&record)?;
        let plan = self.plan(&request);
        ensure!(
            record.model == plan.model,
            "run `{run_id}` used model `{}` but current profile selects `{}`",
            record.model,
            plan.model
        );
        ensure!(
            record.model_modalities == plan.modalities,
            "run `{run_id}` used model modalities {:?} but current configuration selects {:?}",
            record.model_modalities,
            plan.modalities
        );
        self.store.prepare_resume(&run_id).await?;
        let mode = match expected_parent_run_id {
            Some(_) if self.store.load_trajectory(&run_id).await?.is_empty() => RunMode::New,
            Some(_) => RunMode::ChildActivity,
            None => RunMode::RootRestart,
        };
        self.run_with_mode(request, run_id, mode, lease.clone(), mailbox, cleanup_done)
            .await
    }
}

pub(super) async fn append_handle_results(
    store: &RunDirStore,
    run_id: &str,
    trajectory: &mut Vec<TrajectoryMessage>,
    notices: &[HandleOutputNotice],
) -> Result<u64> {
    if notices.is_empty() {
        return Ok(0);
    }
    let content = notices
        .iter()
        .map(|notice| MessageContent::RuntimeHandle {
            handle: notice.handle.clone(),
            kind: notice.kind.as_str().to_owned(),
            name: notice.name.clone(),
            status: notice.output.status.as_str().to_owned(),
            content: notice.output.content.clone(),
            metadata: notice.output.metadata.clone(),
        })
        .collect::<Vec<_>>();
    let message = Message::new(Role::User, content);
    let estimated_tokens = estimate_message_tokens(&message);
    let trajectory_record = store.append_message(run_id, &message).await?;
    trajectory.push(trajectory_record);
    Ok(estimated_tokens)
}

pub(super) async fn append_restart_reminder(
    store: &RunDirStore,
    run_id: &str,
    trajectory: &mut Vec<TrajectoryMessage>,
) -> Result<()> {
    let message = Message::new(
        Role::User,
        vec![MessageContent::RuntimeReminder {
            text: RESTART_REMINDER.to_owned(),
        }],
    );
    trajectory.push(store.append_message(run_id, &message).await?);
    Ok(())
}
