use anyhow::{Context, Result, anyhow};
use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MODEL_ID: &str = "intfloat/e5-small-v2";
pub const DIM: usize = 384;

pub struct Embedder {
    model: Mutex<TextEmbedding>,
}

impl Embedder {
    pub fn load() -> Result<Self> {
        let dir = find_model_dir().ok_or_else(|| {
            anyhow!("e5-small-v2 model files were not found. Expected assets/models/e5-small-v2.")
        })?;
        let onnx =
            std::fs::read(dir.join("model_quantized.onnx")).context("read model_quantized.onnx")?;
        let tokens = TokenizerFiles {
            tokenizer_file: std::fs::read(dir.join("tokenizer.json"))
                .context("read tokenizer.json")?,
            config_file: std::fs::read(dir.join("config.json")).context("read config.json")?,
            special_tokens_map_file: std::fs::read(dir.join("special_tokens_map.json"))
                .context("read special_tokens_map.json")?,
            tokenizer_config_file: std::fs::read(dir.join("tokenizer_config.json"))
                .context("read tokenizer_config.json")?,
        };
        let model = UserDefinedEmbeddingModel::new(onnx, tokens).with_pooling(Pooling::Mean);
        let options = InitOptionsUserDefined::default()
            .with_max_length(512)
            .with_intra_threads(4);
        let model = TextEmbedding::try_new_from_user_defined(model, options)
            .context("initialize e5-small-v2")?;
        Ok(Self {
            model: Mutex::new(model),
        })
    }

    pub fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_with_prefix(texts, "passage: ")
    }

    pub fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed_with_prefix(&[text.to_string()], "query: ")?;
        out.pop().ok_or_else(|| anyhow!("empty embedding"))
    }

    fn embed_with_prefix(&self, texts: &[String], prefix: &str) -> Result<Vec<Vec<f32>>> {
        let inputs: Vec<String> = texts
            .iter()
            .map(|t| {
                // E5 expects a single prefix; strip any the caller added.
                let trimmed = t
                    .strip_prefix("passage: ")
                    .or_else(|| t.strip_prefix("query: "))
                    .unwrap_or(t);
                format!("{prefix}{trimmed}")
            })
            .collect();
        let mut model = self
            .model
            .lock()
            .map_err(|_| anyhow!("embedding model lock poisoned"))?;
        let mut vectors = model
            .embed(inputs, Some(16))
            .context("embed texts with e5-small-v2")?;
        for v in &mut vectors {
            normalize(v);
        }
        Ok(vectors)
    }
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v {
            *x /= norm;
        }
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    a[..len].iter().zip(&b[..len]).map(|(x, y)| x * y).sum()
}

fn find_model_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(dir) = std::env::var("SHORTLIST_MODEL_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/models/e5-small-v2"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("assets/models/e5-small-v2"));
            candidates.push(dir.join("../Resources/models/e5-small-v2"));
        }
    }
    candidates.push(shortlist_core::models_dir().join("e5-small-v2"));
    candidates
        .into_iter()
        .find(|dir| dir.join("model_quantized.onnx").exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "loads the bundled 32MB e5-small-v2 model"]
    fn loads_and_embeds() {
        let embedder = Embedder::load().expect("load e5-small-v2");
        let query = embedder
            .embed_query("rust backend engineer")
            .expect("embed query");
        assert_eq!(query.len(), DIM);
        let passages = embedder
            .embed_passages(&["Built payment systems in Rust".to_string()])
            .expect("embed passage");
        assert_eq!(passages[0].len(), DIM);
        assert!(cosine(&query, &passages[0]) > 0.0);
    }
}
