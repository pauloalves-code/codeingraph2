#!/usr/bin/env bash
# ============================================================================
# codeingraph2 — Global installer
# ============================================================================
# 1. Verifica Docker.
# 2. Pergunta porta, usuário e senha da UI web (a menos que passados via flag).
# 3. Cria diretório do projeto em $SCRIPT_DIR/$INSTANCE_NAME/.
# 4. Escreve .env em $SCRIPT_DIR/$INSTANCE_NAME/.env.
# 5. Sobe o container com nome único (INSTANCE_NAME).
# 6. Atualiza registry.json em $SCRIPT_DIR/registry.json.
# 7. Registra servidor MCP global "codeingraph2" em ~/.mcp.json e ~/.claude.json.
#    (Uma única entrada global substitui entradas por projeto.)
#
# Suporte multi-projeto: use --name PROJETO para instalar mais de uma instância
# sem conflitos de container ou volume.
# ============================================================================
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

c_blue()  { printf '\033[1;34m%s\033[0m\n' "$*"; }
c_red()   { printf '\033[1;31m%s\033[0m\n' "$*" >&2; }
c_green() { printf '\033[1;32m%s\033[0m\n' "$*"; }
c_yellow(){ printf '\033[1;33m%s\033[0m\n' "$*"; }
log()  { c_blue   "[codeingraph2] $*"; }
warn() { c_yellow "[codeingraph2] $*"; }
err()  { c_red    "[codeingraph2] $*"; }
ok()   { c_green  "[codeingraph2] $*"; }

usage() {
    cat <<EOF
Usage: $0 [options]

Caminhos:
  --target PATH        Diretório de código a indexar (default: \$PWD/target_code)
  --vault  PATH        Saída do vault Obsidian (default: \$SCRIPT_DIR/\$NAME/obsidian_vault)

Instância:
  --name   NAME        Nome da instância (default: basename do --target)
                       Arquivos do projeto ficam em: $SCRIPT_DIR/<NAME>/
                       Instale com nomes diferentes para múltiplos projetos.

Web UI:
  --port   N           Porta do host para o viewer    (default: pergunta)
  --user   NAME        Usuário da UI                  (default: pergunta)
  --pass   SECRET      Senha da UI                    (default: pergunta, oculta)
  --no-web             Desativa a UI web
  --no-vault           Desativa a geração do vault Obsidian

Controle:
  --no-build           Pula docker compose build
  --no-start           Pula docker compose up
  --uninstall          Remove entrada do registry, MCP (se último projeto) e derruba container
  --non-interactive    Nunca faz prompt (usa defaults / flags)
  -h, --help           Esta ajuda

Exemplos:
  # Instalar para outro projeto na porta 3360
  $0 --target /docker/myproject --name myproject --port 3360

  # Instalar para epify na porta 3357
  $0 --target /docker/epify --name epify --port 3357
EOF
}

# ---------------------- defaults / args ----------------------
TARGET_CODE="${CODEINGRAPH2_TARGET:-}"
OBSIDIAN_VAULT_IN="${CODEINGRAPH2_VAULT:-}"
INSTANCE_NAME="${CODEINGRAPH2_INSTANCE:-}"
WEB_PORT="${WEB_PORT:-}"
WEB_USER_IN="${WEB_USER:-}"
WEB_PASS_IN=""
WEB_ENABLED_IN="1"
VAULT_ENABLED_IN="1"
DO_BUILD=1
DO_START=1
DO_UNINSTALL=0
INTERACTIVE=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)      TARGET_CODE="$2"; shift 2 ;;
        --vault)       OBSIDIAN_VAULT_IN="$2"; shift 2 ;;
        --name)        INSTANCE_NAME="$2"; shift 2 ;;
        --port)        WEB_PORT="$2"; shift 2 ;;
        --user)        WEB_USER_IN="$2"; shift 2 ;;
        --pass)        WEB_PASS_IN="$2"; shift 2 ;;
        --no-web)      WEB_ENABLED_IN="0"; shift ;;
        --no-vault)    VAULT_ENABLED_IN="0"; shift ;;
        --no-build)    DO_BUILD=0; shift ;;
        --no-start)    DO_START=0; shift ;;
        --uninstall)   DO_UNINSTALL=1; shift ;;
        --non-interactive) INTERACTIVE=0; shift ;;
        -h|--help)     usage; exit 0 ;;
        *) err "Opção desconhecida: $1"; usage; exit 1 ;;
    esac
done

TARGET_CODE="${TARGET_CODE:-$SCRIPT_DIR/target_code}"

# Derive instance name from target directory basename if not given
INSTANCE_NAME="${INSTANCE_NAME:-$(basename "$TARGET_CODE")}"
# Sanitize: keep only alphanumeric + hyphen + underscore
INSTANCE_NAME="$(echo "$INSTANCE_NAME" | tr -cs '[:alnum:]_-' '_' | sed 's/^_//;s/_$//')"
INSTANCE_NAME="${INSTANCE_NAME:-codeingraph2}"

# Project data directory: always inside SCRIPT_DIR/projects/<INSTANCE_NAME>/
PROJECT_DATA_DIR="${SCRIPT_DIR}/projects/${INSTANCE_NAME}"

# Vault defaults to <PROJECT_DATA_DIR>/obsidian_vault
OBSIDIAN_VAULT="${OBSIDIAN_VAULT_IN:-${PROJECT_DATA_DIR}/obsidian_vault}"

CONTAINER_NAME="${INSTANCE_NAME}_container"

REGISTRY_FILE="${SCRIPT_DIR}/registry.json"

# ---------------------- uninstall path ----------------------
if [[ $DO_UNINSTALL -eq 1 ]]; then
    log "Desinstalando instância '${INSTANCE_NAME}'..."
    ENV_FILE="${PROJECT_DATA_DIR}/.env"
    COMPOSE_EXTRA_ARGS="--env-file ${ENV_FILE} --project-name codeingraph2-${INSTANCE_NAME}"
    # shellcheck disable=SC2086
    (cd "$SCRIPT_DIR" && docker compose $COMPOSE_EXTRA_ARGS down 2>/dev/null || true)

    # Remove from registry.json
    if [[ -f "$REGISTRY_FILE" ]] && command -v python3 >/dev/null 2>&1; then
        python3 - "$REGISTRY_FILE" "$INSTANCE_NAME" <<'PY' || true
import json, sys
path, name = sys.argv[1], sys.argv[2]
try:
    with open(path) as f: cfg = json.load(f)
except Exception:
    sys.exit(0)
if "projects" in cfg and name in cfg["projects"]:
    del cfg["projects"][name]
    with open(path, "w") as f: json.dump(cfg, f, indent=2)
    print(f"removed '{name}' from registry")
remaining = len(cfg.get("projects", {}))
print(f"remaining projects: {remaining}")
sys.exit(0 if remaining > 0 else 10)
PY
        _reg_exit=$?
        # If no projects remain, stop MCP container and remove global MCP entries
        if [[ $_reg_exit -eq 10 ]]; then
            log "Último projeto removido — parando container MCP global..."
            (cd "$SCRIPT_DIR" && docker compose -f mcp-compose.yml down 2>/dev/null || true)
            _remove_global_mcp() {
                local path="$1"
                [[ -f "$path" ]] || return
                python3 - "$path" "codeingraph2" <<'PY' || true
import json, sys
path, name = sys.argv[1], sys.argv[2]
try:
    with open(path) as f: cfg = json.load(f)
except Exception:
    sys.exit(0)
if "mcpServers" in cfg and name in cfg["mcpServers"]:
    del cfg["mcpServers"][name]
    with open(path, "w") as f: json.dump(cfg, f, indent=2)
    print(f"removed global MCP from {path}")
PY
            }
            _remove_global_mcp "${HOME}/.mcp.json"
            _remove_global_mcp "${HOME}/.claude.json"
        fi
    fi
    ok "Desinstalado."
    exit 0
fi

# ---------------------- 1. Docker check ----------------------
if ! command -v docker >/dev/null 2>&1; then
    err "Docker não encontrado. Instale: https://docs.docker.com/engine/install/"
    exit 1
fi
if ! docker info >/dev/null 2>&1; then
    err "Docker daemon não está rodando. Inicie o Docker e rode de novo."
    exit 1
fi
if ! docker compose version >/dev/null 2>&1; then
    err "Docker Compose v2 não encontrado (docker compose)."
    exit 1
fi
ok "Docker OK."

log "Instância: ${INSTANCE_NAME} (container: ${CONTAINER_NAME})"
log "Dados do projeto: ${PROJECT_DATA_DIR}"

# ---------------------- 2. Prompts (web UI) ----------------------
if [[ "$WEB_ENABLED_IN" = "1" ]]; then
    if [[ -z "${WEB_PORT}" ]]; then
        if [[ $INTERACTIVE -eq 1 ]]; then
            read -r -p "Porta para a UI web (1024-65535) [7890]: " _p
            WEB_PORT="${_p:-7890}"
        else
            WEB_PORT="7890"
        fi
    fi
    if ! [[ "$WEB_PORT" =~ ^[0-9]+$ ]] || [[ "$WEB_PORT" -lt 1 || "$WEB_PORT" -gt 65535 ]]; then
        err "Porta inválida: $WEB_PORT"; exit 1
    fi

    if [[ -z "${WEB_USER_IN}" ]]; then
        if [[ $INTERACTIVE -eq 1 ]]; then
            read -r -p "Usuário da UI [admin]: " _u
            WEB_USER_IN="${_u:-admin}"
        else
            WEB_USER_IN="admin"
        fi
    fi

    if [[ -z "${WEB_PASS_IN}" ]]; then
        if [[ $INTERACTIVE -eq 1 ]]; then
            while :; do
                read -r -s -p "Senha da UI (mín. 6 caracteres): " _p1; echo
                read -r -s -p "Confirme:                         " _p2; echo
                if [[ -n "$_p1" && "$_p1" == "$_p2" && ${#_p1} -ge 6 ]]; then
                    WEB_PASS_IN="$_p1"; break
                fi
                warn "Não bateu ou < 6 caracteres — tente de novo."
            done
        else
            err "--pass é obrigatório em --non-interactive."; exit 1
        fi
    fi

    SALT_HEX="$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
    if command -v python3 >/dev/null 2>&1; then
        HASH_HEX="$(python3 - "$SALT_HEX" "$WEB_PASS_IN" <<'PY'
import sys, hashlib, binascii
salt = binascii.unhexlify(sys.argv[1])
h = hashlib.sha256(salt + sys.argv[2].encode()).hexdigest()
print(h)
PY
)"
    else
        HASH_HEX="$(printf '%s' "$WEB_PASS_IN" | ( { printf '%b' "$(echo "$SALT_HEX" | sed 's/../\\x&/g')"; cat; } | openssl dgst -sha256 -hex | awk '{print $NF}'))"
    fi
    WEB_AUTH_VAL="sha256:${SALT_HEX}:${HASH_HEX}"
else
    WEB_PORT="${WEB_PORT:-7890}"
    WEB_USER_IN=""
    WEB_AUTH_VAL=""
fi

# ---------------------- 3. Create project directory + .env ----------------------
mkdir -p "${PROJECT_DATA_DIR}" "${TARGET_CODE}" "${OBSIDIAN_VAULT}"

PROJECT_NAME="$(basename "$TARGET_CODE")"
ENV_FILE="${PROJECT_DATA_DIR}/.env"

cat > "$ENV_FILE" <<EOF
# Auto-gerado por install_global.sh em $(date -u +%Y-%m-%dT%H:%M:%SZ)
INSTANCE_NAME=${INSTANCE_NAME}
TARGET_CODE=${TARGET_CODE}
OBSIDIAN_VAULT=${OBSIDIAN_VAULT}
PROJECT_DATA_DIR=${PROJECT_DATA_DIR}
PROJECT_NAME=${PROJECT_NAME}
WEB_ENABLED=${WEB_ENABLED_IN}
VAULT_ENABLED=${VAULT_ENABLED_IN}
WEB_PORT=${WEB_PORT}
WEB_USER=${WEB_USER_IN}
WEB_AUTH=${WEB_AUTH_VAL}
EOF
chmod 600 "$ENV_FILE"
ok "Escreveu $ENV_FILE (modo 600)."

log "INSTANCE_NAME   = $INSTANCE_NAME"
log "TARGET_CODE     = $TARGET_CODE"
log "OBSIDIAN_VAULT  = $OBSIDIAN_VAULT"
log "PROJECT_DATA    = $PROJECT_DATA_DIR"
if [[ "$WEB_ENABLED_IN" = "1" ]]; then
    log "Web UI          = http://localhost:$WEB_PORT (user: $WEB_USER_IN)"
else
    log "Web UI          = desativada"
fi

# ---------------------- 4. Build + up ----------------------
COMPOSE_EXTRA_ARGS="--env-file ${ENV_FILE} --project-name codeingraph2-${INSTANCE_NAME}"

if [[ $DO_BUILD -eq 1 ]]; then
    log "Construindo imagem (pode demorar na primeira vez)..."
    # shellcheck disable=SC2086
    (cd "$SCRIPT_DIR" && docker compose $COMPOSE_EXTRA_ARGS build)
fi
if [[ $DO_START -eq 1 ]]; then
    log "Subindo container ${CONTAINER_NAME}..."
    # shellcheck disable=SC2086
    (cd "$SCRIPT_DIR" && docker compose $COMPOSE_EXTRA_ARGS up -d)
fi

# ---------------------- 5. Atualiza registry.json ----------------------
if ! command -v python3 >/dev/null 2>&1; then
    warn "python3 não encontrado — registry.json não atualizado."
else
    [[ -f "$REGISTRY_FILE" ]] || echo '{"projects":{}}' > "$REGISTRY_FILE"
    python3 - "$REGISTRY_FILE" "$INSTANCE_NAME" "$TARGET_CODE" "${PROJECT_DATA_DIR}/graph.db" <<'PY'
import json, sys
path, name, target, db = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
try:
    with open(path) as f: cfg = json.load(f)
except Exception:
    cfg = {"projects": {}}
cfg.setdefault("projects", {})
cfg["projects"][name] = {"target": target, "db": db}
with open(path, "w") as f: json.dump(cfg, f, indent=2)
print(f"  registered '{name}' in {path}")
PY
    ok "Registry atualizado: ${REGISTRY_FILE}"
fi

# ---------------------- 6. Sobe container MCP global (codeingraph2_mcp) ----------------------
# Um único container persistente serve a todos os projetos via `docker exec`.
# Usa a imagem já construída para evitar build extra.

MCP_CONTAINER="codeingraph2_mcp"

if [[ $DO_START -eq 1 ]]; then
    if ! docker ps --filter "name=^${MCP_CONTAINER}$" --filter "status=running" --format "{{.Names}}" | grep -q "^${MCP_CONTAINER}$"; then
        log "Subindo container MCP global '${MCP_CONTAINER}'..."
        (cd "$SCRIPT_DIR" && docker compose -f mcp-compose.yml up -d)
        ok "Container MCP global iniciado."
    else
        log "Container MCP global '${MCP_CONTAINER}' já está rodando."
    fi
fi

# ---------------------- 7. Registra MCP global "codeingraph2" ----------------------
# Usa `docker exec -i` no container persistente — sem latência de startup.

_register_mcp() {
    local path="$1" name="$2" cmd="$3" args_json="$4" indent="${5:-2}"
    python3 - "$path" "$name" "$cmd" "$args_json" "$indent" <<'PY'
import json, sys, os
path, name, cmd, args_json, indent = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], int(sys.argv[5])
args = json.loads(args_json)
try:
    with open(path) as f: cfg = json.load(f)
except Exception:
    cfg = {}
cfg.setdefault("mcpServers", {})
cfg["mcpServers"][name] = {"command": cmd, "args": args}
with open(path, "w") as f: json.dump(cfg, f, indent=indent)
print(f"  registered '{name}' in {path}")
PY
}

if ! command -v python3 >/dev/null 2>&1; then
    warn "python3 não encontrado — adicione manualmente em ~/.mcp.json e ~/.claude.json"
else
    GLOBAL_MCP_NAME="codeingraph2"
    GLOBAL_MCP_COMMAND="docker"
    GLOBAL_MCP_ARGS="[\"exec\",\"-i\",\"${MCP_CONTAINER}\",\"/usr/local/bin/mcp_server\"]"

    # ~/.mcp.json  (Claude Code CLI)
    MCP_JSON="${HOME}/.mcp.json"
    [[ -f "$MCP_JSON" ]] || echo '{"mcpServers":{}}' > "$MCP_JSON"
    _register_mcp "$MCP_JSON" "$GLOBAL_MCP_NAME" "$GLOBAL_MCP_COMMAND" "$GLOBAL_MCP_ARGS" 2
    ok "MCP global '${GLOBAL_MCP_NAME}' registrado em ${MCP_JSON}."

    # ~/.claude.json  (Claude Code VSCode extension)
    CLAUDE_JSON="${HOME}/.claude.json"
    if [[ -f "$CLAUDE_JSON" ]]; then
        _register_mcp "$CLAUDE_JSON" "$GLOBAL_MCP_NAME" "$GLOBAL_MCP_COMMAND" "$GLOBAL_MCP_ARGS" 4
        ok "MCP global '${GLOBAL_MCP_NAME}' registrado em ${CLAUDE_JSON}."
    fi
fi

ok "Instalação concluída."
cat <<EOF

Próximos passos:
  1. Reinicie o Claude Desktop para carregar o MCP.
  2. Código indexado em:           $TARGET_CODE
  3. Dados do projeto em:          $PROJECT_DATA_DIR
  4. Vault Obsidian em:            $OBSIDIAN_VAULT
EOF
if [[ "$WEB_ENABLED_IN" = "1" ]]; then
cat <<EOF
  5. Abra a UI web:                http://localhost:$WEB_PORT
     (usuário: $WEB_USER_IN)
EOF
fi
cat <<EOF
  6. Logs:                         docker logs -f ${CONTAINER_NAME}
  7. Status:                       docker exec ${CONTAINER_NAME} codeingraph2 health
  8. MCP global registrado como:   codeingraph2
     Projetos disponíveis via:     list_projects (tool MCP)
     Registry:                     ${REGISTRY_FILE}
EOF
