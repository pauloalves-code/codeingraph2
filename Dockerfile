# syntax=docker/dockerfile:1.6
# codeingraph2 — multi-stage build
# Stage 1 (builder): compile Rust daemon + mcp_server
# Stage 2 (runtime): minimal debian-slim with sqlite3

############################
# Stage 1 — builder
############################
FROM rust:1.85-slim AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config build-essential libssl-dev cmake git clang \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache dependencies first (Cargo.toml / Cargo.lock) to speed iterative builds.
COPY daemon/Cargo.toml ./Cargo.toml
COPY daemon/Cargo.lock* ./
RUN mkdir -p src src/bin \
    && echo 'fn main() {}'     > src/main.rs \
    && echo 'fn main() {}'     > src/bin/mcp_server.rs \
    && echo '// cache stub'    > src/lib.rs \
    && cargo build --release --bins \
    && rm -rf src target/release/deps/codeingraph2* target/release/deps/mcp_server*

# Actual source.
COPY daemon/src        ./src
COPY daemon/static     ./static
COPY daemon/migrations ./migrations
COPY templates         /templates
RUN cargo build --release --locked --bins

############################
# Stage 2 — runtime
############################
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libssl3 sqlite3 tini \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/codeingraph2 /usr/local/bin/codeingraph2
COPY --from=builder /build/target/release/mcp_server  /usr/local/bin/mcp_server
COPY --from=builder /templates                        /opt/codeingraph2/templates
COPY --from=builder /build/migrations                 /opt/codeingraph2/migrations

RUN mkdir -p /var/lib/codeingraph2 /target_code /obsidian_vault

WORKDIR /opt/codeingraph2

ENV CODEINGRAPH2_DB=/var/lib/codeingraph2/graph.db \
    CODEINGRAPH2_TARGET=/target_code \
    CODEINGRAPH2_VAULT=/obsidian_vault \
    CODEINGRAPH2_TEMPLATES=/opt/codeingraph2/templates \
    CODEINGRAPH2_MIGRATIONS=/opt/codeingraph2/migrations \
    WEB_BIND=0.0.0.0:7890 \
    RUST_LOG=info

EXPOSE 7890

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/codeingraph2"]
CMD ["daemon"]
