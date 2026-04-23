#!/usr/bin/env bash
# ============================================================================
# codeingraph2 — Global installer
# ============================================================================
# 1. Verifica Docker.
# 2. Pergunta porta, usuário e senha da UI web (a menos que passados via flag).
# 3. Escreve .env para o docker-compose consumir.
# 4. Sobe o container com nome único (INSTANCE_NAME).
# 5. Registra o servidor MCP no ~/.mcp.json e via `claude mcp add-json`.
#
# Suporte multi-projeto: use --name PROJETO para instalar mais de uma instância
# sem conflitos de container ou volume. Cada instância fica em sua própria porta.
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
  --vault  PATH        Saída do vault Obsidian        (default: \$PWD/obsidian_vault)

Instância:
  --name   NAME        Nome da instância (default: basename do --target)
                       Usado para container_name, volume e entrada MCP.
                       Instale com nomes diferentes para múltiplos projetos.

Web UI:
  --port   N           Porta do host para o viewer    (default: pergunta)
  --user   NAME        Usuário da UI                  (default: pergunta)
  --pass   SECRET      Senha da UI                    (default: pergunta, oculta)
  --no-web             Desativa a UI web

Controle:
  --no-build           Pula docker compose build
  --no-start           Pula docker compose up
  --uninstall          Remove entrada MCP e derruba container
  --non-interactive    Nunca faz prompt (usa defaults / flags)
  -h, --help           Esta ajuda

Exemplos:
  # Instalar para o projeto myproject na porta 3360
  $0 --target /docker/myproject --name myproject --port 3360

  # Instalar para codeingraph2 na porta 3358
  $0 --target /docker/codeingraph2 --name codeingraph2 --port 3358
EOF
}

# ---------------------- defaults / args ----------------------
TARGET_CODE="${CODEINGRAPH2_TARGET:-}"
OBSIDIAN_VAULT="${CODEINGRAPH2_VAULT:-}"
INSTANCE_NAME="${CODEINGRAPH2_INSTANCE:-}"
WEB_PORT="${WEB_PORT:-}"
WEB_USER_IN="${WEB_USER:-}"
WEB_PASS_IN=""
WEB_ENABLED_IN="1"
DO_BUILD=1
DO_START=1
DO_UNINSTALL=0
INTERACTIVE=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)      TARGET_CODE="$2"; shift 2 ;;
        --vault)       OBSIDIAN_VAULT="$2"; shift 2 ;;
        --name)        INSTANCE_NAME="$2"; shift 2 ;;
        --port)        WEB_PORT="$2"; shift 2 ;;
        --user)        WEB_USER_IN="$2"; shift 2 ;;
        --pass)        WEB_PASS_IN="$2"; shift 2 ;;
        --no-web)      WEB_ENABLED_IN="0"; shift ;;
        --no-build)    DO_BUILD=0; shift ;;
        --no-start)    DO_START=0; shift ;;
        --uninstall)   DO_UNINSTALL=1; shift ;;
        --non-interactive) INTERACTIVE=0; shift ;;
        -h|--help)     usage; exit 0 ;;
        *) err "Opção desconhecida: $1"; usage; exit 1 ;;
    esac
done

TARGET_CODE="${TARGET_CODE:-$SCRIPT_DIR/target_code}"
OBSIDIAN_VAULT="${OBSIDIAN_VAULT:-$SCRIPT_DIR/obsidian_vault}"
# Derive instance name from target directory basename if not given
INSTANCE_NAME="${INSTANCE_NAME:-$(basename "$TARGET_CODE")}"
# Sanitize: keep only alphanumeric + hyphen + underscore
INSTANCE_NAME="$(echo "$INSTANCE_NAME" | tr -cs '[:alnum:]_-' '_' | sed 's/^_//;s/_$//')"
INSTANCE_NAME="${INSTANCE_NAME:-codeingraph2}"

CONTAINER_NAME="${INSTANCE_NAME}_container"
MCP_NAME="codeingraph2-${INSTANCE_NAME}"   # unique per instance, e.g. codeingraph2-myproject
MCP_COMMAND="docker"
MCP_ARGS="[\"exec\",\"-i\",\"${CONTAINER_NAME}\",\"/usr/local/bin/mcp_server\"]"

# ---------------------- uninstall path ----------------------
if [[ $DO_UNINSTALL -eq 1 ]]; then
    log "Desinstalando instância '${INSTANCE_NAME}'..."
    # Determine env file location for this instance (mirrors install logic)
    _target_real="$(realpath "$TARGET_CODE" 2>/dev/null || echo "$TARGET_CODE")"
    _script_real="$(realpath "$SCRIPT_DIR" 2>/dev/null || echo "$SCRIPT_DIR")"
    if [[ "$_target_real" == "$_script_real" ]]; then
        _uninstall_extra=""
    else
        _env="/opt/codeingraph2/${INSTANCE_NAME}.env"
        _uninstall_extra="--env-file $_env --project-name codeingraph2-${INSTANCE_NAME}"
    fi
    # shellcheck disable=SC2086
    (cd "$SCRIPT_DIR" && docker compose $_uninstall_extra down 2>/dev/null || true)
    # Remove from ~/.mcp.json
    MCP_JSON="${HOME}/.mcp.json"
    if [[ -f "$MCP_JSON" ]]; then
        python3 - "$MCP_JSON" "$MCP_NAME" <<'PY' || true
import json, sys
path, name = sys.argv[1], sys.argv[2]
try:
    with open(path) as f: cfg = json.load(f)
except Exception:
    sys.exit(0)
if "mcpServers" in cfg and name in cfg["mcpServers"]:
    del cfg["mcpServers"][name]
    with open(path, "w") as f: json.dump(cfg, f, indent=2)
    print(f"removed {name} from {path}")
PY
    fi
    # Remove from ~/.claude.json (Claude Code VSCode extension)
    CLAUDE_JSON="${HOME}/.claude.json"
    if [[ -f "$CLAUDE_JSON" ]]; then
        python3 - "$CLAUDE_JSON" "$MCP_NAME" <<'PY' || true
import json, sys
path, name = sys.argv[1], sys.argv[2]
try:
    with open(path) as f: cfg = json.load(f)
except Exception:
    sys.exit(0)
if "mcpServers" in cfg and name in cfg["mcpServers"]:
    del cfg["mcpServers"][name]
    with open(path, "w") as f: json.dump(cfg, f, indent=4)
    print(f"removed {name} from {path}")
PY
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

# ---------------------- 3. .env for docker-compose ----------------------
PROJECT_NAME="$(basename "$TARGET_CODE")"

# Determine if this is a self-install (TARGET_CODE == SCRIPT_DIR) or an
# external install. External installs get their own isolated env file so that
# $SCRIPT_DIR/.env — which drives the canonical codeingraph2 instance — is
# never overwritten by a secondary project installation.
_target_real="$(realpath "$TARGET_CODE" 2>/dev/null || echo "$TARGET_CODE")"
_script_real="$(realpath "$SCRIPT_DIR" 2>/dev/null || echo "$SCRIPT_DIR")"

if [[ "$_target_real" == "$_script_real" ]]; then
    # Self-install: write directly into the project directory as before.
    ENV_FILE="$SCRIPT_DIR/.env"
    COMPOSE_EXTRA_ARGS=""
else
    # External install: keep the env file completely outside SCRIPT_DIR.
    mkdir -p /opt/codeingraph2
    ENV_FILE="/opt/codeingraph2/${INSTANCE_NAME}.env"
    # Pass a separate project name so docker compose doesn't mix project state
    # with other instances running from the same SCRIPT_DIR.
    COMPOSE_EXTRA_ARGS="--env-file $ENV_FILE --project-name codeingraph2-${INSTANCE_NAME}"
fi

cat > "$ENV_FILE" <<EOF
# Auto-gerado por install_global.sh em $(date -u +%Y-%m-%dT%H:%M:%SZ)
INSTANCE_NAME=$INSTANCE_NAME
TARGET_CODE=$TARGET_CODE
OBSIDIAN_VAULT=$OBSIDIAN_VAULT
PROJECT_NAME=$PROJECT_NAME
WEB_ENABLED=$WEB_ENABLED_IN
WEB_PORT=$WEB_PORT
WEB_USER=$WEB_USER_IN
WEB_AUTH=$WEB_AUTH_VAL
EOF
chmod 600 "$ENV_FILE"
ok "Escreveu $ENV_FILE (modo 600)."

mkdir -p "$TARGET_CODE" "$OBSIDIAN_VAULT"
log "INSTANCE_NAME   = $INSTANCE_NAME"
log "TARGET_CODE     = $TARGET_CODE"
log "OBSIDIAN_VAULT  = $OBSIDIAN_VAULT"
if [[ "$WEB_ENABLED_IN" = "1" ]]; then
    log "Web UI          = http://localhost:$WEB_PORT (user: $WEB_USER_IN)"
else
    log "Web UI          = desativada"
fi

# ---------------------- 4. Build + up ----------------------
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

# ---------------------- 5. Registra MCP nos arquivos de config ----------------------
# Registra em ~/.mcp.json  (Claude Code CLI / terminal)
# Registra em ~/.claude.json (Claude Code VSCode extension)
# Ambos fazem merge — nunca sobrescrevem entradas de outras instâncias.

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
    # ~/.mcp.json  (Claude Code CLI)
    MCP_JSON="${HOME}/.mcp.json"
    [[ -f "$MCP_JSON" ]] || echo '{"mcpServers":{}}' > "$MCP_JSON"
    _register_mcp "$MCP_JSON" "$MCP_NAME" "$MCP_COMMAND" "$MCP_ARGS" 2
    ok "MCP '${MCP_NAME}' registrado em ${MCP_JSON}."

    # ~/.claude.json  (Claude Code VSCode extension — uses mcpServers at top level)
    CLAUDE_JSON="${HOME}/.claude.json"
    if [[ -f "$CLAUDE_JSON" ]]; then
        _register_mcp "$CLAUDE_JSON" "$MCP_NAME" "$MCP_COMMAND" "$MCP_ARGS" 4
        ok "MCP '${MCP_NAME}' registrado em ${CLAUDE_JSON}."
    fi
fi

ok "Instalação concluída."
cat <<EOF

Próximos passos:
  1. Reinicie o Claude Desktop para carregar o MCP.
  2. Monte seu código em:         $TARGET_CODE
  3. Inspecione o vault em:       $OBSIDIAN_VAULT
EOF
if [[ "$WEB_ENABLED_IN" = "1" ]]; then
cat <<EOF
  4. Abra a UI web:               http://localhost:$WEB_PORT
     (usuário: $WEB_USER_IN)
EOF
fi
cat <<EOF
  5. Logs:                        docker logs -f ${CONTAINER_NAME}
  6. Status:                      docker exec ${CONTAINER_NAME} codeingraph2 health
  7. MCP registrado como:         ${MCP_NAME}
EOF
