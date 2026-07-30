use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::{Result, bail};
use async_trait::async_trait;
use serde_json::Value;

use crate::model::ToolSpec;

mod assembly;
pub mod bash;
mod command;
pub mod delegate;
mod handle;
mod history;
pub mod load_skill;
mod manifest;
mod paths;
pub mod read;
pub mod web_search;
pub mod write;

pub use assembly::{RunToolAssembly, build_app_tools};
pub use bash::BashTool;
pub use command::FiascoTool;
pub use delegate::DelegateTool;
pub use load_skill::LoadSkillTool;
pub(crate) use manifest::embedded_tool_spec;
pub use read::ReadTool;
pub use web_search::WebSearchTool;
pub use write::WriteTool;

#[derive(Clone)]
pub struct ToolContext {
    pub run_id: String,
    pub call_id: String,
    pub workspace: PathBuf,
}

#[derive(Debug, Clone)]
pub struct RawToolOutput {
    pub content: Vec<u8>,
    /// Optional file containing the complete output. ArtifactStore consumes it
    /// without loading the whole file into memory.
    pub source_path: Option<PathBuf>,
    pub media_type: String,
    pub is_error: bool,
    /// Send this binary result to the model as a native image attachment in
    /// addition to preserving it as an artifact.
    pub attach_to_model: bool,
}

impl RawToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into().into_bytes(),
            source_path: None,
            media_type: "text/plain; charset=utf-8".to_owned(),
            is_error: false,
            attach_to_model: false,
        }
    }

    pub fn image(content: Vec<u8>, media_type: impl Into<String>) -> Self {
        Self {
            content,
            source_path: None,
            media_type: media_type.into(),
            is_error: false,
            attach_to_model: true,
        }
    }

    pub fn file(path: PathBuf, media_type: impl Into<String>, is_error: bool) -> Self {
        Self {
            content: Vec::new(),
            source_path: Some(path),
            media_type: media_type.into(),
            is_error,
            attach_to_model: false,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn execute(&self, context: ToolContext, arguments: Value) -> Result<RawToolOutput>;
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, RegisteredTool>,
}

#[derive(Clone)]
struct RegisteredTool {
    implementation: Arc<dyn Tool>,
    spec: ToolSpec,
}

impl ToolRegistry {
    pub fn register(&mut self, implementation: Arc<dyn Tool>) -> Result<()> {
        let spec = implementation.spec();
        let name = spec.name.clone();
        if self.tools.contains_key(&name) {
            bail!("tool `{name}` is already registered");
        }
        self.tools.insert(
            name,
            RegisteredTool {
                implementation,
                spec,
            },
        );
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).map(|tool| tool.implementation.clone())
    }

    pub fn spec(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.get(name).map(|tool| &tool.spec)
    }

    pub fn remove(&mut self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.remove(name).map(|tool| tool.implementation)
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec.clone()).collect()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
}

#[cfg(test)]
mod registry_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    struct CountingTool {
        name: &'static str,
        spec_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn spec(&self) -> ToolSpec {
            self.spec_calls.fetch_add(1, Ordering::SeqCst);
            ToolSpec {
                name: self.name.to_owned(),
                description: "test tool".to_owned(),
                input_schema: json!({"type": "object"}),
            }
        }

        async fn execute(&self, _context: ToolContext, _arguments: Value) -> Result<RawToolOutput> {
            Ok(RawToolOutput::text("ok"))
        }
    }

    #[test]
    fn registry_freezes_specs_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::default();
        registry
            .register(Arc::new(CountingTool {
                name: "backgroundable",
                spec_calls: calls.clone(),
            }))
            .unwrap();
        registry
            .register(Arc::new(CountingTool {
                name: "control",
                spec_calls: calls.clone(),
            }))
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            registry
                .specs()
                .into_iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>(),
            ["backgroundable", "control"]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
