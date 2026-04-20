#!/usr/bin/env bash
# ============================================================================
# codeingraph2 — Global installer
# ============================================================================
# 1. Verifica Docker.
# 2. Pergunta porta, usuário e senha da UI web (a menos que passados via flag).
# 3. Escreve .env para o docker-compose consumir (WEB_PORT, WEB_USER, WEB_AUTH).
# 4. Sobe o container codeingraph2_container.
# 5. Registra o servidor MCP no claude_desktop_config.json do host.
# 6. (Opcional) Registra também via `claude mcp add-json`.
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
EOF
}

# ---------------------- defaults / args ----------------------
TARGET_CODE="${CODEINGRAPH2_TARGET:-}"
OBSIDIAN_VAULT="${CODEINGRAPH2_VAULT:-}"
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

# ---------------------- claude config path ----------------------
case "$(uname -s)" in
    Linux)   CLAUDE_CFG="${CLAUDE_DESKTOP_CONFIG:-$HOME/.config/Claude/claude_desktop_config.json}" ;;
    Darwin)  CLAUDE_CFG="${CLAUDE_DESKTOP_CONFIG:-$HOME/Library/Application Support/Claude/claude_desktop_config.json}" ;;
    MINGW*|MSYS*|CYGWIN*) CLAUDE_CFG="${CLAUDE_DESKTOP_CONFIG:-$APPDATA/Claude/claude_desktop_config.json}" ;;
    *)       CLAUDE_CFG="${CLAUDE_DESKTOP_CONFIG:-$HOME/.config/Claude/claude_desktop_config.json}" ;;
esac

MCP_NAME="codeingraph2"
MCP_COMMAND="docker"
MCP_ARGS='["exec","-i","codeingraph2_container","/usr/local/bin/mcp_server"]'

# ---------------------- uninstall path ----------------------
if [[ $DO_UNINSTALL -eq 1 ]]; then
    log "Desinstalando..."
    (cd "$SCRIPT_DIR" && docker compose down 2>/dev/null || true)
    if [[ -f "$CLAUDE_CFG" ]]; then
        python3 - "$CLAUDE_CFG" "$MCP_NAME" <<'PY' || true
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
    command -v claude >/dev/null 2>&1 && claude mcp remove -s user "$MCP_NAME" 2>/dev/null || true
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

    # Hash = sha256(salt || password), ambos hex.
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
        # fallback openssl
        HASH_HEX="$(printf '%s' "$WEB_PASS_IN" | ( { printf '%b' "$(echo "$SALT_HEX" | sed 's/../\\x&/g')"; cat; } | openssl dgst -sha256 -hex | awk '{print $NF}'))"
    fi
    # Colon-separated ("sha256:<salt>:<hash>") to avoid docker-compose
    # variable interpolation colliding with '$'.
    WEB_AUTH_VAL="sha256:${SALT_HEX}:${HASH_HEX}"
else
    WEB_PORT="${WEB_PORT:-7890}"
    WEB_USER_IN=""
    WEB_AUTH_VAL=""
fi

# ---------------------- 3. .env for docker-compose ----------------------
ENV_FILE="$SCRIPT_DIR/.env"
cat > "$ENV_FILE" <<EOF
# Auto-gerado por install_global.sh em $(date -u +%Y-%m-%dT%H:%M:%SZ)
TARGET_CODE=$TARGET_CODE
OBSIDIAN_VAULT=$OBSIDIAN_VAULT
WEB_ENABLED=$WEB_ENABLED_IN
WEB_PORT=$WEB_PORT
WEB_USER=$WEB_USER_IN
WEB_AUTH=$WEB_AUTH_VAL
EOF
chmod 600 "$ENV_FILE"
ok "Escreveu $ENV_FILE (modo 600)."

mkdir -p "$TARGET_CODE" "$OBSIDIAN_VAULT"
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
    (cd "$SCRIPT_DIR" && docker compose build)
fi
if [[ $DO_START -eq 1 ]]; then
    log "Subindo container codeingraph2_container..."
    (cd "$SCRIPT_DIR" && docker compose up -d)
fi

# ---------------------- 5. Registra MCP no claude_desktop_config ----------------------
mkdir -p "$(dirname "$CLAUDE_CFG")"
[[ -f "$CLAUDE_CFG" ]] || echo '{"mcpServers":{}}' > "$CLAUDE_CFG"

if ! command -v python3 >/dev/null 2>&1; then
    warn "python3 não encontrado — adicione manualmente em $CLAUDE_CFG:"
    cat <<JSON
  "mcpServers": {
    "$MCP_NAME": { "command": "$MCP_COMMAND", "args": $MCP_ARGS }
  }
JSON
else
    python3 - "$CLAUDE_CFG" "$MCP_NAME" "$MCP_COMMAND" "$MCP_ARGS" <<'PY'
import json, sys
path, name, cmd, args_json = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
args = json.loads(args_json)
try:
    with open(path) as f: cfg = json.load(f)
except Exception:
    cfg = {}
cfg.setdefault("mcpServers", {})
cfg["mcpServers"][name] = {"command": cmd, "args": args}
with open(path, "w") as f: json.dump(cfg, f, indent=2)
print(f"registered '{name}' -> {cmd} {' '.join(args)} in {path}")
PY
fi

# ---------------------- 6. Claude Code CLI (opcional) ----------------------
if command -v claude >/dev/null 2>&1; then
    log "Registrando também via 'claude mcp add-json' (Claude Code CLI)..."
    claude mcp add-json -s user "$MCP_NAME" \
        "{\"command\":\"$MCP_COMMAND\",\"args\":$MCP_ARGS}" 2>/dev/null \
        && ok "Registrado no Claude Code." \
        || warn "Falhou (talvez já exista). Verifique com: claude mcp list"
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
  5. Logs:                        docker logs -f codeingraph2_container
  6. Status:                      docker exec codeingraph2_container codeingraph2 health
EOF
