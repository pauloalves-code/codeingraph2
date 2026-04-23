# codeingraph2

Daemon Docker que indexa um repositório em um grafo SQLite e o expõe via MCP para que o Claude Code puxe **contexto cirúrgico** — apenas os trechos exatos que importam — antes de qualquer refatoração.

[Read in English](README_EN.md)

## UI Web

![codeingraph2 web viewer](docs/ui-screenshot.png)

> Grafo interativo com força centrípeta, filtros por tipo, busca por nome e painel lateral com código-fonte ao clicar em qualquer nó ou aresta.

## O que gera

| Saída | Descrição |
|---|---|
| Servidor MCP (`mcp_server`) | 6 ferramentas de navegação eficiente em tokens |
| Viewer web | Grafo Cytoscape na porta escolhida (Basic Auth) |
| `CLAUDE.md` | Injetado automaticamente no projeto-alvo para guiar o Claude |
| Vault Obsidian | Mapa visual colorido (opcional, `--no-vault` para desativar) |

## Instalação rápida

```bash
cd /docker/codeingraph2
./install_global.sh --target /caminho/do/repo --name meuprojeto --port 7890
```

Modo não-interativo (para scripts):

```bash
./install_global.sh \
  --target /caminho/do/repo \
  --name meuprojeto \
  --port 7890 --user admin --pass 'segredo!' \
  --non-interactive
```

Após instalar: reinicie o Claude Desktop — o MCP `codeingraph2-meuprojeto` aparece automaticamente.

## Flags do install_global.sh

| Flag | Padrão | Descrição |
|---|---|---|
| `--target PATH` | `./target_code` | Diretório a indexar |
| `--name NAME` | basename do `--target` | Nome da instância (container, volume, entrada MCP) |
| `--port N` | pergunta | Porta da UI web |
| `--user NAME` | pergunta | Usuário da UI web |
| `--pass SECRET` | pergunta | Senha da UI (mín. 6 chars) |
| `--no-web` | — | Desativa a UI web |
| `--no-vault` | — | Desativa a geração do vault Obsidian |
| `--no-build` | — | Pula `docker compose build` |
| `--no-start` | — | Pula `docker compose up` |
| `--non-interactive` | — | Nunca faz prompt (requer `--pass`) |
| `--uninstall` | — | Para o container e remove o MCP |

## Subcomandos do daemon

```bash
docker exec meuprojeto_container codeingraph2 daemon     # watcher + indexer + UI web
docker exec meuprojeto_container codeingraph2 index      # reindexação completa (one-shot)
docker exec meuprojeto_container codeingraph2 vault      # regenera vault Obsidian
docker exec meuprojeto_container codeingraph2 claudemd   # regenera CLAUDE.md
docker exec meuprojeto_container codeingraph2 web        # apenas a UI web
docker exec meuprojeto_container codeingraph2 stats      # estatísticas JSON do grafo
docker exec meuprojeto_container codeingraph2 health     # exit 0 se saudável
```

## Ferramentas MCP

| Ferramenta | Descrição |
|---|---|
| `get_surgical_context` | Trechos exatos de código impactados por um símbolo (BFS até `depth`) |
| `query_graph` | Busca por nome / tipo / arquivo — retorna arquivo + número de linha |
| `get_symbol` | Metadados completos: assinatura, arquivo, linhas exatas, docstring |
| `get_callers` | Quem chama o símbolo X (transitivo, até profundidade N) |
| `get_callees` | O que o símbolo X chama |
| `graph_stats` | Contagens globais (arquivos, símbolos, relações) |

## Linguagens suportadas

Rust, Python, JavaScript, TypeScript.

Para adicionar uma linguagem: inclua uma entrada em [`daemon/src/indexer/languages.rs`](daemon/src/indexer/languages.rs) com a `Language` do tree-sitter e os mapeamentos de `symbol_nodes` / `relation_nodes`.

## Estrutura do repositório

```
.
├── Dockerfile                    # build multi-stage Rust
├── docker-compose.yml
├── install_global.sh             # instalador global do MCP
├── templates/CLAUDE.md.tmpl      # template injetado no projeto-alvo
└── daemon/src/
    ├── main.rs                   # entry point CLI (subcomandos clap)
    ├── bin/mcp_server.rs         # servidor MCP stdio (JSON-RPC)
    ├── config.rs                 # configuração via variáveis de ambiente
    ├── db.rs                     # pool SQLite + migrations
    ├── indexer/                  # walker + specs tree-sitter por linguagem
    ├── watcher.rs                # inotify + debounce + purge de arquivos obsoletos
    ├── impact.rs                 # scores fan-in / fan-out / centralidade
    ├── obsidian/mod.rs           # gerador do vault (opcional)
    ├── claudemd/mod.rs           # renderer do CLAUDE.md (idempotente)
    └── web/                      # servidor HTTP axum + Basic Auth
```

## Status

- [x] Build Docker multi-stage
- [x] Schema SQLite + migrations
- [x] Watcher com debounce + purge de arquivos obsoletos
- [x] Parser tree-sitter (4 linguagens — símbolos, relações, linhas)
- [x] Scores de impacto (fan-in / fan-out / centralidade)
- [x] Vault Obsidian com cores customizadas (opcional via `--no-vault`)
- [x] Template CLAUDE.md idempotente
- [x] Servidor MCP stdio (6 ferramentas)
- [x] Instalador multi-projeto
