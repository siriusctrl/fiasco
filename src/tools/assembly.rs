use std::sync::Arc;

use anyhow::Result;

use crate::{
    agent::handle::RuntimeHandleManager, skills::SkillRegistry, trajectory::TrajectoryReader,
};

use super::{
    BashTool, DelegateTool, FiascoTool, LoadSkillTool, ReadTool, ToolRegistry, WebSearchTool,
    WriteTool, handle, history,
};

/// Assemble process-wide native and command-capable tools. `RunToolAssembly`
/// separates their provider-visible and hidden roles after adding run scope.
pub fn build_app_tools(
    skills: Arc<SkillRegistry>,
    web_search: Option<WebSearchTool>,
    image_enabled: bool,
) -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::default();
    registry.register(Arc::new(ReadTool::new(image_enabled)))?;
    registry.register(Arc::new(WriteTool::default()))?;
    registry.register(Arc::new(BashTool))?;
    registry.register(Arc::new(LoadSkillTool::new(skills)))?;
    if let Some(web_search) = web_search {
        registry.register(Arc::new(web_search))?;
    }
    Ok(registry)
}

/// The one assembly path for the frozen provider schemas and command catalog
/// exposed by an agent run.
pub struct RunToolAssembly {
    registry: ToolRegistry,
    commands: ToolRegistry,
}

impl RunToolAssembly {
    pub fn new(
        mut registry: ToolRegistry,
        reader: Arc<dyn TrajectoryReader>,
        history_search_max_matches: usize,
    ) -> Result<Self> {
        let mut commands = ToolRegistry::default();
        for name in ["load_skill", "web_search", "mcp"] {
            if let Some(tool) = registry.remove(name) {
                commands.register(tool)?;
            }
        }
        history::register(&mut commands, reader, history_search_max_matches)?;
        Ok(Self { registry, commands })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.registry.contains(name)
    }

    pub fn finish(mut self, handles: Arc<RuntimeHandleManager>) -> Result<ToolRegistry> {
        self.commands
            .register(Arc::new(DelegateTool::new(handles.clone())))?;
        handle::register_controls(&mut self.commands, handles)?;
        let redirect_writer = self
            .registry
            .get("write")
            .unwrap_or_else(|| Arc::new(WriteTool::default()));
        self.registry
            .register(Arc::new(FiascoTool::new(self.commands, redirect_writer)?))?;
        Ok(self.registry)
    }
}
