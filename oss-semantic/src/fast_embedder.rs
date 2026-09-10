//! Real local embedding inference via the `fastembed` crate (ONNX Runtime,
//! CPU-first, no PyTorch).
//!
//! Enabled by the `fastembed` cargo feature and intended for production
//! builds only: the model weights are **downloaded from the Hugging Face
//! hub on first use** and cached locally. Tests in this crate never enable
//! the feature — they use
//! [`StubEmbedder`](crate::embedding::StubEmbedder) — so `cargo test`
//! touches no network.
//!
//! Suggested model for code search: a code-trained or strong general
//! embedding model such as `NomicEmbedText` (768-dim) — pass the model you
//! want via [`FastEmbedder::try_new`].

use crate::embedding::Embedder;
use crate::{Result, SemanticError};

/// [`Embedder`] implementation backed by `fastembed::TextEmbedding`.
/// The model sits behind a mutex because fastembed's `embed` takes
/// `&mut self` (stateful batching) while [`Embedder::embed`] takes `&self`.
pub struct FastEmbedder {
    model: std::sync::Mutex<fastembed::TextEmbedding>,
    dim: usize,
}

impl FastEmbedder {
    /// Build with full `fastembed` init options (model choice, cache dir,
    /// download progress, ...). The dimensionality is learned by embedding
    /// a probe string once — the same weights serve every later call.
    pub fn try_new(options: fastembed::InitOptions) -> Result<Self> {
        let mut model = fastembed::TextEmbedding::try_new(options)
            .map_err(|e| SemanticError::Embed(format!("fastembed init: {e}")))?;
        let dim = model
            .embed(vec!["probe".to_string()], None)
            .map_err(|e| SemanticError::Embed(format!("fastembed probe: {e}")))?[0]
            .len();
        Ok(Self {
            model: std::sync::Mutex::new(model),
            dim,
        })
    }

    /// Build with the default model (`BGESmallENV15`, 384-dim).
    pub fn default_model() -> Result<Self> {
        Self::try_new(fastembed::InitOptions::new(
            fastembed::EmbeddingModel::BGESmallENV15,
        ))
    }
}

impl Embedder for FastEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut model = self
            .model
            .lock()
            .map_err(|_| SemanticError::Embed("model mutex poisoned".into()))?;
        model
            .embed(texts, None)
            .map_err(|e| SemanticError::Embed(format!("fastembed embed: {e}")))
    }

    fn dim(&self) -> usize {
        self.dim
    }
}
