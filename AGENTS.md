# AGENTS.md — frontmatter-mcp coding harness

Guidelines for any LLM agent working in this repository.

## Project layout

```
src/
  lib.rs    — all core logic (split, join, YAML round-trip, type inference, public API)
  main.rs   — CLI dispatch + MCP ServerHandler (3 tools: read_meta, write_meta, bump_version)
Cargo.toml  — single package, one lib + one binary named `frontmatter`
justfile    — build, test, install recipes
```

The package name is `frontmatter-mcp`; the lib crate name is `frontmatter_mcp`
(Cargo normalises the hyphen). `main.rs` imports it as `use frontmatter_mcp as fm;`.
The compiled binary is named `frontmatter`. Installing copies it into
`~/.local/bin/` (or `/usr/local/bin/` with `just install --system`) and, if
absent, creates a relative `frontmatter-mcp` symlink beside it so the binary
can be reached in MCP mode by name.

## Build & test

```bash
just build    # cargo build --release → target/release/frontmatter
just test     # cargo test (43 tests, must stay green)
just lint     # cargo clippy -- -D warnings
just compress # build + upx-pack target/release/frontmatter in place
just install  # compress + copy frontmatter into ~/.local/bin, link frontmatter-mcp
just install --system   # same, into /usr/local/bin via sudo
```

`just compress` requires `upx` on PATH. It's idempotent: `upx -t` checks
whether the binary is already packed before invoking `upx --best --lzma`,
so re-running `just install` without a source change doesn't error.

Always run `just test` after any change and confirm all 43 tests pass
before considering a task done.

## Core invariants — never break these

1. **Body is sacred.** The content after the closing `---` delimiter must
   be returned byte-for-byte unchanged by every write operation.
   `tests::chapter_body_untouched` and `tests::join_round_trip` guard this.

2. **Key order is preserved.** Updating an existing frontmatter key must
   not change its position in the YAML block. New keys are appended at
   the end. Five tests in the `key order preservation` section guard this.

3. **Type round-trip.** Integer YAML values must come back as JSON integers,
   not strings. `"1.1.7"` (semver) must remain a string, not be parsed as
   a float. The `detect_*` and `chapter_*` test group guards this.

4. **MCP stdout is clean.** All logging goes to stderr via `tracing`.
   Nothing must be written to stdout except the MCP JSON-RPC messages.
   `serve_mcp` now routes stdin/stdout through an in-memory-pipe shim, so a
   *single* task owns the real stdout — injected replies and rmcp's output
   must never interleave. Anything else writing to stdout breaks this.

## Antigravity (AGY CLI) compatibility shim

Google's Antigravity CLI violates the MCP lifecycle: it sends a proprietary
`server/discover` request *before* the mandatory `initialize` handshake to
decide whether the executable is one of its bundled plugins. rmcp's strict
state machine treats that stray frame as fatal and drops the connection
(`expect initialized request, but received: ...server/discover...`), so the
server never starts. Node-based servers survive only because they reply
`-32601 Method not found` and keep the pipe open.

`serve_mcp` reproduces that lenient behaviour: it interposes a filter between
the real stdio and rmcp (fed via `tokio::io::duplex` pipes). `intercept_probe`
answers any method in `AGY_PROBE_METHODS` with `-32601` and swallows the frame;
everything else — including the real `initialize` — passes through byte-for-byte.
If AGY adds more pre-init probe methods, extend `AGY_PROBE_METHODS`. Guarded by
the `tests` module in `main.rs`. **Do not** hand raw stdio straight to
`rmcp::serve_server` again, or AGY breaks.

## Key functions in `lib.rs`

| Function | What it does |
|---|---|
| `split(text)` | Returns `(Option<fm_text>, body)` — body never touches a parser |
| `join(fm, body)` | Reassembles the file with `---` delimiters |
| `read_meta(path, key)` | Returns plain JSON value(s) |
| `read_meta_typed(path, key)` | Returns `{value, type, [items_type]}` per field |
| `write_meta(path, updates, touch_updated)` | Patches keys in-place |
| `bump_version(path, level)` | Bumps semver `version` field |
| `infer_type(v)` | Returns `(type_name, Option<items_type>)` for a YAML value |
| `path_of(s)` | Expands `~/` to `$HOME/` |

## MCP tools (in `main.rs`)

| Tool | Required args | Optional args |
|---|---|---|
| `read_meta` | `path` | `key`, `typed` |
| `write_meta` | `path`, `updates` | `touch_updated` |
| `bump_version` | `path` | `level` |

Adding a new MCP tool requires changes in three places:
1. `tool_list()` — add a `Tool::new(...)` entry with schema
2. `dispatch()` — add a match arm
3. `lib.rs` — add the backing function and its tests

## Type system (`infer_type`)

Scalar types detected from string values:

| Type | Detection rule |
|---|---|
| `wikilink` | starts with `[[` and ends with `]]` (len > 4) |
| `datetime` | `YYYY-MM-DD HH:MM:SS` or `YYYY-MM-DDTHH:MM:SS` (≥19 chars) |
| `date` | exactly `YYYY-MM-DD` (10 chars) |
| `string` | anything else |

Array `items_type` is the common element type, or `"mixed"` if heterogeneous.
Detection priority within arrays: `wikilink > datetime > date > string > integer > float > boolean > mixed`.

## YAML serialisation notes

- `serde_yaml 0.9` uses `IndexMap`-backed `Mapping` — insertion order is
  preserved and `insert` on an existing key updates in-place without moving it.
- `serde_yaml` follows YAML 1.2: datetime strings like `2025-03-16 08:30:00`
  are serialised as plain unquoted scalars (not tagged timestamps).
- Wikilinks (`[[...]]`) may be single-quoted in the output (`'[[...]]'`) — this
  is cosmetically different from double-quoted input but semantically identical.
- List items are serialised as `- item` (no indent) rather than `  - item`.
  Both are valid YAML; Obsidian accepts either.

## Dependency policy

Keep the dependency footprint minimal. Current direct deps:

```
rmcp          MCP protocol (server + stdio transport)
tokio         async runtime (MCP server needs it; CLI pays the startup cost)
serde_json    JSON for CLI output and MCP wire format
serde_yaml    YAML round-trip
anyhow        error propagation
chrono        local wall-clock time for `updated` timestamps
tracing       structured logging (stderr only)
tracing-subscriber  log formatting
```

`tempfile` is a dev-dependency (tests only). Do not add runtime dependencies
without a compelling reason.

## Binary detection logic

`main()` enters MCP server mode when:
- The binary's file stem ends with `-mcp` (e.g. if manually renamed or
  symlinked to `frontmatter-mcp`), **or**
- The first CLI argument is `serve`.

Everything else is CLI mode. `just install` creates a relative `frontmatter-mcp`
symlink beside the binary, so MCP mode can be reached either by that name or via
the explicit subcommand:

```bash
just install                 # installs frontmatter + frontmatter-mcp symlink
frontmatter-mcp              # MCP server mode via the symlinked name
frontmatter serve            # MCP server mode via explicit subcommand
frontmatter read ~/notes/foo.md   # CLI mode
```
