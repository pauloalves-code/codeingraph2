# codeingraph2 — Surgical Context & Visual Intelligence

A Docker-isolated daemon that watches a codebase, builds a 3D correlation
matrix (**Symbols × Relations × Lines**) in SQLite, and exposes it over MCP so
Claude Code can pull **surgical context** — only the snippets that matter —
before any refactor.

It also emits:

* an **Obsidian vault** with colored Graph View (the "war map").
* an **embedded web viewer** (Cytoscape) served on a user-chosen port,
  protected by Basic Auth — clique num nó para ver metadados + código fonte;
  clique numa aresta para ver a relação + 5 linhas de contexto.
* a `CLAUDE.md` in the target project that teaches Claude how to use the graph.

## Arquitetura

```
┌──────────────────────────────────────────────────────────────────────┐
│  Host (você)                                                          │
│    /docker/codeingraph2 ─ install_global.sh                           │
│                                                                       │
│  Claude Desktop / Claude Code                                         │
│    └── MCP client ──► docker exec -i codeingraph2_container ./mcp_server
├──────────────────────────────────────────────────────────────────────┤
│  Container: codeingraph2_container (isolado)                          │
│                                                                       │
│   /target_code   ◄── bind mount do repo do usuário                    │
│   /obsidian_vault ◄─ bind mount do vault (war map visual)             │
│                                                                       │
│   ┌─ watcher (inotify, debounce) ─┐                                   │
│   │                                ▼                                   │
│   │              tree-sitter indexer                                   │
│   │                  │                                                 │
│   │                  ▼                                                 │
│   │       SQLite graph.db ◄── 3D matrix: (symbol × relation × line)    │
│   │             │   │                                                  │
│   │             │   └─► obsidian generator ─► /obsidian_vault          │
│   │             │                                                       │
│   │             └───► claudemd renderer ───► /target_code/CLAUDE.md     │
│   │                                                                     │
│   └─ mcp_server (stdio JSON-RPC, read-only) ──► Claude                  │
└──────────────────────────────────────────────────────────────────────┘
```

## Instalação rápida

```bash
cd /docker/codeingraph2
./install_global.sh --target /caminho/do/seu/repo --vault /caminho/do/vault
```

O script pergunta (a menos que você passe `--port / --user / --pass`):
- **Porta** da UI web (default 7890).
- **Usuário** (default `admin`).
- **Senha** (>=6 chars, confirmada; salva como `sha256(salt||pass)` num `.env` com modo 600).

Depois ele:
1. valida o Docker,
2. escreve o `.env` que o `docker-compose` consome,
3. faz `docker compose build` + `up -d`,
4. registra o servidor MCP no `claude_desktop_config.json` do host
   (e também via `claude mcp add-json` se o CLI estiver disponível).

Reinicie o Claude Desktop, acesse `http://localhost:<porta>` para o mapa visual,
e a ferramenta `codeingraph2` aparece no Claude.

### Flags úteis

```bash
./install_global.sh --non-interactive \
  --target /repo --vault /vault \
  --port 8080 --user alice --pass 's3gura!'

./install_global.sh --no-web         # só daemon + MCP, sem UI
./install_global.sh --uninstall      # derruba container + remove MCP
```

## Subcomandos do daemon

```bash
docker exec codeingraph2_container codeingraph2 daemon     # watcher + indexer + UI web (default no compose)
docker exec codeingraph2_container codeingraph2 index      # one-shot full index
docker exec codeingraph2_container codeingraph2 web        # apenas a UI web
docker exec codeingraph2_container codeingraph2 vault      # regenerate vault
docker exec codeingraph2_container codeingraph2 claudemd   # regenerate CLAUDE.md
docker exec codeingraph2_container codeingraph2 stats      # JSON stats
docker exec codeingraph2_container codeingraph2 health     # exit 0 se OK
```

## UI web

Endpoint: `http://<host>:<WEB_PORT>/` (Basic Auth). Usa Cytoscape com layout
`cose` (force-directed) e a mesma paleta do Obsidian.

- Barra superior: busca por nome/qualificador, filtro por tipo, limite de nós, botão recarregar.
- Painel lateral ao clicar em nó: tipo, arquivo, assinatura, docstring, **bloco de código** com highlight por Prism.js, listas de fan-in/fan-out navegáveis.
- Painel lateral ao clicar em aresta: de/para, relation_kind, linha, snippet de contexto (5 linhas).

Endpoints da API (JSON, mesmo Basic Auth):

| Rota                      | Descrição |
|---------------------------|-----------|
| `GET /api/stats`          | totais e breakdown por kind |
| `GET /api/graph?q&kind&limit` | `{nodes, edges}` |
| `GET /api/node/:id`       | metadados + source lines |
| `GET /api/edge/:id`       | metadados + 5 linhas de contexto |
| `GET /api/source?file&start&end` | trecho bruto (validado contra traversal) |

## Ferramentas MCP

| Tool                    | Descrição |
|-------------------------|-----------|
| `get_surgical_context`  | Fatias exatas de código impactadas por um símbolo (BFS até `depth`) |
| `query_graph`           | Busca por nome / tipo / arquivo |
| `get_symbol`            | Metadados completos de um símbolo |
| `get_callers`           | Chain de quem chama um símbolo |
| `get_callees`           | O que um símbolo chama |
| `graph_stats`           | Contagens globais |

## Schema SQLite (3D matrix)

* **X:** `symbols` (classe / função / variável / …)
* **Y:** `relations.relation_kind` (calls / imports / inherits / …)
* **Z:** `relations.line` (número de linha física)

`line_index` materializa a projeção Z: dado `(file_id, line)` responde em O(1)
qual símbolo está lá e quantas relações nascem daquela linha.

`impact_scores` cacheia `fan_in`, `fan_out` e centralidade para priorizar
snippets no `get_surgical_context`.

## Linguagens suportadas (v0)

Rust, Python, JavaScript, TypeScript. Adicionar outra linguagem = adicionar
uma entrada em [`daemon/src/indexer/languages.rs`](daemon/src/indexer/languages.rs)
com a `Language` do tree-sitter e os mapeamentos de `symbol_nodes` /
`relation_nodes`.

## Estrutura do repositório

```
.
├── Dockerfile                    # multi-stage build do binário Rust
├── docker-compose.yml            # serviço + volumes + env
├── install_global.sh             # registra MCP globalmente no Claude
├── templates/
│   └── CLAUDE.md.tmpl            # template injetado no target
├── daemon/
│   ├── Cargo.toml
│   ├── migrations/001_initial.sql
│   ├── static/index.html         # viewer Cytoscape (embutido via include_str!)
│   └── src/
│       ├── lib.rs
│       ├── main.rs               # bin: codeingraph2
│       ├── bin/mcp_server.rs     # bin: mcp_server (stdio JSON-RPC)
│       ├── config.rs
│       ├── db.rs
│       ├── indexer/              # walker + per-language tree-sitter specs
│       │   ├── mod.rs
│       │   ├── languages.rs
│       │   └── parser.rs
│       ├── watcher.rs            # inotify + debounce
│       ├── impact.rs             # fan-in / fan-out / centralidade
│       ├── obsidian/mod.rs       # vault generator + graph.json colors
│       ├── claudemd/mod.rs       # CLAUDE.md renderer (idempotente)
│       └── web/                  # axum HTTP server + Basic Auth
│           ├── mod.rs
│           └── auth.rs
└── README.md
```

## Status

Este é o esqueleto inicial (v0). O que está implementado:

- [x] Build Docker multi-stage
- [x] Schema SQLite + migrations
- [x] Watcher com debounce
- [x] Parser tree-sitter para 4 linguagens (símbolos + relações + linhas)
- [x] Scores de impacto (fan-in/out/centralidade simples)
- [x] Vault Obsidian com cores customizadas
- [x] Template CLAUDE.md com blocos gerenciados
- [x] Servidor MCP stdio (6 tools)
- [x] Script de instalação global

Próximos passos (v1):
- [ ] Resolução cross-file de símbolos (usar qualified names globais)
- [ ] Betweenness centrality real
- [ ] Tree-sitter queries em vez de walker genérico (precisão por linguagem)
- [ ] Modo "patch" no `get_surgical_context` (devolver diff sugerido)
- [ ] Suporte a Go, Ruby, Java, C++
