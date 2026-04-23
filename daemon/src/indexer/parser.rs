//! Generic tree-sitter walker. Given a file and a [`LangSpec`] (see
//! [`super::languages`]), it produces a flat list of symbols and relations
//! to be upserted by [`crate::indexer`].
//!
//! This is intentionally a *skeleton*: it captures the common denominator of
//! symbol extraction across Rust/Python/JS/TS. Language-specific refinements
//! (generics, decorators, attribute macros, etc.) can be added incrementally
//! by extending the per-language specs in [`super::languages`].

use anyhow::{anyhow, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tree_sitter::{Node, Parser, Tree};

use super::languages::{for_lang, LangSpec};

#[derive(Debug, Clone, Copy, Serialize)]
pub enum SymbolKind {
    File, Class, Function, Method, Variable, Constant, Enum, Trait, Module,
}
impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",       Self::Class    => "class",
            Self::Function => "function", Self::Method  => "method",
            Self::Variable => "variable", Self::Constant=> "constant",
            Self::Enum => "enum",         Self::Trait   => "trait",
            Self::Module => "module",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum RelationKind {
    Calls, Inherits, Imports, References, Contains, Implements, Assigns, Reads,
    UsesType,
}
impl RelationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Calls => "calls",   Self::Inherits  => "inherits",
            Self::Imports=>"imports", Self::References=> "references",
            Self::Contains=>"contains", Self::Implements=>"implements",
            Self::Assigns=>"assigns", Self::Reads     => "reads",
            Self::UsesType => "uses_type",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParsedSymbol {
    pub name: String,
    pub qualified_name: String,
    pub kind: SymbolKind,
    pub signature: Option<String>,
    pub visibility: Option<String>,
    pub docstring: Option<String>,
    pub start_line: i64,
    pub end_line: i64,
    pub start_col: i64,
    pub end_col: i64,
    pub body_hash: Option<String>,
    pub parent_idx: Option<usize>,   // index into Parsed.symbols
}

#[derive(Debug, Clone)]
pub struct ParsedRelation {
    pub source_idx: usize,            // index into Parsed.symbols
    pub target_name: Option<String>,
    pub kind: RelationKind,
    pub line: i64,
    pub col: i64,
    pub weight: f64,
}

pub struct Parsed {
    pub symbols: Vec<ParsedSymbol>,
    pub relations: Vec<ParsedRelation>,
    pub tree: Tree,
}

pub fn parse(lang_key: &str, rel_path: &str, src: &str) -> Result<Parsed> {
    let spec = for_lang(lang_key)
        .ok_or_else(|| anyhow!("unsupported language: {lang_key}"))?;

    let mut parser = Parser::new();
    parser.set_language(&spec.lang)
        .map_err(|e| anyhow!("set_language failed: {e}"))?;
    let tree = parser.parse(src.as_bytes(), None)
        .ok_or_else(|| anyhow!("parse returned None"))?;

    let mut symbols = Vec::new();
    let mut relations = Vec::new();

    // We also push a synthetic "file" symbol as index 0, so relations that
    // live at module scope can still attach to something.
    let root = tree.root_node();
    symbols.push(ParsedSymbol {
        name: rel_path.rsplit('/').next().unwrap_or(rel_path).to_string(),
        qualified_name: rel_path.to_string(),
        kind: SymbolKind::File,
        signature: None,
        visibility: None,
        docstring: None,
        start_line: 1,
        end_line: (src.lines().count() as i64).max(1),
        start_col: 0,
        end_col: 0,
        body_hash: Some(short_hash(src)),
        parent_idx: None,
    });

    walk(&spec, root, src, /*parent_idx=*/Some(0), &mut symbols, &mut relations);

    Ok(Parsed { symbols, relations, tree })
}

fn walk(
    spec: &LangSpec,
    node: Node,
    src: &str,
    parent_idx: Option<usize>,
    symbols: &mut Vec<ParsedSymbol>,
    relations: &mut Vec<ParsedRelation>,
) {
    // Is this node a symbol declaration?
    let mut this_idx = parent_idx;
    if let Some(sn) = spec.symbol_nodes.iter().find(|s| s.node_kind == node.kind()) {
        if let Some(name) = field_text(&node, sn.name_field, src)
            .or_else(|| first_identifier(&node, src))
        {
            let start = node.start_position();
            let end   = node.end_position();
            let body = node.utf8_text(src.as_bytes()).unwrap_or("");
            let sig_line_end = body.find('\n').map(|i| i.min(200)).unwrap_or(body.len().min(200));
            let signature = body.get(..sig_line_end).map(|s| s.trim().to_string());
            let qname = match parent_idx.and_then(|i| symbols.get(i)) {
                Some(p) if matches!(p.kind, SymbolKind::Class | SymbolKind::Module | SymbolKind::Trait)
                    => format!("{}::{name}", p.qualified_name),
                _   => name.clone(),
            };
            symbols.push(ParsedSymbol {
                name,
                qualified_name: qname,
                kind: sn.symbol_kind,
                signature,
                visibility: None,
                docstring: None,
                start_line: (start.row as i64) + 1,
                end_line:   (end.row   as i64) + 1,
                start_col:  start.column as i64,
                end_col:    end.column   as i64,
                body_hash:  Some(short_hash(body)),
                parent_idx,
            });
            this_idx = Some(symbols.len() - 1);

            // A symbol inside another symbol is a "contains" relation.
            if let Some(p) = parent_idx {
                if p != symbols.len() - 1 {
                    relations.push(ParsedRelation {
                        source_idx: p,
                        target_name: Some(symbols.last().unwrap().qualified_name.clone()),
                        kind: RelationKind::Contains,
                        line: (start.row as i64) + 1,
                        col:  start.column as i64,
                        weight: 1.0,
                    });
                }
            }
        }
    }

    // Is this node a relation?
    if let Some(rn) = spec.relation_nodes.iter().find(|r| r.node_kind == node.kind()) {
        let target = rn.target_field
            .and_then(|f| field_text(&node, f, src))
            .or_else(|| first_identifier(&node, src));
        if let Some(tname) = target {
            let pos = node.start_position();
            let src_idx = this_idx.or(parent_idx).unwrap_or(0);
            relations.push(ParsedRelation {
                source_idx: src_idx,
                target_name: Some(tname),
                kind: rn.relation_kind,
                line: (pos.row as i64) + 1,
                col:  pos.column as i64,
                weight: 1.0,
            });
        }
    }

    // Recurse.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(spec, child, src, this_idx, symbols, relations);
    }
}

fn field_text(node: &Node, field: &str, src: &str) -> Option<String> {
    if field.is_empty() { return None; }
    let child = node.child_by_field_name(field)?;
    child.utf8_text(src.as_bytes()).ok().map(|s| s.to_string())
}

fn first_identifier(node: &Node, src: &str) -> Option<String> {
    let mut cursor = node.walk();
    for ch in node.named_children(&mut cursor) {
        if ch.kind().contains("identifier") {
            if let Ok(s) = ch.utf8_text(src.as_bytes()) {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn short_hash(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(&h.finalize()[..8])
}
