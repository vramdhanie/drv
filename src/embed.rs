use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

/// BGE queries score better with this instruction prefix; documents are
/// embedded bare. (Asymmetric retrieval, per the model card.)
const QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    /// Loads (downloading on first use, ~130 MB) the local embedding model.
    /// Everything runs on-device; nothing is sent to any service.
    /// `threads` caps ONNX's CPU use — the default would take every core.
    pub fn load(threads: usize) -> Result<Self> {
        let cache = dirs::cache_dir()
            .context("no cache directory")?
            .join("drv")
            .join("models");
        std::fs::create_dir_all(&cache)?;
        let model = TextEmbedding::try_new(
            TextInitOptions::new(EmbeddingModel::BGESmallENV15)
                .with_cache_dir(cache)
                .with_show_download_progress(true)
                .with_intra_threads(threads.max(1)),
        )
        .context("loading embedding model")?;
        Ok(Self { model })
    }

    pub fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.model.embed(texts, None).context("embedding documents")
    }

    pub fn embed_query(&mut self, query: &str) -> Result<Vec<f32>> {
        let mut out = self
            .model
            .embed(&[format!("{QUERY_PREFIX}{query}")], None)
            .context("embedding query")?;
        Ok(out.remove(0))
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}
