//! The embedding model: one table lookup per token, then mean-pool.
//!
//! **There is no forward pass here, and that is not a shortcut.** potion-base-8M is
//! a model2vec *static* model — its `config.json` says `"model_type": "model2vec",
//! "architectures": ["StaticModel"]`, and its `model.safetensors` holds exactly one
//! tensor, `embeddings` of shape [29528, 256]. No attention weights exist in the
//! file to run. The C++ tree's `kernels.cpp` (267 lines of matmul, softmax,
//! layernorm and an AVX2 dispatch) and `encoder.cpp` (a 12-layer BERT block loop)
//! served a *second* backend, bge-small-en-v1.5, reachable only via
//! `CML_EMBED_BACKEND=encoder`.
//!
//! That backend does not survive the port, for the reason its own author wrote in
//! `embedder.cpp`: it embeds a corpus in ninety minutes instead of seconds, it
//! produces 384-dim vectors where the stored ones are 256-dim, and running it once
//! silently disabled semantic search until `embed --all` was re-run. A 133 MB
//! download and 770 lines of hand-vectorised linear algebra existed to make the
//! results *worse*. Two backends also meant an `Embedder` facade over them; with
//! one, that abstraction goes too.
//!
//! `CML_EMBED_MODEL` still selects the model, so a different model2vec checkpoint
//! (of any width) drops in.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::wordpiece::WordPiece;
use crate::R;

/// model2vec's `encode()` default.
const MAX_TOKENS: usize = 512;

/// A safetensors JSON header larger than this is a corrupt length field, not a
/// header — without the cap it becomes a multi-gigabyte allocation.
const MAX_HEADER: usize = 16 << 20;

pub fn model_id() -> String {
    match std::env::var("CML_EMBED_MODEL") {
        Ok(m) if !m.is_empty() => m,
        _ => "minishlab/potion-base-8M".to_string(),
    }
}

/// A repo id (`minishlab/potion-base-8M`) or a plain directory, resolved against
/// the HuggingFace cache. Nothing is fetched: a machine without the model gets
/// keyword-only search, not a surprise 30 MB download inside a session hook.
fn model_dir(id: &str) -> Option<PathBuf> {
    let direct = Path::new(id);
    if direct.is_dir() {
        return Some(direct.to_path_buf());
    }
    let hub = match std::env::var_os("HF_HOME") {
        Some(h) => PathBuf::from(h).join("hub"),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".cache/huggingface/hub"),
    };
    let flat = format!("models--{}", id.replace('/', "--"));
    std::fs::read_dir(hub.join(flat).join("snapshots"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.join("model.safetensors").is_file())
}

/// safetensors: a u64 little-endian header length, a JSON header naming every
/// tensor, then packed data. serde_json reads the header; only the `embeddings`
/// tensor's bytes are ever touched, so the format needs no crate.
///
/// The C++ mmap'd the file because the *encoder's* 133 MB of weights would not fit
/// a 3.6 GB laptop otherwise. 30 MB reads outright, which costs one `Vec` and buys
/// back the tree's `unsafe`.
fn read_embeddings(path: &Path) -> R<(Vec<f32>, usize)> {
    let mut f = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;

    let mut len = [0u8; 8];
    f.read_exact(&mut len).map_err(|_| format!("unreadable safetensors: {}", path.display()))?;
    let header_len = u64::from_le_bytes(len) as usize;
    if header_len == 0 || header_len > MAX_HEADER {
        return Err(format!("corrupt safetensors header in {}", path.display()).into());
    }
    let mut header = vec![0u8; header_len];
    f.read_exact(&mut header)
        .map_err(|_| format!("truncated safetensors header in {}", path.display()))?;
    let header: serde_json::Value = serde_json::from_slice(&header)
        .map_err(|e| format!("unparsable safetensors header in {}: {e}", path.display()))?;

    let t = header.get("embeddings").ok_or_else(|| {
        format!("{} has no 'embeddings' tensor — not a model2vec model", path.display())
    })?;
    // Reading a bf16 checkpoint as floats produces plausible-looking garbage, which
    // is worse than refusing it.
    if t.get("dtype").and_then(|v| v.as_str()) != Some("F32") {
        return Err("embeddings dtype is not F32 — convert the checkpoint to float32".into());
    }
    let nums = |key: &str| -> Vec<usize> {
        t.get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|n| n.as_u64()).map(|n| n as usize).collect())
            .unwrap_or_default()
    };
    let shape = nums("shape");
    let offsets = nums("data_offsets");
    if shape.len() != 2 || offsets.len() != 2 || offsets[1] < offsets[0] {
        return Err("unexpected embeddings shape".into());
    }
    let (rows, dim) = (shape[0], shape[1]);
    let need = rows
        .checked_mul(dim)
        .and_then(|n| n.checked_mul(4))
        .ok_or("embeddings shape overflows")?;
    if dim == 0 || offsets[1] - offsets[0] != need {
        return Err("embeddings tensor does not match its declared shape".into());
    }

    f.seek(SeekFrom::Start((8 + header_len + offsets[0]) as u64))?;
    let mut raw = vec![0u8; need];
    f.read_exact(&mut raw)
        .map_err(|_| format!("embeddings tensor runs past the end of {}", path.display()))?;
    // No transmute: the file is little-endian by spec and this is one pass over
    // 30 MB, well under the cost of the tokenisation it feeds.
    let matrix = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok((matrix, dim))
}

pub struct Model {
    tok: WordPiece,
    /// Row-major [vocab x dim]. Indexed by token id; a row is one token's vector.
    matrix: Vec<f32>,
    dim: usize,
    normalize: bool,
    id: String,
}

impl Model {
    pub fn load() -> R<Self> {
        let id = model_id();
        let dir = model_dir(&id).ok_or_else(|| {
            format!(
                "embedding model '{id}' not found in the HuggingFace cache — \
                 fetch it once with network access"
            )
        })?;
        let tok = WordPiece::load(&dir.join("tokenizer.json"))?;
        let (matrix, dim) = read_embeddings(&dir.join("model.safetensors"))?;
        // config.json decides whether vectors are L2-normalised; potion says yes.
        let normalize = std::fs::read_to_string(dir.join("config.json"))
            .ok()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
            .and_then(|c| c.get("normalize").and_then(|v| v.as_bool()))
            .unwrap_or(true);
        Ok(Self { tok, matrix, dim, normalize, id })
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Tokenise, look up one row per token, mean-pool, optionally L2-normalise.
    ///
    /// [UNK] is dropped *before* truncation, matching model2vec: a run of unknown
    /// words must not consume the token budget of the real ones behind it.
    pub fn encode(&self, text: &str) -> Vec<f32> {
        // model2vec pre-truncates by characters, budgeting the median vocabulary
        // token length per token, so a megabyte of transcript is not tokenised in
        // full only to keep its first 512 tokens.
        let budget = MAX_TOKENS * self.tok.median_token_bytes;
        let clipped = match text.char_indices().nth(budget) {
            Some((i, _)) => &text[..i],
            None => text,
        };

        let unk = self.tok.unk();
        let mut ids = self.tok.encode(clipped);
        ids.retain(|&id| id != unk);
        ids.truncate(MAX_TOKENS);

        let mut sum = vec![0.0f32; self.dim];
        let mut pooled = 0usize;
        for id in ids {
            let at = id as usize * self.dim;
            // Doubles as the vocab/matrix mismatch guard the C++ spelled out.
            let Some(row) = self.matrix.get(at..at + self.dim) else { continue };
            for (s, r) in sum.iter_mut().zip(row) {
                *s += r;
            }
            pooled += 1;
        }
        let denom = pooled.max(1) as f32;
        for s in &mut sum {
            *s /= denom;
        }

        if self.normalize {
            let norm = sum.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
            for s in &mut sum {
                *s /= norm;
            }
        }
        sum
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_ids_map_to_the_huggingface_cache_layout() {
        // The real directory on this machine is
        // ~/.cache/huggingface/hub/models--minishlab--potion-base-8M/snapshots/<sha>/
        assert_eq!(
            format!("models--{}", "minishlab/potion-base-8M".replace('/', "--")),
            "models--minishlab--potion-base-8M"
        );
    }

    /// Absent the model, every caller must degrade rather than fail — `load`
    /// returning `Err` is what `vector::search` turns into an empty result.
    #[test]
    fn a_missing_model_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join("cml-no-such-model");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(model_dir(dir.to_str().unwrap()).is_none());
    }
}
