# codeingraph2

Docker-isolated daemon that indexes a codebase into a SQLite graph and exposes it over MCP so Claude Code can pull **surgical context** — only the exact snippets that matter — before any refactor.

[Leia em Português](README.md)

## Web UI

![codeingraph2 web viewer](docs/ui-screenshot.png)

> Interactive force-directed graph with auto zoom-to-fit, multi-select kind highlight, live search dropdown, and a side panel showing source code when clicking any node or edge.

## What it produces

| Output | Description |
|---|---|
| Global MCP server (`mcp_server`) | 8 tools for token-efficient code navigation |
| Web viewer | Cytoscape graph on a user-chosen port (Basic Auth) |
| `CLAUDE.md` | Auto-injected into the target project to guide Claude |
| Obsidian vault | Optional colored graph view (war map) |

## Multi-project

A single global MCP entry (`codeingraph2`) covers all installed projects. Each project has its own independent daemon container. The `"project"` parameter routes calls to the correct database:

```json
{ "tool": "graph_stats", "arguments": { "project": "my-project" } }
```

Use `list_projects` to discover available projects.

## Quick install

```bash
cd /path/to/codeingraph2
./install_global.sh --target /path/to/your/repo --name my-project --port 7890
```

The script:
1. Creates `projects/my-project/` with the container `.env`
2. Builds the image and starts the daemon (`my-project_container`)
3. Starts (or reuses) the persistent global MCP container `codeingraph2_mcp`
4. Registers the `codeingraph2` entry in `~/.mcp.json` and `~/.claude.json`

Installing a second project does not create a new MCP entry — it only updates `registry.json`:

```bash
./install_global.sh --target /other/repo --name other-project --port 7891
```

Non-interactive (for scripts):

```bash
./install_global.sh \
  --target /path/to/repo --name my-project \
  --port 7890 --user admin --pass 'secret!' \
  --non-interactive
```

## Directory structure

```
codeingraph2/
├── docker-compose.yml        # per-project daemon
├── mcp-compose.yml           # persistent global MCP container
├── install_global.sh         # project installer / manager
├── registry.json             # auto-generated (not committed)
├── daemon/src/               # Rust source
│
└── projects/                 # per-project data (not committed)
    └── my-project/
        ├── .env
        ├── graph.db
        └── obsidian_vault/
```

`projects/` and `registry.json` are in `.gitignore`.

## install_global.sh flags

| Flag | Default | Description |
|---|---|---|
| `--target PATH` | `./target_code` | Directory to index |
| `--name NAME` | basename of `--target` | Instance name |
| `--port N` | prompt | Web UI port |
| `--user NAME` | prompt | Web UI username |
| `--pass SECRET` | prompt | Web UI password (≥6 chars) |
| `--no-web` | — | Disable web UI |
| `--no-vault` | — | Disable Obsidian vault generation |
| `--no-build` | — | Skip `docker compose build` |
| `--no-start` | — | Skip `docker compose up` |
| `--non-interactive` | — | Never prompt (requires `--pass`) |
| `--uninstall` | — | Stop container and remove from registry |

## Daemon subcommands

```bash
docker exec my-project_container codeingraph2 daemon     # watcher + indexer + web UI
docker exec my-project_container codeingraph2 index      # one-shot full reindex
docker exec my-project_container codeingraph2 vault      # regenerate Obsidian vault
docker exec my-project_container codeingraph2 claudemd   # regenerate CLAUDE.md
docker exec my-project_container codeingraph2 web        # web UI only
docker exec my-project_container codeingraph2 stats      # JSON graph stats
docker exec my-project_container codeingraph2 health     # exit 0 if healthy
```

## MCP tools

All tools accept an optional `"project"` parameter.

| Tool | Description |
|---|---|
| `list_projects` | List all registered projects |
| `get_surgical_context` | Exact code snippets impacted by a symbol, with `source` included |
| `patch_symbol` | Edit a symbol by name — no `Read`, no `old_string` needed |
| `query_graph` | Search by name / kind / file — returns file + line numbers |
| `get_symbol` | Full metadata: signature, file, exact lines, docstring |
| `get_callers` | Who calls symbol X (transitive, up to depth N) |
| `get_callees` | What symbol X calls |
| `graph_stats` | Global counts (files, symbols, relations) |

## Recommended Claude workflow

```jsonc
// 1. Before refactoring: get surgical context
{ "tool": "get_surgical_context", "arguments": { "symbol": "my_function", "depth": 1 } }
// → returns full source of the symbol and all its callers

// 2. Edit directly by name — no old_string needed
{ "tool": "patch_symbol", "arguments": { "symbol": "my_function", "new_source": "..." } }
// The daemon reindexes automatically on the next watcher tick
```

## Supported languages

Rust, Python, JavaScript, TypeScript.

To add a language: add an entry in [`daemon/src/indexer/languages.rs`](daemon/src/indexer/languages.rs) with the tree-sitter `Language` and `symbol_nodes` / `relation_nodes` mappings.

## Repository structure

```
.
├── Dockerfile                    # multi-stage Rust build
├── docker-compose.yml            # per-project daemon
├── mcp-compose.yml               # persistent global MCP container
├── install_global.sh             # project installer / manager
├── Architecture.md               # detailed architecture documentation
├── templates/CLAUDE.md.tmpl      # template injected into the target project
└── daemon/src/
    ├── main.rs                   # CLI entry point (clap subcommands)
    ├── bin/mcp_server.rs         # MCP stdio server (JSON-RPC 2.0)
    ├── config.rs                 # env-based configuration
    ├── db.rs                     # SQLite pool + migrations
    ├── indexer/                  # walker + tree-sitter per-language specs
    ├── watcher.rs                # inotify + debounce + loop suppression
    ├── impact.rs                 # fan-in / fan-out / centrality scores
    ├── obsidian/mod.rs           # vault generator (optional)
    ├── claudemd/mod.rs           # CLAUDE.md renderer (idempotent)
    └── web/                      # axum HTTP server + Basic Auth
```

## Graph color palette

| Type | Color |
|---|---|
| Files / Modules | `#999999` gray |
| Classes / Traits / Enums | `#ff4d4d` red |
| Functions / Methods | `#4d94ff` blue |
| Variables / Constants | `#ffdb4d` yellow |
