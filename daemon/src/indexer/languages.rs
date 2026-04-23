//! Language-specific tree-sitter setup: returns the Language + a small set of
//! node-kind -> (SymbolKind, RelationKind) mappings used by the generic walker.
//!
//! This is intentionally a best-effort skeleton. Each language declares:
//!   - which node kinds introduce a *symbol* (and how to extract its name)
//!   - which node kinds represent a *relation* (and which field holds the target)
//!
//! Real-world parsers handle many more cases; this is enough for the pipeline
//! to produce a meaningful graph across rust/python/js/ts on day one.

use super::parser::{RelationKind, SymbolKind};

pub struct LangSpec {
    pub key: &'static str,
    pub lang: tree_sitter::Language,
    pub symbol_nodes: &'static [SymbolNode],
    pub relation_nodes: &'static [RelationNode],
}

pub struct SymbolNode {
    pub node_kind: &'static str,
    pub name_field: &'static str,   // field name holding the identifier
    pub symbol_kind: SymbolKind,
}

pub struct RelationNode {
    pub node_kind: &'static str,
    /// If set, read this field for the target identifier; else use first
    /// child with kind `identifier`.
    pub target_field: Option<&'static str>,
    pub relation_kind: RelationKind,
}

pub fn for_lang(key: &str) -> Option<LangSpec> {
    match key {
        "rust"       => Some(rust()),
        "python"     => Some(python()),
        "javascript" => Some(javascript()),
        "typescript" => Some(typescript()),
        _ => None,
    }
}

fn rust() -> LangSpec {
    LangSpec {
        key: "rust",
        lang: tree_sitter_rust::language(),
        symbol_nodes: &[
            SymbolNode { node_kind: "function_item",     name_field: "name", symbol_kind: SymbolKind::Function },
            SymbolNode { node_kind: "struct_item",       name_field: "name", symbol_kind: SymbolKind::Class    },
            SymbolNode { node_kind: "enum_item",         name_field: "name", symbol_kind: SymbolKind::Enum     },
            SymbolNode { node_kind: "trait_item",        name_field: "name", symbol_kind: SymbolKind::Trait    },
            SymbolNode { node_kind: "impl_item",         name_field: "type", symbol_kind: SymbolKind::Class    },
            SymbolNode { node_kind: "mod_item",          name_field: "name", symbol_kind: SymbolKind::Module   },
            SymbolNode { node_kind: "const_item",        name_field: "name", symbol_kind: SymbolKind::Constant },
            SymbolNode { node_kind: "static_item",       name_field: "name", symbol_kind: SymbolKind::Variable },
        ],
        relation_nodes: &[
            RelationNode { node_kind: "call_expression",  target_field: Some("function"), relation_kind: RelationKind::Calls },
            RelationNode { node_kind: "use_declaration",  target_field: None,             relation_kind: RelationKind::Imports },
            RelationNode { node_kind: "macro_invocation", target_field: Some("macro"),    relation_kind: RelationKind::Calls },
        ],
    }
}

fn python() -> LangSpec {
    LangSpec {
        key: "python",
        lang: tree_sitter_python::language(),
        symbol_nodes: &[
            SymbolNode { node_kind: "function_definition", name_field: "name", symbol_kind: SymbolKind::Function },
            SymbolNode { node_kind: "class_definition",    name_field: "name", symbol_kind: SymbolKind::Class    },
        ],
        relation_nodes: &[
            RelationNode { node_kind: "call",              target_field: Some("function"), relation_kind: RelationKind::Calls },
            RelationNode { node_kind: "import_statement",  target_field: None,             relation_kind: RelationKind::Imports },
            RelationNode { node_kind: "import_from_statement", target_field: None,         relation_kind: RelationKind::Imports },
        ],
    }
}

fn javascript() -> LangSpec {
    LangSpec {
        key: "javascript",
        lang: tree_sitter_javascript::language(),
        symbol_nodes: &[
            SymbolNode { node_kind: "function_declaration", name_field: "name", symbol_kind: SymbolKind::Function },
            SymbolNode { node_kind: "class_declaration",    name_field: "name", symbol_kind: SymbolKind::Class    },
            SymbolNode { node_kind: "method_definition",    name_field: "name", symbol_kind: SymbolKind::Method   },
            SymbolNode { node_kind: "lexical_declaration",  name_field: "",     symbol_kind: SymbolKind::Variable },
        ],
        relation_nodes: &[
            RelationNode { node_kind: "call_expression",   target_field: Some("function"), relation_kind: RelationKind::Calls },
            RelationNode { node_kind: "import_statement",  target_field: Some("source"),   relation_kind: RelationKind::Imports },
            // Each named import specifier: import { Foo, Bar } from '…'
            // target_field "name" covers aliased imports (Foo as F); first_identifier fallback covers plain (Foo).
            RelationNode { node_kind: "import_specifier",  target_field: Some("name"),     relation_kind: RelationKind::UsesType },
        ],
    }
}

fn typescript() -> LangSpec {
    LangSpec {
        key: "typescript",
        lang: tree_sitter_typescript::language_typescript(),
        symbol_nodes: &[
            SymbolNode { node_kind: "function_declaration",  name_field: "name", symbol_kind: SymbolKind::Function },
            SymbolNode { node_kind: "class_declaration",     name_field: "name", symbol_kind: SymbolKind::Class    },
            SymbolNode { node_kind: "method_definition",     name_field: "name", symbol_kind: SymbolKind::Method   },
            SymbolNode { node_kind: "interface_declaration", name_field: "name", symbol_kind: SymbolKind::Trait    },
            SymbolNode { node_kind: "type_alias_declaration",name_field: "name", symbol_kind: SymbolKind::Class    },
        ],
        relation_nodes: &[
            RelationNode { node_kind: "call_expression",  target_field: Some("function"), relation_kind: RelationKind::Calls },
            RelationNode { node_kind: "import_statement", target_field: Some("source"),   relation_kind: RelationKind::Imports },
            // Each named import specifier: import { Foo } / import type { Foo } / import { type Foo }
            // Works for aliases too: import { Foo as F } captures "Foo" via the "name" field.
            RelationNode { node_kind: "import_specifier", target_field: Some("name"),     relation_kind: RelationKind::UsesType },
        ],
    }
}
