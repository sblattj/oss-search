//! Code-aware chunking: tree-sitter function/class/impl extraction with a
//! sliding-window fallback.
//!
//! The cascade (research note 21): if the language has a registered grammar,
//! chunk top-level function/class/impl-shaped items (signature + body, with
//! adjacent doc comments and attributes attached); otherwise — or when a
//! parse yields no items at all — fall back to fixed 60-line windows with a
//! 10-line overlap. Tree-sitter's GLR error recovery means broken or
//! partial code still produces usable subtrees: nodes containing `ERROR`
//! children are emitted anyway, since downstream embeddings tolerate noise.

pub mod registry;
pub mod window;

pub use registry::{LanguageSupport, by_extension, by_name};

use crate::{Result, SemanticError};

/// What kind of code unit a [`Chunk`] captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    /// A function, method, or procedure.
    Function,
    /// A class.
    Class,
    /// A struct.
    Struct,
    /// An enum.
    Enum,
    /// A trait.
    Trait,
    /// An impl block.
    Impl,
    /// A type or interface declaration.
    Interface,
    /// A named type alias or type declaration.
    Type,
    /// A fallback sliding-window slice.
    Window,
}

impl ChunkKind {
    /// Stable lowercase name (used as metadata and in the SQLite store).
    pub fn as_str(self) -> &'static str {
        match self {
            ChunkKind::Function => "function",
            ChunkKind::Class => "class",
            ChunkKind::Struct => "struct",
            ChunkKind::Enum => "enum",
            ChunkKind::Trait => "trait",
            ChunkKind::Impl => "impl",
            ChunkKind::Interface => "interface",
            ChunkKind::Type => "type",
            ChunkKind::Window => "window",
        }
    }
}

/// One chunk of source code: a complete function/class/impl item (or a
/// fallback window), with a 1-based inclusive line span inside its file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Chunk {
    /// Stable identifier, currently `"{path}:{start}-{end}"`.
    pub id: String,
    /// Repo-relative file path this chunk came from.
    pub path: String,
    /// The kind of code unit captured.
    pub kind: ChunkKind,
    /// The item's symbol name when extractable (e.g. `fibonacci`, `Greeter`).
    pub symbol: Option<String>,
    /// 1-based first line (after doc-comment attachment).
    pub start_line: usize,
    /// 1-based last line, inclusive.
    pub end_line: usize,
    /// Full chunk text: doc comments/attributes + signature + body.
    pub text: String,
}

/// Map a tree-sitter node kind to a [`ChunkKind`].
fn kind_for_node(kind: &str) -> Option<ChunkKind> {
    Some(match kind {
        "function_item"
        | "function_definition"
        | "function_declaration"
        | "method_definition"
        | "method_declaration" => ChunkKind::Function,
        "class_definition" | "class_declaration" | "abstract_class_declaration" => ChunkKind::Class,
        "struct_item" | "struct_specifier" => ChunkKind::Struct,
        "enum_item" | "enum_specifier" | "enum_declaration" => ChunkKind::Enum,
        "trait_item" => ChunkKind::Trait,
        "impl_item" => ChunkKind::Impl,
        "interface_declaration" => ChunkKind::Interface,
        "type_declaration" => ChunkKind::Type,
        _ => return None,
    })
}

/// Chunk `source` in the language named `lang` (see [`registry`] for
/// supported names). Unknown languages — or parses that yield no items —
/// fall back to [`window`] chunking.
pub fn chunk_source(source: &str, lang: &str, path: &str) -> Vec<Chunk> {
    match by_name(lang) {
        Some(support) => {
            let chunks = tree_sitter_chunks(source, support, path).unwrap_or_default();
            if chunks.is_empty() {
                window::window_chunks(source, path)
            } else {
                chunks
            }
        }
        None => window::window_chunks(source, path),
    }
}

/// Chunk `source` by inferring the language from a file extension.
pub fn chunk_by_extension(source: &str, ext: &str, path: &str) -> Vec<Chunk> {
    match by_extension(ext) {
        Some(support) => chunk_source(source, support.name, path),
        None => window::window_chunks(source, path),
    }
}

/// Extract chunks via the registered grammar.
fn tree_sitter_chunks(source: &str, support: &LanguageSupport, path: &str) -> Result<Vec<Chunk>> {
    let mut parser = support
        .parser()
        .map_err(|e| SemanticError::Parse(format!("{}: {e}", support.name)))?;
    let tree = parser.parse(source, None).ok_or_else(|| {
        SemanticError::Parse(format!("{}: parser returned no tree", support.name))
    })?;

    let lines: Vec<&str> = source.lines().collect();
    let mut chunks = Vec::new();
    let root = tree.root_node();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if support.chunk_kinds.contains(&child.kind()) {
            emit_chunk(&mut chunks, child, source, &lines, support, path);
        } else if support.peek_kinds.contains(&child.kind()) {
            // Wrapper node (export_statement, declaration, ...): its direct
            // children may hold the real item.
            let mut inner = child.walk();
            for grandchild in child.children(&mut inner) {
                if support.chunk_kinds.contains(&grandchild.kind()) {
                    emit_chunk(&mut chunks, grandchild, source, &lines, support, path);
                }
            }
        }
    }
    Ok(chunks)
}

/// Record one chunk for `node`, attaching adjacent doc comments.
fn emit_chunk(
    chunks: &mut Vec<Chunk>,
    node: tree_sitter::Node<'_>,
    source: &str,
    lines: &[&str],
    support: &LanguageSupport,
    path: &str,
) {
    let node_start_row = node.start_position().row;
    let end_row = node.end_position().row.min(lines.len().saturating_sub(1));
    if end_row < node_start_row {
        return;
    }
    let start_row = attach_start_row(lines, node_start_row, support);
    let text = lines[start_row..=end_row].join("\n");
    chunks.push(Chunk {
        id: format!("{path}:{}-{}", start_row + 1, end_row + 1),
        path: path.to_string(),
        kind: kind_for_node(node.kind()).unwrap_or(ChunkKind::Window),
        symbol: symbol_for_node(node, source, 0),
        start_line: start_row + 1,
        end_line: end_row + 1,
        text,
    });
}

/// Walk upward from the item's first line, absorbing contiguous doc-comment
/// lines, block-comment tails, and attribute lines. Stops at the first
/// blank or code line, so an item without adjacent docs is unchanged.
fn attach_start_row(lines: &[&str], node_start_row: usize, support: &LanguageSupport) -> usize {
    let mut row = node_start_row;
    while row > 0 {
        let line = lines[row - 1].trim();
        let is_line_comment = support.line_comments.iter().any(|p| line.starts_with(p));
        let is_attribute = support
            .attribute_prefixes
            .iter()
            .any(|p| line.starts_with(p));
        let is_block_tail = line == "*/" || line.ends_with("*/") || line.starts_with('*');
        if is_line_comment || is_attribute || is_block_tail {
            row -= 1;
        } else {
            break;
        }
    }
    row
}

/// Best-effort symbol name for a chunk node: the `name` field; the `type`
/// field for rust impl blocks; then one level of descent into children
/// (go type specs, c declarations). Returns `None` when the grammar
/// doesn't expose a usable name (e.g. C `function_definition`).
fn symbol_for_node(node: tree_sitter::Node<'_>, source: &str, depth: u8) -> Option<String> {
    let accept = |text: &str| -> Option<String> {
        let text = text.trim();
        (!text.is_empty() && text.len() <= 128 && !text.contains(char::is_whitespace))
            .then(|| text.to_string())
    };
    if let Some(child) = node.child_by_field_name("name")
        && let Ok(text) = child.utf8_text(source.as_bytes())
        && let Some(sym) = accept(text)
    {
        return Some(sym);
    }
    // rust impl blocks name the implemented type, not a "name".
    if node.kind() == "impl_item"
        && let Some(child) = node.child_by_field_name("type")
        && let Ok(text) = child.utf8_text(source.as_bytes())
        && let Some(sym) = accept(text)
    {
        return Some(sym);
    }
    if depth == 0 {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(child) = child.child_by_field_name("name")
                && let Ok(text) = child.utf8_text(source.as_bytes())
                && let Some(sym) = accept(text)
            {
                return Some(sym);
            }
        }
    }
    None
}
