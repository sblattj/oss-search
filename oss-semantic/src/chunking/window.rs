//! Fixed sliding-window fallback for unknown languages or files where
//! parsing yields nothing usable. Windows are 60 lines with a 10-line
//! overlap (step 50), per the research-backed cascade in note 21.

use super::{Chunk, ChunkKind};

/// Window size in lines.
pub const WINDOW_LINES: usize = 60;
/// Overlap between consecutive windows in lines.
pub const WINDOW_OVERLAP: usize = 10;

/// Chunk `source` into fixed windows of [`WINDOW_LINES`] lines with
/// [`WINDOW_OVERLAP`] lines of overlap. 1-based inclusive line spans.
/// Empty input yields no chunks.
pub fn window_chunks(source: &str, path: &str) -> Vec<Chunk> {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let step = WINDOW_LINES - WINDOW_OVERLAP;
    let mut chunks = Vec::new();
    let mut start = 0; // 0-based line index
    while start < lines.len() {
        let end = (start + WINDOW_LINES).min(lines.len());
        let text = lines[start..end].join("\n");
        let first = start + 1; // 1-based
        let last = end; // end is exclusive => 1-based inclusive end
        chunks.push(Chunk {
            id: format!("{path}:{first}-{last}"),
            path: path.to_string(),
            kind: ChunkKind::Window,
            symbol: None,
            start_line: first,
            end_line: last,
            text,
        });
        if end == lines.len() {
            break;
        }
        start += step;
    }
    chunks
}
