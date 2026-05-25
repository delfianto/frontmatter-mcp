//! Single binary: runs as CLI when invoked as `front`, and as an MCP
//! server when invoked as `front-mcp` (symlink) or via `front serve`.

#![allow(clippy::field_reassign_with_default)]

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
};
use serde_json::{Value, json};
use tracing::info;

use frontmatter_mcp as fm;

// ── MCP handler ───────────────────────────────────────────────────────────────

struct Handler;

impl ServerHandler for Handler {
    fn get_info(&self) -> ServerInfo {
        let mut caps = ServerCapabilities::default();
        caps.tools = Some(Default::default());

        let mut impl_info = Implementation::default();
        impl_info.name = "front-mcp".to_owned();
        impl_info.version = env!("CARGO_PKG_VERSION").to_owned();

        let mut info = ServerInfo::default();
        info.protocol_version = rmcp::model::ProtocolVersion::LATEST;
        info.capabilities = caps;
        info.server_info = impl_info;
        info.instructions = Some(
            "Read and patch YAML frontmatter in Markdown/Obsidian files. \
             The file body is never parsed or modified — only the YAML block \
             between the --- delimiters is touched."
                .to_owned(),
        );
        info
    }

    async fn list_tools(
        &self,
        _req: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(tool_list())
    }

    async fn call_tool(
        &self,
        req: CallToolRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        dispatch(req)
    }
}

// ── Tool registry ─────────────────────────────────────────────────────────────

fn schema(props: Value, required: &[&str]) -> Arc<rmcp::model::JsonObject> {
    Arc::new(rmcp::model::object(json!({
        "type": "object",
        "properties": props,
        "required": required,
    })))
}

fn tool_list() -> ListToolsResult {
    let mut r = ListToolsResult::default();
    r.tools = vec![
        Tool::new(
            "read_meta",
            "Read YAML frontmatter from a Markdown file. \
             Returns the full metadata object when key is omitted, or the value of a single key. \
             Returns null for missing keys. \
             When typed=true each value is wrapped as \
             {\"value\": ..., \"type\": \"<type>\", [\"items_type\": \"<type>\"]} \
             where type is one of: null, boolean, integer, float, \
             date (YYYY-MM-DD), datetime (YYYY-MM-DD HH:MM:SS), \
             wikilink ([[...]]), string, array, object — \
             and items_type describes the common element type of arrays.",
            schema(
                json!({
                    "path":  { "type": "string",  "description": "Absolute or ~-prefixed path to the file" },
                    "key":   { "type": "string",  "description": "Optional single key to read" },
                    "typed": { "type": "boolean", "description": "Annotate each value with its detected type (default: false)" }
                }),
                &["path"],
            ),
        ),
        Tool::new(
            "write_meta",
            "Patch YAML frontmatter keys in a Markdown file. \
             Only the keys present in `updates` are changed; all other keys and \
             the entire file body are left untouched. \
             Auto-sets the `updated` timestamp unless touch_updated is false.",
            schema(
                json!({
                    "path":          { "type": "string",  "description": "Absolute or ~-prefixed path to the file" },
                    "updates":       { "type": "object",  "description": "Key/value pairs to patch into the frontmatter" },
                    "touch_updated": { "type": "boolean", "description": "Auto-set the `updated` timestamp (default: true)" }
                }),
                &["path", "updates"],
            ),
        ),
        Tool::new(
            "bump_version",
            "Bump the semver `version` field in a Markdown file's frontmatter. \
             Initialises to 0.1.0 if the field is absent. \
             Also sets `updated` to the current local timestamp. \
             Returns {\"version\": \"<new_version>\"}.",
            schema(
                json!({
                    "path":  { "type": "string", "description": "Absolute or ~-prefixed path to the file" },
                    "level": {
                        "type": "string",
                        "enum": ["major", "minor", "patch"],
                        "description": "Version component to bump (default: patch)"
                    }
                }),
                &["path"],
            ),
        ),
    ];
    r
}

// ── Tool dispatch ─────────────────────────────────────────────────────────────

fn arg_str<'a>(args: &'a rmcp::model::JsonObject, key: &str) -> Option<&'a str> {
    args.get(key)?.as_str()
}

fn arg_bool(args: &rmcp::model::JsonObject, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn ok_json(v: impl serde::Serialize) -> Result<CallToolResult, McpError> {
    serde_json::to_string_pretty(&v)
        .map(|s| CallToolResult::success(vec![Content::text(s)]))
        .map_err(|e| McpError::internal_error(e.to_string(), None))
}

fn err_text(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.into())])
}

fn dispatch(req: CallToolRequestParams) -> Result<CallToolResult, McpError> {
    let args = req.arguments.unwrap_or_default();

    match req.name.as_ref() {
        "read_meta" => {
            let Some(p) = arg_str(&args, "path") else {
                return Ok(err_text("missing required argument 'path'"));
            };
            let key = arg_str(&args, "key");
            let typed = arg_bool(&args, "typed", false);
            let path = fm::path_of(p);
            let result = if typed {
                fm::read_meta_typed(&path, key)
            } else {
                fm::read_meta(&path, key)
            };
            match result {
                Ok(v) => ok_json(v),
                Err(e) => Ok(err_text(e.to_string())),
            }
        }

        "write_meta" => {
            let Some(p) = arg_str(&args, "path") else {
                return Ok(err_text("missing required argument 'path'"));
            };
            let touch = arg_bool(&args, "touch_updated", true);
            let Some(updates) = args.get("updates").and_then(Value::as_object) else {
                return Ok(err_text(
                    "missing or invalid argument 'updates' (must be an object)",
                ));
            };
            match fm::write_meta(&fm::path_of(p), updates.clone(), touch) {
                Ok(()) => ok_json(json!({"ok": true})),
                Err(e) => Ok(err_text(e.to_string())),
            }
        }

        "bump_version" => {
            let Some(p) = arg_str(&args, "path") else {
                return Ok(err_text("missing required argument 'path'"));
            };
            let level = arg_str(&args, "level").unwrap_or("patch");
            match fm::bump_version(&fm::path_of(p), level) {
                Ok(v) => ok_json(json!({"version": v})),
                Err(e) => Ok(err_text(e.to_string())),
            }
        }

        other => Err(McpError::invalid_params(
            format!("unknown tool '{other}'"),
            None,
        )),
    }
}

// ── MCP server entry ──────────────────────────────────────────────────────────

async fn serve_mcp() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "frontmatter_mcp=info".parse().unwrap()),
        )
        .init();

    info!("front-mcp starting");

    let (stdin, stdout) = rmcp::transport::io::stdio();
    let srv = rmcp::serve_server(Handler, (stdin, stdout))
        .await
        .context("failed to start MCP server")?;

    srv.waiting().await.map(|_| ()).context("MCP server error")
}

// ── CLI entry ─────────────────────────────────────────────────────────────────

const USAGE: &str = "\
front — surgical YAML frontmatter patcher for Markdown/Obsidian files

USAGE:
  front read  <path> [--key KEY]
  front write <path> <json_object> [--no-touch-updated]
  front bump  <path> [--level major|minor|patch]
  front serve                         (run as MCP server on stdio)

Symlink or rename the binary to `front-mcp` to start directly in MCP mode.
All output is JSON on stdout. Non-zero exit on error.";

fn flag_val<'a>(args: &'a [String], flag: &str, short: &str) -> Option<&'a str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag || a == short {
            return it.next().map(String::as_str);
        }
    }
    None
}

fn run_cli(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        None | Some("--help") | Some("-h") => {
            eprintln!("{USAGE}");
            return Ok(());
        }

        Some("read") => {
            anyhow::ensure!(args.len() >= 2, "usage: front read <path> [--key KEY]");
            let path = fm::path_of(&args[1]);
            let key = flag_val(&args[2..], "--key", "-k");
            let v = fm::read_meta(&path, key)?;
            println!("{}", serde_json::to_string(&v).unwrap());
        }

        Some("write") => {
            anyhow::ensure!(
                args.len() >= 3,
                "usage: front write <path> <json_object> [--no-touch-updated]"
            );
            let path = fm::path_of(&args[1]);
            let updates: serde_json::Map<String, Value> =
                serde_json::from_str(&args[2]).context("updates must be a JSON object")?;
            let touch = !args.iter().any(|a| a == "--no-touch-updated");
            fm::write_meta(&path, updates, touch)?;
            println!("{{\"ok\":true}}");
        }

        Some("bump") => {
            anyhow::ensure!(
                args.len() >= 2,
                "usage: front bump <path> [--level major|minor|patch]"
            );
            let path = fm::path_of(&args[1]);
            let level = flag_val(&args[2..], "--level", "-l").unwrap_or("patch");
            let new_v = fm::bump_version(&path, level)?;
            println!("{{\"version\":\"{new_v}\"}}");
        }

        Some("serve") => {
            // Reached here if called as `front serve` but tokio isn't running yet.
            // This branch is dead — serve_mcp() is dispatched before run_cli() is called.
            unreachable!()
        }

        Some(other) => anyhow::bail!("unknown subcommand '{other}'\n\n{USAGE}"),
    }
    Ok(())
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let raw: Vec<String> = std::env::args().collect();

    // Detect MCP mode: binary name ends with "-mcp", or first argument is "serve"
    let binary = Path::new(&raw[0])
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    let mcp_mode = binary.ends_with("-mcp") || raw.get(1).map(String::as_str) == Some("serve");

    if mcp_mode {
        serve_mcp().await
    } else {
        run_cli(&raw[1..])
    }
}
