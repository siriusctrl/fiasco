use std::{
    collections::{BTreeSet, HashSet},
    str,
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Map, Number, Value, json};

use crate::model::ToolSpec;

use super::{RawToolOutput, Tool, ToolContext, ToolRegistry};

mod parser;

#[derive(Debug, Clone, Copy)]
struct Route {
    path: &'static [&'static str],
    tool: &'static str,
    usage: &'static str,
}

const ROUTES: &[Route] = &[
    Route {
        path: &["skill", "load"],
        tool: "load_skill",
        usage: "skill load <name>",
    },
    Route {
        path: &["web", "search"],
        tool: "web_search",
        usage: "web search <query> [count=<integer>] [country=<string>] [search_lang=<string>]",
    },
    Route {
        path: &["history", "search"],
        tool: "history_search",
        usage: "history search <pattern>",
    },
    Route {
        path: &["history", "read"],
        tool: "history_read",
        usage: "history read <ref> [before=<integer>] [after=<integer>]",
    },
    Route {
        path: &["agent", "start"],
        tool: "delegate",
        usage: "agent start name=<string> prompt=<string|->",
    },
    Route {
        path: &["agent", "send"],
        tool: "send_message",
        usage: "agent send handle=<id> mode=<steer|followup> message=<string|->",
    },
    Route {
        path: &["agent", "list"],
        tool: "list_handles",
        usage: "agent list [handles=<JSON array>] [include_closed=<boolean>]",
    },
    Route {
        path: &["agent", "inspect"],
        tool: "inspect",
        usage: "agent inspect <handle> [before_seq=<integer>] [limit=<integer>]",
    },
    Route {
        path: &["agent", "wait"],
        tool: "wait",
        usage: "agent wait [handles=<JSON array>]",
    },
    Route {
        path: &["agent", "stop"],
        tool: "stop",
        usage: "agent stop <handle>",
    },
    Route {
        path: &["agent", "close"],
        tool: "close",
        usage: "agent close <handle>",
    },
    Route {
        path: &["mcp"],
        tool: "mcp",
        usage: "mcp <source> <tool> [name=value ...]",
    },
];

pub struct FiascoTool {
    commands: ToolRegistry,
    redirect_writer: Arc<dyn Tool>,
    routes: Vec<&'static Route>,
}

impl FiascoTool {
    pub fn new(commands: ToolRegistry, redirect_writer: Arc<dyn Tool>) -> Result<Self> {
        let routes = ROUTES
            .iter()
            .filter(|route| commands.contains(route.tool))
            .collect::<Vec<_>>();
        let routed_tools = routes
            .iter()
            .map(|route| route.tool)
            .collect::<HashSet<_>>();
        let unrouted = commands
            .names()
            .filter(|name| !routed_tools.contains(name))
            .collect::<Vec<_>>();
        ensure!(
            unrouted.is_empty(),
            "Fiasco command registry contains unrouted tool(s): {}",
            unrouted.join(", ")
        );
        ensure!(
            redirect_writer.spec().name == "write",
            "Fiasco redirect writer must use the `write` contract"
        );
        Ok(Self {
            commands,
            redirect_writer,
            routes,
        })
    }

    async fn run(
        &self,
        context: ToolContext,
        command: &str,
        stdin: Option<String>,
    ) -> Result<RawToolOutput> {
        let invocation = parser::parse(command)?;
        let mut input = stdin.map(|content| CommandInput {
            content: content.into_bytes(),
            media_type: "text/plain; charset=utf-8".to_owned(),
        });
        let last_index = invocation.stages.len() - 1;
        let mut final_output = None;

        for (index, stage) in invocation.stages.iter().enumerate() {
            let output = self
                .execute_stage(context.clone(), stage, input.as_ref())
                .await
                .with_context(|| {
                    format!(
                        "execute Fiasco pipeline stage {} `{}`",
                        index + 1,
                        shell_words::join(stage)
                    )
                })?;
            if output.is_error {
                return Ok(output);
            }
            if index == last_index {
                final_output = Some(output);
            } else {
                input = Some(CommandInput::from_output(output).await?);
            }
        }

        let output = final_output.expect("a parsed pipeline always has a final stage");
        if let Some(path) = invocation.redirect {
            let input = CommandInput::from_output(output).await?;
            let content = input.utf8("redirected output")?;
            return self
                .redirect_writer
                .execute(
                    context,
                    json!({
                        "path": path,
                        "content": content,
                    }),
                )
                .await
                .context("write redirected Fiasco output");
        }
        Ok(output)
    }

    async fn execute_stage(
        &self,
        context: ToolContext,
        words: &[String],
        input: Option<&CommandInput>,
    ) -> Result<RawToolOutput> {
        if words.first().is_some_and(|word| word == "help") {
            ensure!(input.is_none(), "`help` does not accept stdin");
            return self.help(&words[1..]);
        }

        let route = self
            .resolve_route(words)
            .with_context(|| format!("unknown Fiasco command `{}`", shell_words::join(words)))?;
        let tool = self
            .commands
            .get(route.tool)
            .expect("enabled route must retain its tool");
        let arguments = if route.tool == "mcp" {
            compile_mcp_arguments(&words[route.path.len()..], input)?
        } else {
            let spec = self
                .commands
                .spec(route.tool)
                .expect("enabled route must retain its spec");
            compile_tool_arguments(spec, &words[route.path.len()..], input)?
        };
        tool.execute(context, arguments).await
    }

    fn resolve_route(&self, words: &[String]) -> Option<&'static Route> {
        self.routes
            .iter()
            .copied()
            .filter(|route| {
                words.len() >= route.path.len()
                    && route
                        .path
                        .iter()
                        .zip(words)
                        .all(|(expected, actual)| expected == actual)
            })
            .max_by_key(|route| route.path.len())
    }

    fn help(&self, path: &[String]) -> Result<RawToolOutput> {
        if path.is_empty() {
            return Ok(RawToolOutput::text(self.catalog()));
        }
        let route = self
            .routes
            .iter()
            .copied()
            .find(|route| {
                route.path.len() == path.len()
                    && route
                        .path
                        .iter()
                        .zip(path)
                        .all(|(expected, actual)| expected == actual)
            })
            .with_context(|| format!("unknown Fiasco command `{}`", path.join(" ")))?;
        let spec = self
            .commands
            .spec(route.tool)
            .expect("enabled route must retain its spec");
        Ok(RawToolOutput::text(format!(
            "Usage: {}\n\n{}\n\nInput schema:\n{}",
            route.usage,
            spec.description,
            serde_json::to_string_pretty(&spec.input_schema)?
        )))
    }

    fn catalog(&self) -> String {
        let mut lines = vec![
            "Enabled Fiasco commands:".to_owned(),
            "- help [command path]".to_owned(),
        ];
        lines.extend(self.routes.iter().map(|route| format!("- {}", route.usage)));
        lines.join("\n")
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FiascoArguments {
    command: String,
    stdin: Option<String>,
}

#[async_trait]
impl Tool for FiascoTool {
    fn spec(&self) -> ToolSpec {
        let mut spec = crate::tools::embedded_tool_spec(include_str!("tool.yaml"), module_path!());
        spec.description.push_str("\n\n");
        spec.description.push_str(&self.catalog());
        spec
    }

    async fn execute(&self, context: ToolContext, arguments: Value) -> Result<RawToolOutput> {
        let arguments: FiascoArguments =
            serde_json::from_value(arguments).context("invalid fiasco arguments")?;
        ensure!(
            !arguments.command.trim().is_empty(),
            "`command` must not be empty"
        );
        self.run(context, &arguments.command, arguments.stdin).await
    }
}

struct CommandInput {
    content: Vec<u8>,
    media_type: String,
}

impl CommandInput {
    async fn from_output(output: RawToolOutput) -> Result<Self> {
        ensure!(
            !output.attach_to_model,
            "native image output cannot enter a Fiasco pipeline or redirect"
        );
        let content = match output.source_path {
            Some(path) => tokio::fs::read(&path)
                .await
                .with_context(|| format!("read pipeline output {}", path.display()))?,
            None => output.content,
        };
        Ok(Self {
            content,
            media_type: output.media_type,
        })
    }

    fn utf8(&self, purpose: &str) -> Result<&str> {
        str::from_utf8(&self.content).with_context(|| {
            format!(
                "{purpose} must be UTF-8; received {}",
                self.media_type.as_str()
            )
        })
    }
}

fn compile_mcp_arguments(words: &[String], input: Option<&CommandInput>) -> Result<Value> {
    ensure!(
        words.len() >= 2,
        "MCP command must begin with `<source> <tool>`"
    );
    let mut compiled = words.to_vec();
    let mut consumed_stdin = false;
    for word in &mut compiled {
        if word == "-" {
            *word = consume_stdin(input, &mut consumed_stdin, "MCP positional argument")?;
        } else if let Some((name, raw)) = word.split_once('=')
            && raw == "-"
        {
            let value = consume_stdin(input, &mut consumed_stdin, "MCP named argument")?;
            *word = format!("{name}={value}");
        }
    }
    if let Some(input) = input
        && !consumed_stdin
    {
        ensure!(
            compiled.len() == 2,
            "MCP pipeline input is ambiguous; use `name=-` to select its argument"
        );
        compiled.push(input.utf8("MCP pipeline input")?.to_owned());
    }
    Ok(json!({ "command": shell_words::join(compiled) }))
}

fn compile_tool_arguments(
    spec: &ToolSpec,
    tokens: &[String],
    input: Option<&CommandInput>,
) -> Result<Value> {
    let schema = &spec.input_schema;
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    let additional_properties = schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if tokens.is_empty()
        && let Some(input) = input
        && let Some(arguments) = input_object_arguments(input, &properties, &required)?
    {
        return Ok(Value::Object(arguments));
    }

    let mut arguments = Map::new();
    let mut positional = None;
    let mut consumed_stdin = false;
    for token in tokens {
        if let Some((name, raw)) = token.split_once('=') {
            ensure!(!name.is_empty(), "command argument name must not be empty");
            ensure!(
                !arguments.contains_key(name),
                "duplicate command argument `{name}`"
            );
            let property_schema = properties.get(name);
            ensure!(
                property_schema.is_some() || additional_properties,
                "unknown argument `{name}` for command `{}`",
                spec.name
            );
            let raw = if raw == "-" {
                consume_stdin(input, &mut consumed_stdin, name)?
            } else {
                raw.to_owned()
            };
            arguments.insert(
                name.to_owned(),
                compile_value(&raw, property_schema)
                    .with_context(|| format!("compile argument `{name}`"))?,
            );
        } else {
            ensure!(
                positional.is_none(),
                "command arguments must use `name=value`; only one positional value is allowed"
            );
            positional = Some(token.as_str());
        }
    }

    if let Some(raw) = positional {
        let missing_required = required
            .iter()
            .copied()
            .filter(|name| !arguments.contains_key(*name))
            .collect::<Vec<_>>();
        let target = if missing_required.len() == 1 {
            missing_required[0]
        } else if properties.len() == 1 {
            properties.keys().next().expect("one property").as_str()
        } else {
            bail!(
                "positional argument is ambiguous for command `{}`; use `name=value`",
                spec.name
            )
        };
        ensure!(
            !arguments.contains_key(target),
            "duplicate command argument `{target}`"
        );
        let raw = if raw == "-" {
            consume_stdin(input, &mut consumed_stdin, target)?
        } else {
            raw.to_owned()
        };
        arguments.insert(
            target.to_owned(),
            compile_value(&raw, properties.get(target))
                .with_context(|| format!("compile argument `{target}`"))?,
        );
    }

    if let Some(input) = input
        && !consumed_stdin
    {
        let missing_required = required
            .iter()
            .copied()
            .filter(|name| !arguments.contains_key(*name))
            .collect::<Vec<_>>();
        ensure!(
            missing_required.len() == 1,
            "command `{}` does not unambiguously consume pipeline stdin; use `name=-`",
            spec.name
        );
        let target = missing_required[0];
        arguments.insert(
            target.to_owned(),
            compile_value(input.utf8("pipeline input")?, properties.get(target))
                .with_context(|| format!("compile stdin argument `{target}`"))?,
        );
    }

    let missing = required
        .into_iter()
        .filter(|name| !arguments.contains_key(*name))
        .collect::<Vec<_>>();
    ensure!(
        missing.is_empty(),
        "missing required command argument(s): {}",
        missing.join(", ")
    );
    Ok(Value::Object(arguments))
}

fn input_object_arguments(
    input: &CommandInput,
    properties: &Map<String, Value>,
    required: &BTreeSet<&str>,
) -> Result<Option<Map<String, Value>>> {
    let Ok(Value::Object(arguments)) = serde_json::from_slice::<Value>(&input.content) else {
        return Ok(None);
    };
    if arguments.keys().any(|name| !properties.contains_key(name))
        || required.iter().any(|name| !arguments.contains_key(*name))
    {
        return Ok(None);
    }
    Ok(Some(arguments))
}

fn consume_stdin(
    input: Option<&CommandInput>,
    consumed: &mut bool,
    argument: &str,
) -> Result<String> {
    ensure!(
        !*consumed,
        "pipeline stdin is already consumed; `{argument}` cannot consume it again"
    );
    let input = input.with_context(|| format!("`{argument}=-` requires pipeline stdin"))?;
    *consumed = true;
    Ok(input.utf8("pipeline input")?.to_owned())
}

fn compile_value(raw: &str, schema: Option<&Value>) -> Result<Value> {
    let expected = schema.and_then(simple_schema_type);
    match expected {
        Some("string") => Ok(Value::String(raw.to_owned())),
        Some("boolean") => match raw {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => bail!("expected `true` or `false`"),
        },
        Some("integer") => parse_integer(raw),
        Some("number") => {
            let value: f64 = raw.parse().context("expected a JSON number")?;
            let number = Number::from_f64(value).context("number must be finite")?;
            Ok(Value::Number(number))
        }
        Some("array") => parse_compound(raw, "array", Value::is_array),
        Some("object") => parse_compound(raw, "object", Value::is_object),
        Some("null") => {
            ensure!(raw == "null", "expected `null`");
            Ok(Value::Null)
        }
        _ if raw.starts_with('[') || raw.starts_with('{') => {
            serde_json::from_str(raw).context("parse JSON argument")
        }
        _ => Ok(Value::String(raw.to_owned())),
    }
}

fn simple_schema_type(schema: &Value) -> Option<&str> {
    match schema.get("type")? {
        Value::String(kind) => Some(kind),
        Value::Array(kinds) => {
            let mut non_null = kinds
                .iter()
                .filter_map(Value::as_str)
                .filter(|kind| *kind != "null");
            let kind = non_null.next()?;
            non_null.next().is_none().then_some(kind)
        }
        _ => None,
    }
}

fn parse_integer(raw: &str) -> Result<Value> {
    if let Ok(value) = raw.parse::<i64>() {
        return Ok(Value::Number(value.into()));
    }
    let value = raw.parse::<u64>().context("expected a JSON integer")?;
    Ok(Value::Number(value.into()))
}

fn parse_compound(
    raw: &str,
    expected: &str,
    predicate: impl FnOnce(&Value) -> bool,
) -> Result<Value> {
    let value: Value =
        serde_json::from_str(raw).with_context(|| format!("parse JSON {expected}"))?;
    ensure!(predicate(&value), "expected a JSON {expected}");
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;
    use crate::tools::WriteTool;

    struct LoadTool;

    #[async_trait]
    impl Tool for LoadTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "load_skill".into(),
                description: "Load one test value.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"name": {"type": "string"}},
                    "required": ["name"],
                    "additionalProperties": false
                }),
            }
        }

        async fn execute(&self, _context: ToolContext, arguments: Value) -> Result<RawToolOutput> {
            Ok(RawToolOutput::text(
                arguments["name"].as_str().context("missing name")?,
            ))
        }
    }

    struct ArgumentsTool {
        name: &'static str,
        schema: Value,
    }

    #[async_trait]
    impl Tool for ArgumentsTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: format!("Echo {} arguments.", self.name),
                input_schema: self.schema.clone(),
            }
        }

        async fn execute(&self, _context: ToolContext, arguments: Value) -> Result<RawToolOutput> {
            Ok(RawToolOutput {
                content: serde_json::to_vec(&arguments)?,
                source_path: None,
                media_type: "application/json".into(),
                is_error: false,
                attach_to_model: false,
            })
        }
    }

    struct ErrorTool;

    #[async_trait]
    impl Tool for ErrorTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "web_search".into(),
                description: "Return a model-visible test error.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"],
                    "additionalProperties": false
                }),
            }
        }

        async fn execute(&self, _context: ToolContext, _arguments: Value) -> Result<RawToolOutput> {
            Ok(RawToolOutput {
                content: b"upstream failed".to_vec(),
                source_path: None,
                media_type: "text/plain; charset=utf-8".into(),
                is_error: true,
                attach_to_model: false,
            })
        }
    }

    fn context(workspace: &Path) -> ToolContext {
        ToolContext {
            run_id: "run-1".into(),
            call_id: "call-1".into(),
            workspace: workspace.into(),
        }
    }

    fn command_tool(tools: Vec<Arc<dyn Tool>>) -> FiascoTool {
        let mut registry = ToolRegistry::default();
        for tool in tools {
            registry.register(tool).unwrap();
        }
        FiascoTool::new(registry, Arc::new(WriteTool::default())).unwrap()
    }

    #[tokio::test]
    async fn pipeline_automatically_fills_one_missing_required_argument() {
        let tool = command_tool(vec![
            Arc::new(LoadTool),
            Arc::new(ArgumentsTool {
                name: "web_search",
                schema: json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "count": {"type": "integer"}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            }),
        ]);
        let workspace = TempDir::new().unwrap();
        let output = tool
            .execute(
                context(workspace.path()),
                json!({"command": "skill load 'pipeline value' | web search count=3"}),
            )
            .await
            .unwrap();

        assert_eq!(
            serde_json::from_slice::<Value>(&output.content).unwrap(),
            json!({"query": "pipeline value", "count": 3})
        );
    }

    #[tokio::test]
    async fn named_stdin_placeholder_compiles_json_values() {
        let tool = command_tool(vec![Arc::new(ArgumentsTool {
            name: "wait",
            schema: json!({
                "type": "object",
                "properties": {"handles": {"type": "array"}},
                "additionalProperties": false
            }),
        })]);
        let workspace = TempDir::new().unwrap();
        let output = tool
            .execute(
                context(workspace.path()),
                json!({
                    "command": "agent wait handles=-",
                    "stdin": "[\"a\",\"b\"]"
                }),
            )
            .await
            .unwrap();

        assert_eq!(
            serde_json::from_slice::<Value>(&output.content).unwrap(),
            json!({"handles": ["a", "b"]})
        );
    }

    #[tokio::test]
    async fn mcp_stage_substitutes_pipeline_input_without_losing_word_boundaries() {
        let tool = command_tool(vec![
            Arc::new(LoadTool),
            Arc::new(ArgumentsTool {
                name: "mcp",
                schema: json!({
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": ["command"],
                    "additionalProperties": false
                }),
            }),
        ]);
        let workspace = TempDir::new().unwrap();
        let output = tool
            .execute(
                context(workspace.path()),
                json!({
                    "command": "skill load 'hello world' | mcp notes save body=-"
                }),
            )
            .await
            .unwrap();
        let arguments: Value = serde_json::from_slice(&output.content).unwrap();
        assert_eq!(
            shell_words::split(arguments["command"].as_str().unwrap()).unwrap(),
            ["notes", "save", "body=hello world"]
        );
    }

    #[tokio::test]
    async fn redirect_atomically_writes_the_successful_final_output() {
        let tool = command_tool(vec![Arc::new(LoadTool)]);
        let workspace = TempDir::new().unwrap();
        let output = tool
            .execute(
                context(workspace.path()),
                json!({"command": "skill load 'complete output' > generated/result.txt"}),
            )
            .await
            .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(workspace.path().join("generated/result.txt"))
                .await
                .unwrap(),
            "complete output"
        );
        assert_eq!(
            String::from_utf8(output.content).unwrap(),
            "Wrote 15 bytes to generated/result.txt"
        );
    }

    #[tokio::test]
    async fn pipeline_stops_on_error_and_does_not_apply_redirect() {
        let tool = command_tool(vec![Arc::new(ErrorTool), Arc::new(LoadTool)]);
        let workspace = TempDir::new().unwrap();
        let output = tool
            .execute(
                context(workspace.path()),
                json!({
                    "command": "web search failure | skill load - > should-not-exist.txt"
                }),
            )
            .await
            .unwrap();

        assert!(output.is_error);
        assert_eq!(output.content, b"upstream failed");
        assert!(
            !tokio::fs::try_exists(workspace.path().join("should-not-exist.txt"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn help_and_schema_list_only_enabled_routes() {
        let tool = command_tool(vec![Arc::new(LoadTool)]);
        let workspace = TempDir::new().unwrap();
        let help = tool
            .execute(
                context(workspace.path()),
                json!({"command": "help skill load"}),
            )
            .await
            .unwrap();
        let help = String::from_utf8(help.content).unwrap();
        assert!(help.contains("Usage: skill load <name>"));
        assert!(help.contains("\"required\""));

        let description = tool.spec().description;
        assert!(description.contains("skill load <name>"));
        assert!(!description.contains("web search"));
        assert!(!description.contains("agent start"));
    }

    #[tokio::test]
    async fn rejects_ambiguous_or_unused_pipeline_input() {
        let tool = command_tool(vec![Arc::new(ArgumentsTool {
            name: "send_message",
            schema: json!({
                "type": "object",
                "properties": {
                    "handle": {"type": "string"},
                    "message": {"type": "string"},
                    "mode": {"type": "string"}
                },
                "required": ["handle", "message", "mode"],
                "additionalProperties": false
            }),
        })]);
        let workspace = TempDir::new().unwrap();

        let error = tool
            .execute(
                context(workspace.path()),
                json!({
                    "command": "agent send handle=h mode=steer message=explicit",
                    "stdin": "unused"
                }),
            )
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("does not unambiguously consume"),
            "{error:#}"
        );
    }
}
