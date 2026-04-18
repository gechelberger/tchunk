use anyhow::Result;
use tiktoken_rs::CoreBPE;

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
