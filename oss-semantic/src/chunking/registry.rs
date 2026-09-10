//! Language support registry.
//!
//! Adding a grammar is: add the grammar crate to `Cargo.toml`, write one
//! `LanguageSupport` entry in [`registry()`], done. Unknown languages never
//! reach tree-sitter — [`crate::chunking`] sends them to the fixed sliding
//! window instead.

use std::sync::OnceLock;

/// One supported language: its tree-sitter grammar plus the per-language
/// knobs the chunker needs.
pub struct LanguageSupport {
    /// Canonical language name (lowercase), e.g. `"rust"`.
    pub name: &'static str,
    /// File extensions (without the dot) that map to this language.
    pub extensions: &'static [&'static str],
    /// The compiled tree-sitter grammar.
    pub language: tree_sitter::Language,
    /// Node kinds that become chunks when they appear at the top level of
    /// the tree (function / class / impl shaped items).
    pub chunk_kinds: &'static [&'static str],
    /// Top-level node kinds whose *children* may contain chunk kinds; used
    /// for wrappers like `export_statement` (js/ts) and `declaration` (c).
    pub peek_kinds: &'static [&'static str],
    /// Line-comment prefixes for doc-comment attachment, e.g. `"//"`.
    pub line_comments: &'static [&'static str],
    /// Attribute prefixes for attachment, e.g. `"#["` (rust) or `"@"` (ts).
    pub attribute_prefixes: &'static [&'static str],
}

impl LanguageSupport {
    /// Fresh parser configured with this grammar.
    pub fn parser(&self) -> Result<tree_sitter::Parser, tree_sitter::LanguageError> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&self.language)?;
        Ok(parser)
    }
}

/// The registry of supported languages.
pub fn registry() -> &'static [LanguageSupport] {
    static REGISTRY: OnceLock<Vec<LanguageSupport>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        vec![
            LanguageSupport {
                name: "rust",
                extensions: &["rs"],
                language: tree_sitter_rust::LANGUAGE.into(),
                chunk_kinds: &[
                    "function_item",
                    "struct_item",
                    "enum_item",
                    "trait_item",
                    "impl_item",
                ],
                peek_kinds: &[],
                line_comments: &["//"],
                attribute_prefixes: &["#["],
            },
            LanguageSupport {
                name: "python",
                extensions: &["py", "pyi"],
                language: tree_sitter_python::LANGUAGE.into(),
                chunk_kinds: &["function_definition", "class_definition"],
                peek_kinds: &[],
                line_comments: &["#"],
                attribute_prefixes: &["@"],
            },
            LanguageSupport {
                name: "javascript",
                extensions: &["js", "jsx", "mjs", "cjs"],
                language: tree_sitter_javascript::LANGUAGE.into(),
                chunk_kinds: &[
                    "function_declaration",
                    "class_declaration",
                    "abstract_class_declaration",
                ],
                peek_kinds: &["export_statement"],
                line_comments: &["//"],
                attribute_prefixes: &["@"],
            },
            LanguageSupport {
                name: "typescript",
                extensions: &["ts"],
                language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                chunk_kinds: &[
                    "function_declaration",
                    "class_declaration",
                    "abstract_class_declaration",
                    "interface_declaration",
                    "enum_declaration",
                ],
                peek_kinds: &["export_statement"],
                line_comments: &["//"],
                attribute_prefixes: &["@"],
            },
            LanguageSupport {
                name: "tsx",
                extensions: &["tsx"],
                language: tree_sitter_typescript::LANGUAGE_TSX.into(),
                chunk_kinds: &[
                    "function_declaration",
                    "class_declaration",
                    "abstract_class_declaration",
                    "interface_declaration",
                    "enum_declaration",
                ],
                peek_kinds: &["export_statement"],
                line_comments: &["//"],
                attribute_prefixes: &["@"],
            },
            LanguageSupport {
                name: "go",
                extensions: &["go"],
                language: tree_sitter_go::LANGUAGE.into(),
                chunk_kinds: &[
                    "function_declaration",
                    "method_declaration",
                    "type_declaration",
                ],
                peek_kinds: &[],
                line_comments: &["//"],
                attribute_prefixes: &[],
            },
            LanguageSupport {
                name: "c",
                extensions: &["c", "h"],
                language: tree_sitter_c::LANGUAGE.into(),
                chunk_kinds: &[
                    "function_definition",
                    "struct_specifier",
                    "enum_specifier",
                    "union_specifier",
                ],
                peek_kinds: &["declaration", "linkage_specification"],
                line_comments: &["//"],
                attribute_prefixes: &[],
            },
        ]
    })
}

/// Look up support by canonical language name.
pub fn by_name(name: &str) -> Option<&'static LanguageSupport> {
    registry().iter().find(|l| l.name == name)
}

/// Look up support by file extension (without the leading dot).
pub fn by_extension(ext: &str) -> Option<&'static LanguageSupport> {
    let ext = ext.trim_start_matches('.');
    registry().iter().find(|l| l.extensions.contains(&ext))
}
