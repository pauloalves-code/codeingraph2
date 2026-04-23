# codeingraph2

Docker-isolated daemon that indexes a codebase into a SQLite graph and exposes it over MCP so Claude Code can pull **surgical context** — only the exact snippets that matter before any refactor.

[Leia em Português](README.md)

## What it produces

| Output | Description |
|---|---|
| MCP server (`mcp_server`) | 6 tools for token-efficient code navigation |
| Web viewer | Cytoscape graph on a user-chosen port (Basic Auth) |
| `CLAUDE.md` | Auto-injected into the target project to guide Claude |
| Obsidian vault | Optional colored graph view (war map) |

## Quick install

```bash
cd /docker/codeingraph2
./install_global.sh --target /path/to/your/repo --name myproject --port 7890
```

Non-interactive (for scripts):

```bash
./install_global.sh \
  --target /path/to/repo \
  --name myproject \
  --port 7890 --user admin --pass 'secret!' \
  --non-interactive
```

After install: restart Claude Desktop — the `codeingraph2-myproject` MCP tool appears automatically.

## install_global.sh flags

| Flag | Default | Description |
|---|---|---|
| `--target PATH` | `./target_code` | Directory to index |
| `--name NAME` | basename of `--target` | Instance name (used for container, volume, MCP entry) |
| `--port N` | prompt | Web UI port |
| `--user NAME` | prompt | Web UI username |
| `--pass SECRET` | prompt | Web UI password (≥6 chars) |
| `--no-web` | — | Disable web UI |
| `--no-vault` | — | Disable Obsidian vault generation |
| `--no-build` | — | Skip `docker compose build` |
| `--no-start` | — | Skip `docker compose up` |
| `--non-interactive` | — | Never prompt (requires `--pass`) |
| `--uninstall` | — | Stop container and remove MCP entry |

## Daemon subcommands

```bash
docker exec myproject_container codeingraph2 daemon     # watcher + indexer + web UI
docker exec myproject_container codeingraph2 index      # one-shot full reindex
docker exec myproject_container codeingraph2 vault      # regenerate Obsidian vault
docker exec myproject_container codeingraph2 claudemd   # regenerate CLAUDE.md
docker exec myproject_container codeingraph2 web        # web UI only
docker exec myproject_container codeingraph2 stats      # JSON graph stats
docker exec myproject_container codeingraph2 health     # exit 0 if healthy
```

## MCP tools

| Tool | Description |
|---|---|
| `get_surgical_context` | Exact code snippets impacted by a symbol (BFS up to `depth`) |
| `query_graph` | Search by name / kind / file — returns file + line numbers |
| `get_symbol` | Full metadata: signature, file, exact lines, docstring |
| `get_callers` | Who calls symbol X (transitive, up to depth N) |
| `get_callees` | What symbol X calls |
| `graph_stats` | Global counts (files, symbols, relations) |

## Supported languages

Rust, Python, JavaScript, TypeScript.

To add a language: add an entry in [`daemon/src/indexer/languages.rs`](daemon/src/indexer/languages.rs) with the tree-sitter `Language` and `symbol_nodes` / `relation_nodes` mappings.

## Repository structure

```
.
├── Dockerfile                    # multi-stage Rust build
├── docker-compose.yml
├── install_global.sh             # global MCP installer
├── templates/CLAUDE.md.tmpl      # template injected into the target project
└── daemon/src/
    ├── main.rs                   # CLI entry point (clap subcommands)
    ├── bin/mcp_server.rs         # MCP stdio server (JSON-RPC)
    ├── config.rs                 # env-based config
    ├── db.rs                     # SQLite pool + migrations
    ├── indexer/                  # walker + tree-sitter per-language specs
    ├── watcher.rs                # inotify + debounce
    ├── impact.rs                 # fan-in / fan-out / centrality scores
    ├── obsidian/mod.rs           # vault generator
    ├── claudemd/mod.rs           # CLAUDE.md renderer (idempotent)
    └── web/                      # axum HTTP server + Basic Auth
```

## Status

- [x] Multi-stage Docker build
- [x] SQLite schema + migrations
- [x] Watcher with debounce + stale-file purge
- [x] tree-sitter parser (4 languages — symbols, relations, lines)
- [x] Impact scores (fan-in / fan-out / centrality)
- [x] Obsidian vault with custom colors (optional)
- [x] Idempotent CLAUDE.md template
- [x] MCP stdio server (6 tools)
- [x] Multi-project installer
