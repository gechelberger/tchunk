use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

#[derive(Serialize)]
pub struct Index {
    pub tool: &'static str,
    pub version: &'static str,
    pub source: Source,
    pub config: Config,
    pub chunks: Vec<ChunkEntry>,
    pub warnings: Vec<Warning>,
}

#[derive(Serialize)]
pub struct Source {
    pub path: String,
    pub page_count: usize,
    pub total_tokens: usize,
}

#[derive(Serialize)]
pub struct Config {
    pub tokenizer: String,
    pub max_tokens: usize,
    pub split_at_requested: String,
    pub split_at_effective: String,
}

#[derive(Serialize)]
pub struct ChunkEntry {
    pub filename: String,
    pub pages: Pages,
    pub token_count: usize,
    pub effective_level: String,
}

#[derive(Serialize)]
pub struct Pages {
    pub start: u32,
    pub end: u32,
    pub count: usize,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Warning {
    OutlineMissing {
        requested: String,
    },
    OversizedPage {
        page: u32,
        tokens: usize,
    },
    ScanLike {
        near_empty_pages: usize,
        total_pages: usize,
    },
    ImageDominant {
        pages_affected: usize,
        total_pages: usize,
    },
}

impl Index {
    pub fn write(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .context("failed to serialize index metadata")?;
        std::fs::write(path, json)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_minimal_index() {
        let idx = Index {
            tool: "tchunk-pdf",
            version: "0.1.0",
            source: Source {
                path: "book.pdf".to_string(),
                page_count: 10,
                total_tokens: 1234,
            },
            config: Config {
                tokenizer: "cl100k_base".to_string(),
                max_tokens: 500_000,
                split_at_requested: "page".to_string(),
                split_at_effective: "page".to_string(),
            },
            chunks: vec![ChunkEntry {
                filename: "book_001.pdf".to_string(),
                pages: Pages { start: 1, end: 10, count: 10 },
                token_count: 1234,
                effective_level: "page".to_string(),
            }],
            warnings: vec![],
        };
        let json = serde_json::to_string(&idx).unwrap();
        assert!(json.contains("\"tool\":\"tchunk-pdf\""));
        assert!(json.contains("\"pages\":{\"start\":1,\"end\":10,\"count\":10}"));
        assert!(json.contains("\"warnings\":[]"));
    }

    #[test]
    fn warnings_serialize_with_kind_tag() {
        let w = Warning::OversizedPage { page: 42, tokens: 999 };
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(json, r#"{"kind":"oversized_page","page":42,"tokens":999}"#);
    }

    #[test]
    fn outline_missing_serializes_requested() {
        let w = Warning::OutlineMissing { requested: "depth-1".to_string() };
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(json, r#"{"kind":"outline_missing","requested":"depth-1"}"#);
    }
}
