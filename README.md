# frontmatter-mcp

Surgical YAML frontmatter patcher for Markdown and Obsidian files.  
Single Rust binary — works as a CLI tool (`front`) and as an MCP server (`front-mcp`) on stdio.

The file body is **never parsed or touched** — only the YAML block between
the `---` delimiters is read or modified. Key insertion order is preserved
on every write.

## Install

```bash
just install
```

This builds a release binary and drops two files into `~/.local/bin`:

```
~/.local/bin/front        ← CLI binary
~/.local/bin/front-mcp    ← symlink → front  (MCP server mode)
```

Manual alternative:

```bash
cargo build --release
cp target/release/front ~/.local/bin/front
ln -s front ~/.local/bin/front-mcp
```

## CLI

```
front read  <path> [--key KEY]
front write <path> <json_object> [--no-touch-updated]
front bump  <path> [--level major|minor|patch]
front serve # start MCP server on stdio
```

All output is JSON on stdout. Non-zero exit on error.

### Examples

```bash
# Read all frontmatter as a JSON object
front read ~/notes/chapter-01.md

# Read a single key
front read ~/notes/chapter-01.md --key status

# Patch keys — body untouched, updated timestamp auto-set
front write ~/notes/chapter-01.md '{"status": "published", "arc": 2}'

# Patch without touching the updated timestamp
front write ~/notes/chapter-01.md '{"status": "draft"}' --no-touch-updated

# Bump the semver version field
front bump ~/notes/chapter-01.md                # patch: 1.1.7 → 1.1.8
front bump ~/notes/chapter-01.md --level minor  # 1.1.7 → 1.2.0
front bump ~/notes/chapter-01.md --level major  # 1.1.7 → 2.0.0
```

## MCP server

The binary runs as an MCP server on stdio when its name ends with `-mcp`
or when called with the `serve` subcommand.

### Harness config (stdio transport)

```json
{
    "mcpServers": {
        "frontmatter": {
            "command": "front-mcp"
        }
    }
}
```

Or with the explicit subcommand if you prefer a single binary name:

```json
{
    "mcpServers": {
        "frontmatter": {
            "command": "front",
            "args": ["serve"]
        }
    }
}
```

### MCP tools

#### `read_meta`

Read YAML frontmatter from a Markdown file.

| Parameter | Type    | Required | Description                                           |
| --------- | ------- | -------- | ----------------------------------------------------- |
| `path`    | string  | yes      | Absolute or `~`-prefixed path                         |
| `key`     | string  | no       | Single key to read; omit for full object              |
| `typed`   | boolean | no       | Wrap each value with type metadata (default: `false`) |

Returns the full metadata object, a single value, or `null` for a missing key.

When `typed: true` each value is wrapped as:

```json
{ "value": <raw_value>, "type": "<type>", "items_type": "<type>" }
```

`items_type` is only present for `array` values.

**Detected types**

| Type       | Pattern                                                      |
| ---------- | ------------------------------------------------------------ |
| `null`     | YAML null                                                    |
| `boolean`  | `true` / `false`                                             |
| `integer`  | whole number                                                 |
| `float`    | decimal number                                               |
| `date`     | `YYYY-MM-DD`                                                 |
| `datetime` | `YYYY-MM-DD HH:MM:SS` or `YYYY-MM-DDTHH:MM:SS`               |
| `wikilink` | `[[...]]`                                                    |
| `string`   | anything else                                                |
| `array`    | sequence; `items_type` set to common element type or `mixed` |
| `object`   | nested mapping                                               |

#### `write_meta`

Patch YAML frontmatter keys in a Markdown file.

| Parameter       | Type    | Required | Description                                    |
| --------------- | ------- | -------- | ---------------------------------------------- |
| `path`          | string  | yes      | Absolute or `~`-prefixed path                  |
| `updates`       | object  | yes      | Key/value pairs to patch                       |
| `touch_updated` | boolean | no       | Auto-set `updated` timestamp (default: `true`) |

Only the supplied keys change. All other keys and the entire body are
left byte-for-byte unchanged. Key insertion order is preserved — updated
keys stay in their original positions; new keys are appended at the end.

#### `bump_version`

Bump the semver `version` field and set `updated` to the current timestamp.

| Parameter | Type   | Required | Description                                     |
| --------- | ------ | -------- | ----------------------------------------------- |
| `path`    | string | yes      | Absolute or `~`-prefixed path                   |
| `level`   | string | no       | `major`, `minor`, or `patch` (default: `patch`) |

Initialises `version` to `0.1.0` if the field is absent.  
Returns `{"version": "<new_version>"}`.

## Behaviour guarantees

- **Body preservation** — the content after the closing `---` is returned
  byte-for-byte unchanged on every write.
- **Key-order preservation** — updating an existing key never changes its
  position in the YAML block. New keys are appended at the end.
- **CRLF tolerance** — frontmatter with Windows line endings is normalised
  before parsing; the body is left untouched.
- **Auto-timestamp** — `write_meta` and `bump_version` set `updated` to
  local wall-clock time (`YYYY-MM-DD HH:MM:SS`) unless suppressed.

## Architecture

```
src/
  lib.rs    — core: split/join, YAML round-trip, type inference, public API
  main.rs   — CLI arg parsing + MCP server handler (3 tools)
```

One Cargo package, one compiled binary. The MCP handler lives in `main.rs`
alongside the CLI so there is no separate server process to manage.
