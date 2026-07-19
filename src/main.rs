//! Single binary: runs as CLI when invoked as `frontmatter`, and as an MCP
//! server when invoked with a binary name/symlink ending in `-mcp` or via
//! `frontmatter serve`.

#![allow(clippy::field_reassign_with_default)]

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, Implementation, ListToolsResult,
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
        impl_info.name = "frontmatter-mcp".to_owned();
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
        .map(|s| CallToolResult::success(vec![ContentBlock::text(s)]))
        .map_err(|e| McpError::internal_error(e.to_string(), None))
}

fn err_text(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(msg.into())])
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

// ── Antigravity (AGY CLI) protocol-violation shim ─────────────────────────────

/// Non-MCP JSON-RPC methods that Google's Antigravity ("AGY") CLI illegally
/// sends *before* the mandatory `initialize` handshake. AGY merged its
/// proprietary "plugin" discovery into standard MCP, so it probes every server
/// with `server/discover` first to decide whether the executable is an AGY
/// plugin. The MCP spec requires `initialize` to be the very first request;
/// rmcp's strict state machine treats this stray frame as fatal and drops the
/// connection (`expect initialized request, but received: ...server/discover...`).
///
/// Node-based servers survive only because they answer the unknown method with
/// a JSON-RPC `-32601` error and keep the pipe open, after which AGY proceeds
/// to a normal `initialize`. `intercept_probe` reproduces exactly that.
/// Extend this list if AGY starts sending further pre-init probe methods.
const AGY_PROBE_METHODS: &[&str] = &["server/discover"];

/// If `line` is one of AGY's illegal pre-init probe *requests*, return the
/// newline-terminated `-32601 Method not found` reply to send back, so the
/// frame can be swallowed instead of forwarded to rmcp. Any other frame —
/// including a real `initialize` — returns `None` and passes through untouched.
fn intercept_probe(line: &[u8]) -> Option<Vec<u8>> {
    let msg: Value = serde_json::from_slice(line).ok()?;
    let method = msg.get("method")?.as_str()?;
    if !AGY_PROBE_METHODS.contains(&method) {
        return None;
    }
    // Only requests (those carrying an `id`) expect a reply; echo the id back.
    let id = msg.get("id")?;
    let reply = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32601, "message": "Method not found" },
    });
    let mut bytes = serde_json::to_vec(&reply).ok()?;
    bytes.push(b'\n');
    Some(bytes)
}

// ── MCP server entry ──────────────────────────────────────────────────────────

async fn serve_mcp() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "frontmatter=info,frontmatter_mcp=info".parse().unwrap()),
        )
        .init();

    info!("frontmatter-mcp starting");

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    // rmcp enforces the MCP lifecycle strictly, so we can't hand it raw stdio:
    // AGY's pre-init `server/discover` probe would kill it before the handshake
    // (see `intercept_probe`). Instead we sit between the client and rmcp on a
    // pair of in-memory pipes and filter the client's input stream.
    const PIPE_BUF: usize = 64 * 1024;
    let (mut to_server, from_client) = tokio::io::duplex(PIPE_BUF); // filter → rmcp stdin
    let (to_client, mut from_server) = tokio::io::duplex(PIPE_BUF); // rmcp stdout → forwarder

    // A single task owns the real stdout so injected replies and rmcp's own
    // output can never interleave (invariant: stdout carries only JSON-RPC).
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(frame) = out_rx.recv().await {
            if stdout.write_all(&frame).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    });

    // Pump rmcp's output to the real stdout, byte-for-byte.
    {
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            loop {
                match from_server.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }

    // Filter the client's input: answer AGY's illegal `server/discover` probe
    // ourselves and drop it; forward every other frame to rmcp unchanged.
    tokio::spawn(async move {
        let mut reader = BufReader::new(tokio::io::stdin());
        let mut line: Vec<u8> = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) | Err(_) => break, // EOF: dropping `to_server` shuts rmcp down
                Ok(_) => {
                    if let Some(reply) = intercept_probe(&line) {
                        info!("swallowed non-MCP `server/discover` probe (Antigravity CLI)");
                        if out_tx.send(reply).is_err() {
                            break;
                        }
                        continue;
                    }
                    if to_server.write_all(&line).await.is_err() {
                        break;
                    }
                    let _ = to_server.flush().await;
                }
            }
        }
    });

    let srv = rmcp::serve_server(Handler, (from_client, to_client))
        .await
        .context("failed to start MCP server")?;

    srv.waiting().await.map(|_| ()).context("MCP server error")
}

// ── CLI entry ─────────────────────────────────────────────────────────────────

const USAGE: &str = "\
frontmatter — surgical YAML frontmatter patcher for Markdown/Obsidian files

USAGE:
  frontmatter read  <path> [--key KEY]
  frontmatter write <path> <json_object> [--no-touch-updated]
  frontmatter bump  <path> [--level major|minor|patch]
  frontmatter serve                         (run as MCP server on stdio)

Symlink or rename the binary to end in `-mcp` to start directly in MCP mode.
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
            anyhow::ensure!(
                args.len() >= 2,
                "usage: frontmatter read <path> [--key KEY]"
            );
            let path = fm::path_of(&args[1]);
            let key = flag_val(&args[2..], "--key", "-k");
            let v = fm::read_meta(&path, key)?;
            println!("{}", serde_json::to_string(&v).unwrap());
        }

        Some("write") => {
            anyhow::ensure!(
                args.len() >= 3,
                "usage: frontmatter write <path> <json_object> [--no-touch-updated]"
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
                "usage: frontmatter bump <path> [--level major|minor|patch]"
            );
            let path = fm::path_of(&args[1]);
            let level = flag_val(&args[2..], "--level", "-l").unwrap_or("patch");
            let new_v = fm::bump_version(&path, level)?;
            println!("{{\"version\":\"{new_v}\"}}");
        }

        Some("serve") => {
            // Reached here if called as `frontmatter serve` but tokio isn't running yet.
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

// ── Tests: Antigravity `server/discover` probe interception ───────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intercepts_agy_server_discover_probe() {
        let line = br#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{}}"#;
        let reply = intercept_probe(line).expect("server/discover must be intercepted");
        assert!(reply.ends_with(b"\n"), "reply must be newline-framed");
        let v: Value = serde_json::from_slice(&reply).unwrap();
        assert_eq!(v["jsonrpc"], json!("2.0"));
        assert_eq!(v["id"], json!(1), "must echo the request id back");
        assert_eq!(v["error"]["code"], json!(-32601), "must be Method not found");
    }

    #[test]
    fn intercept_echoes_string_id() {
        let line = br#"{"jsonrpc":"2.0","id":"probe-7","method":"server/discover"}"#;
        let reply = intercept_probe(line).unwrap();
        let v: Value = serde_json::from_slice(&reply).unwrap();
        assert_eq!(v["id"], json!("probe-7"));
    }

    #[test]
    fn initialize_passes_through() {
        let line = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        assert!(
            intercept_probe(line).is_none(),
            "initialize must never be swallowed"
        );
    }

    #[test]
    fn normal_mcp_traffic_passes_through() {
        assert!(intercept_probe(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        assert!(intercept_probe(br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).is_none());
        assert!(intercept_probe(br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{}}"#).is_none());
    }

    #[test]
    fn non_json_and_empty_lines_pass_through() {
        assert!(intercept_probe(b"this is not json\n").is_none());
        assert!(intercept_probe(b"\n").is_none());
        assert!(intercept_probe(b"").is_none());
    }
}
