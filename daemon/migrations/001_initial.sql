-- codeingraph2 initial schema
-- Supports the 3D correlation matrix:
--   X = symbol (file | class | function | method | variable | ...)
--   Y = relation kind (calls | imports | inherits | references | contains | assigns)
--   Z = physical line number
--
-- All tables use INTEGER PK for fast joins; foreign keys cascade on file deletion
-- so re-indexing a file is idempotent (DELETE files.row -> symbols/relations vanish).

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous  = NORMAL;

------------------------------------------------------------------
-- schema_meta: versioning for migrations
------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS schema_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR REPLACE INTO schema_meta (key, value) VALUES ('version', '1');

------------------------------------------------------------------
-- files: one row per indexed source file
------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS files (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    path         TEXT    NOT NULL UNIQUE,   -- relative to CODEINGRAPH2_TARGET
    language     TEXT    NOT NULL,          -- rust | python | javascript | typescript | ...
    hash         TEXT    NOT NULL,          -- sha256 of file content
    line_count   INTEGER NOT NULL DEFAULT 0,
    size_bytes   INTEGER NOT NULL DEFAULT 0,
    last_indexed INTEGER NOT NULL           -- unix seconds
);
CREATE INDEX IF NOT EXISTS idx_files_language ON files(language);
CREATE INDEX IF NOT EXISTS idx_files_hash     ON files(hash);

------------------------------------------------------------------
-- symbols (X axis): declared entities
------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS symbols (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    file_id           INTEGER NOT NULL,
    parent_symbol_id  INTEGER,               -- for methods inside classes, nested fns
    name              TEXT    NOT NULL,
    qualified_name    TEXT    NOT NULL,      -- e.g. "foo::Bar::baz"
    kind              TEXT    NOT NULL,      -- file|class|function|method|variable|constant|enum|trait|module
    signature         TEXT,                  -- "fn foo(a: i32) -> Result<()>"
    visibility        TEXT,                  -- public|private|protected|pub|pub(crate)|...
    docstring         TEXT,
    start_line        INTEGER NOT NULL,
    end_line          INTEGER NOT NULL,
    start_col         INTEGER NOT NULL DEFAULT 0,
    end_col           INTEGER NOT NULL DEFAULT 0,
    body_hash         TEXT,                  -- sha256 of symbol source, for change detection
    FOREIGN KEY (file_id)          REFERENCES files(id)   ON DELETE CASCADE,
    FOREIGN KEY (parent_symbol_id) REFERENCES symbols(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_symbols_name     ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_qname    ON symbols(qualified_name);
CREATE INDEX IF NOT EXISTS idx_symbols_kind     ON symbols(kind);
CREATE INDEX IF NOT EXISTS idx_symbols_file     ON symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_parent   ON symbols(parent_symbol_id);
CREATE INDEX IF NOT EXISTS idx_symbols_file_ln  ON symbols(file_id, start_line, end_line);

------------------------------------------------------------------
-- relations (Y axis × Z axis): edges between symbols with line info
------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS relations (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    source_symbol_id   INTEGER NOT NULL,
    target_symbol_id   INTEGER,              -- null = unresolved / external
    target_name        TEXT,                 -- used when target_symbol_id is null
    relation_kind      TEXT    NOT NULL,     -- calls|inherits|imports|references|contains|implements|assigns|reads
    line               INTEGER NOT NULL,     -- Z: where the relation occurs
    col                INTEGER NOT NULL DEFAULT 0,
    weight             REAL    NOT NULL DEFAULT 1.0,
    FOREIGN KEY (source_symbol_id) REFERENCES symbols(id) ON DELETE CASCADE,
    FOREIGN KEY (target_symbol_id) REFERENCES symbols(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_relations_src    ON relations(source_symbol_id);
CREATE INDEX IF NOT EXISTS idx_relations_dst    ON relations(target_symbol_id);
CREATE INDEX IF NOT EXISTS idx_relations_name   ON relations(target_name);
CREATE INDEX IF NOT EXISTS idx_relations_kind   ON relations(relation_kind);
CREATE INDEX IF NOT EXISTS idx_relations_line   ON relations(line);
CREATE INDEX IF NOT EXISTS idx_relations_matrix ON relations(source_symbol_id, relation_kind, line);

------------------------------------------------------------------
-- line_index: fast "what is on line N of file F?" lookup
-- (materialised Z projection of the matrix)
------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS line_index (
    file_id         INTEGER NOT NULL,
    line            INTEGER NOT NULL,
    symbol_id       INTEGER,
    relation_count  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (file_id, line),
    FOREIGN KEY (file_id)   REFERENCES files(id)   ON DELETE CASCADE,
    FOREIGN KEY (symbol_id) REFERENCES symbols(id) ON DELETE SET NULL
);

------------------------------------------------------------------
-- impact_scores: cached fan-in / fan-out / centrality per symbol.
-- get_surgical_context uses these to prioritise what to return.
------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS impact_scores (
    symbol_id    INTEGER PRIMARY KEY,
    fan_in       INTEGER NOT NULL DEFAULT 0,    -- how many symbols depend on me
    fan_out      INTEGER NOT NULL DEFAULT 0,    -- how many symbols I depend on
    centrality   REAL    NOT NULL DEFAULT 0.0,  -- simple betweenness proxy
    updated_at   INTEGER NOT NULL,
    FOREIGN KEY (symbol_id) REFERENCES symbols(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_impact_fanin  ON impact_scores(fan_in DESC);
CREATE INDEX IF NOT EXISTS idx_impact_fanout ON impact_scores(fan_out DESC);

------------------------------------------------------------------
-- convenience view: symbols + their file path + impact score
------------------------------------------------------------------
CREATE VIEW IF NOT EXISTS v_symbols_rich AS
SELECT
    s.id, s.name, s.qualified_name, s.kind, s.signature,
    s.start_line, s.end_line,
    f.path        AS file_path,
    f.language    AS language,
    COALESCE(i.fan_in,  0) AS fan_in,
    COALESCE(i.fan_out, 0) AS fan_out,
    COALESCE(i.centrality, 0.0) AS centrality
FROM symbols s
JOIN files f           ON f.id = s.file_id
LEFT JOIN impact_scores i ON i.symbol_id = s.id;
