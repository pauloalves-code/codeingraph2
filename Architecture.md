# codeingraph2 — Arquitetura

## Visão Geral

codeingraph2 é um daemon Rust que monitora um repositório de código-fonte em tempo real, constrói um grafo de símbolos e relações em SQLite, e expõe esse grafo via dois canais complementares: um servidor MCP (stdio JSON-RPC 2.0) para consumo direto por modelos de linguagem, e uma interface web com visualização de grafo force-directed.

O projeto suporta múltiplos repositórios de forma simultânea. Cada repositório tem seu próprio container Docker rodando o daemon. Um único servidor MCP global roteia as consultas para o banco de dados correto com base em um parâmetro `project`.

---

## Estrutura de Diretórios no Host

```
<install_dir>/                  ← diretório onde install_global.sh reside
├── Architecture.md
├── docker-compose.yml
├── mcp-compose.yml             ← compose do container MCP persistente
├── install_global.sh
├── registry.json               ← registro global de projetos
├── Dockerfile
├── daemon/
│   └── src/                    ← código Rust
│
└── projects/                   ← dados de todos os projetos (não commitado)
    ├── project-a/
    │   ├── .env
    │   ├── graph.db            ← SQLite WAL
    │   └── obsidian_vault/     ← vault Obsidian (se habilitado)
    └── project-b/
        ├── .env
        └── graph.db
```

O diretório `projects/` está no `.gitignore` e nunca entra no repositório.

### registry.json

Arquivo JSON em `<install_dir>/registry.json` que mapeia nome de projeto para seus caminhos no host:

```json
{
  "projects": {
    "my-project": {
      "target": "/path/to/my-project",
      "db":     "<install_dir>/projects/my-project/graph.db"
    }
  }
}
```

---

## Instalação de um Projeto (`install_global.sh`)

O script `install_global.sh` é o único ponto de entrada para adicionar um repositório ao codeingraph2. Ele executa os seguintes passos:

1. Valida a presença do Docker e do Docker Compose v2.
2. Coleta (ou recebe via flags) porta, usuário e senha da UI web.
3. Cria o diretório `projects/<INSTANCE_NAME>/` dentro do diretório de instalação.
4. Escreve `projects/<INSTANCE_NAME>/.env` com todas as variáveis de ambiente do container.
5. Executa `docker compose build` (se necessário) e `docker compose up -d` para o daemon.
6. Adiciona ou atualiza a entrada do projeto em `registry.json`.
7. Garante que o container MCP global (`codeingraph2_mcp`) esteja rodando.
8. Registra (ou atualiza) o servidor MCP global `codeingraph2` em `~/.mcp.json` e `~/.claude.json`.

O `.env` de cada projeto contém:

| Variável | Significado |
|---|---|
| `INSTANCE_NAME` | Nome da instância |
| `TARGET_CODE` | Caminho host do código a indexar |
| `OBSIDIAN_VAULT` | Caminho host do vault Obsidian de saída |
| `PROJECT_DATA_DIR` | Diretório host para `graph.db` e metadados |
| `WEB_PORT` | Porta exposta da UI web |
| `WEB_USER` / `WEB_AUTH` | Credenciais da UI (sha256 com salt) |

---

## Deployment (Docker)

### Containers

| Container | Imagem | Função |
|---|---|---|
| `<INSTANCE_NAME>_container` | `codeingraph2:latest` | Daemon por projeto (indexação + watcher + UI web) |
| `codeingraph2_mcp` | `codeingraph2:latest` | Container MCP global persistente (`sleep infinity`) |

### Daemon por projeto — bind mounts (docker-compose.yml)

```
HOST                                    CONTAINER
──────────────────────────────────────────────────────────
${TARGET_CODE}                    →   /target_code
${OBSIDIAN_VAULT}                 →   /obsidian_vault
${PROJECT_DATA_DIR}               →   /var/lib/codeingraph2
```

`/var/lib/codeingraph2` é um bind mount (não volume nomeado), o que permite que o servidor MCP global acesse o `graph.db` de qualquer projeto diretamente pelo filesystem do host, sem precisar de `docker exec` no daemon.

### Container MCP global — mcp-compose.yml

```yaml
container_name: codeingraph2_mcp
image: codeingraph2:latest
entrypoint: ["/bin/sleep", "infinity"]
volumes:
  - /docker:/docker
environment:
  CODEINGRAPH2_REGISTRY: <install_dir>/registry.json
```

O container fica em execução contínua. O Claude invoca o servidor MCP via `docker exec -i codeingraph2_mcp /usr/local/bin/mcp_server`, eliminando a latência de startup de um `docker run --rm`.

### Container daemon

- **Nome:** `${INSTANCE_NAME}_container`
- **Comando:** `codeingraph2 daemon`
- **Healthcheck:** `codeingraph2 health` (verifica integridade do SQLite)
- **Restart policy:** `unless-stopped`

---

## Arquitetura Interna do Daemon

### Binários

O Dockerfile produz dois binários independentes:

| Binário | Uso |
|---|---|
| `/usr/local/bin/codeingraph2` | Daemon principal (entrypoint do container) |
| `/usr/local/bin/mcp_server` | Servidor MCP (executado via `docker exec`) |

### Subcomandos do `codeingraph2`

| Subcomando | O que faz |
|---|---|
| `daemon` | Modo principal: index completo → UI web → watcher (loop infinito) |
| `index` | Index único sem daemon |
| `web` | Só a UI web (sem watcher) |
| `health` | Sanity check do SQLite |
| `vault` | Regenera vault Obsidian |
| `claudemd` | Regenera CLAUDE.md |
| `stats` | Exibe contagens do grafo em JSON |

### Inicialização do Daemon (`run_daemon`)

```
1. index_tree(target)            ← index completo sincrôno
2. impact::recompute()           ← calcula fan-in / fan-out / centrality
3. obsidian::generate()          ← gera vault Obsidian (se habilitado)
4. claudemd::render()            ← gera/atualiza CLAUDE.md

5. Spawn concorrente (tokio::select!):
   ├── web::serve()              ← axum HTTP server (async task)
   └── watcher::run_blocking()   ← inotify loop (spawn_blocking thread)
```

---

## Módulos

### `config` — Configuração

`Config::from_env()` lê todas as variáveis de ambiente e produz uma struct `Config` usada por todos os módulos. Não há arquivo de configuração — todas as configurações vêm de env vars injetadas pelo Docker Compose.

Campos principais: `target`, `vault`, `db_path`, `templates_dir`, `debounce_ms`, `web_enabled`, `web_user`, `web_auth`, `vault_enabled`.

---

### `db` — Banco de Dados (SQLite + WAL)

Wrapper fino sobre `rusqlite`. O daemon usa uma única conexão de escrita protegida por `Mutex<Connection>` (`Pool`). O servidor MCP abre conexões read-only independentes via `open_readonly()`.

**Configuração SQLite:**
- `PRAGMA journal_mode = WAL` — permite leituras concorrentes durante escrita
- `PRAGMA synchronous = NORMAL` — performance sem perder durabilidade em crash
- `PRAGMA foreign_keys = ON`

**Migrações:** arquivos `.sql` em `/opt/codeingraph2/migrations/`, aplicados em ordem lexicográfica no startup.

**Schema (tabelas principais):**

| Tabela | Conteúdo |
|---|---|
| `files` | Arquivos indexados com hash SHA256, contagem de linhas, timestamp |
| `symbols` | Símbolos extraídos: nome, qualified name, kind, assinatura, linhas, parent |
| `relations` | Arestas do grafo: source_symbol_id, target_symbol_id, kind, linha |
| `line_index` | Mapeamento linha → símbolo (para lookup por linha) |
| `impact_scores` | fan_in, fan_out, centrality por símbolo |
| `schema_meta` | Versão do schema |

---

### `indexer` — Indexação do Código-Fonte

Responsável por percorrer o filesystem e popular o banco.

**Linguagens suportadas (por extensão):**
- `.rs` → Rust
- `.py`, `.pyi` → Python
- `.js`, `.mjs`, `.cjs` → JavaScript
- `.ts`, `.tsx` → TypeScript

**Diretórios ignorados:** `.git`, `node_modules`, `target`, `dist`, `build`, `.venv`, `venv`, `__pycache__`, `.next`, `.cache`, `.idea`, `.vscode`

**Fluxo de indexação de um arquivo (`index_file`):**
1. Lê o conteúdo e calcula hash SHA256 (para cache de conteúdo)
2. Chama `parser::parse()` para extrair símbolos e relações via tree-sitter
3. Abre transação SQLite:
   - Upsert na tabela `files`
   - Delete dos símbolos antigos do arquivo (CASCADE limpa relações e line_index)
   - Insert dos novos símbolos com `parent_symbol_id` resolvido localmente
   - Insert das relações (resolve `target_symbol_id` quando o alvo já existe)
   - Rebuild do `line_index` para o arquivo
4. Commit

Após o index completo de todos os arquivos, `resolve_unresolved_relations()` faz uma segunda passagem para resolver referências cross-file que não puderam ser resolvidas na primeira passagem.

#### `parser` — Tree-sitter

`parse(lang, path, src)` usa tree-sitter para construir a AST e faz um walk recursivo sobre ela.

**Tipos de símbolo (`SymbolKind`):**
`File`, `Class`, `Function`, `Method`, `Variable`, `Constant`, `Enum`, `Trait`, `Module`

**Tipos de relação (`RelationKind`):**
`Calls`, `Inherits`, `Imports`, `References`, `Contains`, `Implements`, `Assigns`, `Reads`, `UsesType`

Cada arquivo gera um símbolo sintético do tipo `File` como raiz. Símbolos aninhados (método dentro de classe) geram automaticamente uma relação `Contains`.

#### `languages` — Especificações por Linguagem

`LangSpec` define para cada linguagem quais nós da AST do tree-sitter correspondem a símbolos e relações, e qual campo contém o nome do símbolo. `for_lang(key)` retorna a spec correta.

---

### `impact` — Pontuação de Impacto

`recompute(pool)` recalcula três métricas para cada símbolo:

- **fan_in:** quantas relações apontam *para* este símbolo (quantos dependem dele)
- **fan_out:** quantas relações partem *deste* símbolo (quantas dependências ele tem)
- **centrality:** `(fan_in + fan_out) / max_total` normalizado entre 0 e 1

Estas métricas alimentam o ranking no `get_surgical_context` e na listagem do CLAUDE.md.

---

### `watcher` — Monitor de Filesystem

`run_blocking(cfg, pool)` usa o crate `notify` com inotify (Linux) no modo `RecursiveMode::Recursive` para monitorar todo o diretório `target`.

**Debounce:** eventos são coletados num `HashSet<PathBuf>` e processados em batch somente após `debounce_ms` (padrão 750ms) de inatividade.

**Supressão de eventos (4 camadas para evitar loops):**

| Tipo | Mecanismo | Por quê |
|---|---|---|
| `CLAUDE.md` | Comparação exata de path | O daemon escreve este arquivo — sem supressão causaria loop infinito |
| Vault (`/obsidian_vault`) | `path.starts_with(cfg.vault)` | Arquivos gerados pelo `obsidian::generate` |
| Vault bind-mounted dentro do target | Detecção por inode (comparação dev+ino em filhos diretos do target) | Quando `OBSIDIAN_VAULT` é subdiretório de `TARGET_CODE` no host, inotify reporta eventos do vault como paths sob `/target_code` |
| Data dirs de outros projetos | Presença de `graph.db` + `graph.db-shm` em filhos diretos do target **e** dentro de `projects/`| Quando `PROJECT_DATA_DIR` de outros projetos é subdiretório do `TARGET_CODE` (caso do auto-install do codeingraph2 monitorando a si mesmo) |

**Processamento de um batch:**
```
1. reindex_path() para cada arquivo modificado
2. impact::recompute()
3. obsidian::generate() (se vault_enabled)
4. claudemd::render()
```

`reindex_path()` detecta automaticamente: arquivo removido → `remove_file()`; arquivo modificado → `index_file()`.

---

### `web` — Interface Web (axum)

Servidor HTTP built com axum, com o HTML/JS do viewer embutido em tempo de compilação via `include_str!`.

**Rotas:**

| Rota | Descrição |
|---|---|
| `GET /` | Viewer de grafo (single-page app, HTML inlined) |
| `GET /healthz` | Health check (sem autenticação) |
| `GET /api/graph` | Nós e arestas do grafo (filtros: `kind`, `q`, `limit`) |
| `GET /api/node/:id` | Metadados completos de um símbolo + trecho de código-fonte |
| `GET /api/edge/:id` | Metadados de uma relação + contexto de linha |
| `GET /api/source` | Trecho raw de código por `?file=&start=&end=` |
| `GET /api/stats` | Contagens de files/symbols/relations |

**Autenticação:** HTTP Basic Auth com senha armazenada como `sha256:<salt_hex>:<hash_hex>`. O salt é gerado aleatoriamente pelo `install_global.sh` em cada instalação.

**Paleta de cores do grafo:**

| Tipo | Cor |
|---|---|
| Files / Modules | `#999999` (cinza) |
| Classes / Traits / Enums | `#ff4d4d` (vermelho) |
| Functions / Methods | `#4d94ff` (azul) |
| Variables / Constants | `#ffdb4d` (amarelo) |

---

### `obsidian` — Vault Obsidian

`generate(pool, cfg)` produz uma pasta de notas Markdown compatível com Obsidian em `cfg.vault`.

**Estrutura do vault:**
```
obsidian_vault/
├── Files/          uma nota por arquivo indexado
├── Classes/        uma nota por class/enum/trait/module
├── Functions/      uma nota por function/method
├── Variables/      uma nota por variable/constant
└── .obsidian/
    └── graph.json  grupos de cor para o Graph View
```

Cada nota usa wikilinks (`[[NomeDoSímbolo]]`) para que o Graph View do Obsidian desenhe as arestas automaticamente. O `graph.json` configura os grupos de cor com a mesma paleta da UI web.

---

### `claudemd` — Geração do CLAUDE.md

`render(pool, cfg)` gera ou atualiza o arquivo `CLAUDE.md` na raiz do projeto indexado.

**Idempotência:** lê o conteúdo atual antes de escrever. Se o conteúdo gerado for igual ao existente, não há escrita — o que evita disparar eventos inotify desnecessários.

**Preservação de conteúdo manual:** o arquivo usa marcadores HTML:
```
<!-- codeingraph2:begin -->
  (bloco gerado automaticamente — substituído a cada reindexação)
<!-- codeingraph2:end -->
```
Qualquer texto fora deste bloco é preservado entre reindexações.

**Conteúdo gerado:** estatísticas do grafo, lista de símbolos com maior fan-in e fan-out, convenções de nomenclatura detectadas automaticamente, lista de ferramentas MCP disponíveis, e instruções de uso para o modelo de linguagem.

---

## Servidor MCP Global

### Invocação

O servidor MCP roda via `docker exec` no container persistente `codeingraph2_mcp`:

```bash
docker exec -i codeingraph2_mcp /usr/local/bin/mcp_server
```

O container tem `/docker` montado e conhece o caminho do `registry.json` via env var `CODEINGRAPH2_REGISTRY`. Não há latência de startup — o exec é instantâneo.

### Protocolo

JSON-RPC 2.0 sobre stdio (newline-delimited). Implementa o subconjunto do protocolo MCP:
- `initialize` — handshake com capabilities e instruções de uso
- `tools/list` — lista as ferramentas disponíveis
- `tools/call` — execução de ferramenta

### Roteamento Multi-Projeto

`load_registry()` lê o `registry.json` apontado por `CODEINGRAPH2_REGISTRY`.

`resolve_context(registry, args)` determina qual projeto usar em cada chamada de ferramenta:
1. Se `args["project"]` está presente, usa esse projeto (erro se não encontrado)
2. Se a variável `CODEINGRAPH2_PROJECT` está definida, usa esse projeto
3. Caso contrário, usa o primeiro projeto do registry

### Ferramentas MCP

Todas as ferramentas aceitam o parâmetro opcional `"project"`.

| Ferramenta | Descrição |
|---|---|
| `list_projects` | Lista todos os projetos do registry (nome, target, caminho do DB) |
| `get_surgical_context` | Retorna o símbolo raiz + todos os snippets relacionados até profundidade N, cada um com `source` já incluído — elimina a necessidade de leituras de arquivo |
| `patch_symbol` | Substitui um símbolo pelo nome, sem precisar de `old_string` — localiza as linhas exatas no grafo e reescreve o arquivo |
| `query_graph` | Busca símbolos por nome/tipo/arquivo. Substitui grep |
| `get_symbol` | Metadados completos de um símbolo (assinatura, localização, tipo) |
| `get_callers` | Quem chama o símbolo X, transitivo até profundidade N |
| `get_callees` | O que o símbolo X chama |
| `graph_stats` | Contagens globais (files, symbols, relations) por linguagem |

O servidor abre conexões SQLite **read-only** para todas as ferramentas exceto `patch_symbol`, que escreve diretamente nos arquivos do target. A reindexação é feita automaticamente pelo daemon na próxima janela do watcher.

### Registro no Claude

Uma única entrada é registrada em `~/.mcp.json` e `~/.claude.json`:

```json
{
  "mcpServers": {
    "codeingraph2": {
      "command": "docker",
      "args": ["exec", "-i", "codeingraph2_mcp", "/usr/local/bin/mcp_server"]
    }
  }
}
```

---

## Fluxo de Dados Completo

```
                     ┌────────────────────────────────────────────────┐
                     │            Host filesystem                     │
                     │                                                │
  código-fonte  ───► │  /path/to/project/         (TARGET_CODE)       │
  (editado pelo │    │  <install_dir>/projects/name/graph.db          │
   dev ou IA)   │    └──────────────┬─────────────────────────────────┘
                │                   │ bind mounts
                │    ┌──────────────▼─────────────────────────────────┐
                │    │         Container <name>_container             │
                │    │                                                │
                │    │  /target_code  →  inotify watch                │
                │    │       │                                        │
                │    │       ▼ evento de arquivo                      │
                │    │  watcher::run_blocking()                       │
                │    │       │ batch (debounce 750ms)                 │
                │    │       ▼                                        │
                │    │  indexer::reindex_path()  ──► SQLite WAL       │
                │    │  impact::recompute()       ──► impact_scores   │
                │    │  obsidian::generate()      ──► /obsidian_vault │
                │    │  claudemd::render()        ──► CLAUDE.md       │
                │    │                                                │
                │    │  web::serve() :<WEB_PORT> ──► UI web           │
                │    └────────────────────────────────────────────────┘
                │
                │    ┌────────────────────────────────────────────────┐
                │    │     Container codeingraph2_mcp (global)        │
                │    │           sleep infinity                       │
                │    │                                                │
  Claude  ◄─────┼───►│  docker exec -i ... mcp_server                 │
  (AI)          │    │       │ stdin/stdout JSON-RPC 2.0              │
                │    │                                                │
                │    │  resolve_context(project="name")               │
                │    │       │                                        │
                │    │  SQLite read-only ◄── graph.db no host         │
                │    │                                                │
                │    │  patch_symbol  ──► escreve em /path/project/   │
                │    └────────────────────────────────────────────────┘
```

---

## Adicionando um Novo Projeto

```bash
/path/to/codeingraph2/install_global.sh \
  --target /path/to/my-project \
  --name   my-project \
  --port   3361
```

Isso cria `projects/my-project/`, inicia o container `my-project_container`, adiciona a entrada no `registry.json`, e atualiza a entrada global `codeingraph2` no MCP (que já existia — não cria uma nova entrada por projeto).

A partir daí, o Claude pode usar:
```json
{ "tool": "graph_stats", "arguments": { "project": "my-project" } }
```
