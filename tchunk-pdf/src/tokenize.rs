use std::path::Path;

use anyhow::Result;
use tiktoken_rs::CoreBPE;
use tokenizers::{FromPretrainedParameters, Tokenizer as HfTokenizer};

pub trait Tokenizer {
    fn count(&self, text: &str) -> usize;
    fn name(&self) -> &str;
}

pub struct TiktokenTokenizer {
    bpe: CoreBPE,
    name: &'static str,
}

impl TiktokenTokenizer {
    pub fn new(name: &str) -> Result<Self> {
        match name {
            "cl100k_base" => Ok(Self {
                bpe: tiktoken_rs::cl100k_base()?,
                name: "cl100k_base",
            }),
            "o200k_base" => Ok(Self {
                bpe: tiktoken_rs::o200k_base()?,
                name: "o200k_base",
            }),
            other => anyhow::bail!("unknown tokenizer: {other} (supported: cl100k_base, o200k_base)"),
        }
    }
}

impl Tokenizer for TiktokenTokenizer {
    fn count(&self, text: &str) -> usize {
        self.bpe.encode_ordinary(text).len()
    }

    fn name(&self) -> &str {
        self.name
    }
}

pub struct HuggingFaceTokenizer {
    inner: HfTokenizer,
    name: String,
}

impl HuggingFaceTokenizer {
    pub fn from_file(path: &Path) -> Result<Self> {
        let inner = HfTokenizer::from_file(path).map_err(|e| {
            anyhow::anyhow!("failed to load tokenizer from {}: {e}", path.display())
        })?;
        let basename = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into());
        Ok(Self {
            inner,
            name: format!("huggingface:{basename}"),
        })
    }

    pub fn from_model_id(model_id: &str) -> Result<Self> {
        let params = hf_auth_token().map(|token| FromPretrainedParameters {
            token: Some(token),
            ..Default::default()
        });
        let inner = HfTokenizer::from_pretrained(model_id, params).map_err(|e| {
            anyhow::anyhow!("failed to fetch tokenizer for model '{model_id}': {e}")
        })?;
        Ok(Self {
            inner,
            name: format!("huggingface:{model_id}"),
        })
    }
}

/// Auth token for HuggingFace Hub. Precedence matches the Python `huggingface_hub`
/// library: `HF_TOKEN` env var (preferred) → `HUGGING_FACE_HUB_TOKEN` env var
/// (legacy) → the cached token file written by `huggingface-cli login`
/// (at `$HF_HOME/token`, default `~/.cache/huggingface/token`). Empty values at
/// any level are treated as absent so the next source is tried.
fn hf_auth_token() -> Option<String> {
    for var in ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"] {
        if let Ok(val) = std::env::var(var) {
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    hf_hub::Cache::default().token()
}

impl Tokenizer for HuggingFaceTokenizer {
    fn count(&self, text: &str) -> usize {
        self.inner
            .encode(text, false)
            .expect("huggingface tokenizer failed to encode text")
            .len()
    }

    fn name(&self) -> &str {
        &self.name
    }
}

pub struct WordCountTokenizer;

impl Tokenizer for WordCountTokenizer {
    fn count(&self, text: &str) -> usize {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .count()
    }

    fn name(&self) -> &str {
        "word_count"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_count_empty() {
        assert_eq!(WordCountTokenizer.count(""), 0);
    }

    #[test]
    fn word_count_whitespace_only() {
        assert_eq!(WordCountTokenizer.count("   \t\n  "), 0);
    }

    #[test]
    fn word_count_punctuation_only() {
        assert_eq!(WordCountTokenizer.count("!!! --- ,,,"), 0);
    }

    #[test]
    fn word_count_plain_words() {
        assert_eq!(WordCountTokenizer.count("the quick brown fox"), 4);
    }

    #[test]
    fn word_count_punctuation_between_words() {
        assert_eq!(WordCountTokenizer.count("hello,world"), 2);
    }

    #[test]
    fn word_count_leading_and_trailing_punctuation() {
        assert_eq!(WordCountTokenizer.count("...hello, world!"), 2);
    }

    #[test]
    fn word_count_collapses_runs_of_whitespace_and_punctuation() {
        assert_eq!(WordCountTokenizer.count("one  ,  two -- three"), 3);
    }

    #[test]
    fn word_count_numbers_are_words() {
        assert_eq!(WordCountTokenizer.count("page 42 of 100"), 4);
    }

    #[test]
    fn word_count_apostrophe_splits_contractions() {
        // "don't" has an apostrophe, which is non-alphanumeric, so it splits
        // into "don" + "t". Pinning this down: the tokenizer is intentionally
        // naive and doesn't special-case apostrophes.
        assert_eq!(WordCountTokenizer.count("don't"), 2);
    }

    #[test]
    fn word_count_name_is_word_count() {
        assert_eq!(WordCountTokenizer.name(), "word_count");
    }
}
