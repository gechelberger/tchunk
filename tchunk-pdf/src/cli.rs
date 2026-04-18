use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use crate::plan::BoundaryLevel;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SplitAt {
    Page,
    #[value(name = "any-bookmark", alias = "bookmark")]
    AnyBookmark,
    Subsection,
    Section,
    Chapter,
}

impl From<SplitAt> for BoundaryLevel {
    fn from(s: SplitAt) -> Self {
        match s {
            SplitAt::Page => BoundaryLevel::Page,
            SplitAt::AnyBookmark => BoundaryLevel::AnyBookmark,
            SplitAt::Subsection => BoundaryLevel::Subsection,
            SplitAt::Section => BoundaryLevel::Section,
            SplitAt::Chapter => BoundaryLevel::Chapter,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TokenizerKind {
    #[value(name = "cl100k_base")]
    Cl100kBase,
    #[value(name = "o200k_base")]
    O200kBase,
}

impl TokenizerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenizerKind::Cl100kBase => "cl100k_base",
            TokenizerKind::O200kBase => "o200k_base",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "tchunk-pdf",
    about = "Split a PDF into smaller PDFs at page boundaries under a token budget.",
    version
)]
pub struct Cli {
    /// Input PDF file.
    pub input: PathBuf,

    /// Target maximum tokens per output chunk.
    #[arg(short = 'm', long, default_value_t = 500_000)]
    pub max_tokens: usize,

    /// Coarsest level at which a split between chunks is allowed. Outline-based levels
    /// (chapter/section/subsection/any-bookmark) require the PDF to have a bookmarks tree;
    /// otherwise they fall back to `page` with a warning.
    #[arg(short = 's', long, value_enum, default_value_t = SplitAt::Page)]
    pub split_at: SplitAt,

    /// Output directory (created if missing).
    #[arg(short = 'o', long, default_value = ".")]
    pub output_dir: PathBuf,

    /// Output filename prefix. Defaults to the input file's stem.
    #[arg(short = 'p', long)]
    pub prefix: Option<String>,

    /// Tokenizer used to count tokens per page.
    #[arg(short = 't', long, value_enum, default_value_t = TokenizerKind::Cl100kBase)]
    pub tokenizer: TokenizerKind,

    /// Print per-chunk page ranges and token totals to stderr.
    #[arg(short = 'v', long)]
    pub verbose: bool,
}
