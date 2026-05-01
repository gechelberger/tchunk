use std::path::PathBuf;

use clap::{ArgGroup, Parser, ValueEnum};
use indexmap::IndexSet;

use crate::plan::SplitAt;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SplitAtArg {
    Page,
    #[value(name = "any-bookmark", alias = "bookmark")]
    AnyBookmark,
    Subsection,
    Section,
    Chapter,
}

impl From<SplitAtArg> for SplitAt {
    fn from(s: SplitAtArg) -> Self {
        match s {
            SplitAtArg::Page => SplitAt::Page,
            SplitAtArg::AnyBookmark => SplitAt::AnyBookmark,
            SplitAtArg::Subsection => SplitAt::Depth(3),
            SplitAtArg::Section => SplitAt::Depth(2),
            SplitAtArg::Chapter => SplitAt::Depth(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TokenizerKind {
    #[value(name = "cl100k_base")]
    Cl100kBase,
    #[value(name = "o200k_base")]
    O200kBase,
    #[value(name = "word_count")]
    WordCount,
    #[value(name = "huggingface")]
    HuggingFace,
}

impl TokenizerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenizerKind::Cl100kBase => "cl100k_base",
            TokenizerKind::O200kBase => "o200k_base",
            TokenizerKind::WordCount => "word_count",
            TokenizerKind::HuggingFace => "huggingface",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "tchunk-pdf",
    about = "Split a PDF into smaller PDFs along structural boundaries (chapter cuts by default) under a token budget.",
    version,
    group(
        ArgGroup::new("hf_source")
            .args(["tokenizer_file", "tokenizer_model"])
            .multiple(false)
            .required(false),
    ),
    group(
        ArgGroup::new("split_target")
            .args(["split_at", "split_at_depth"])
            .multiple(false)
            .required(false),
    ),
)]
pub struct Cli {
    /// Input PDF file(s). Each is chunked independently; outputs for each input use
    /// that input's own stem as the filename prefix.
    #[arg(required = true, num_args = 1..)]
    pub inputs: Vec<PathBuf>,

    /// Target maximum tokens per output chunk.
    #[arg(short = 'm', long, default_value_t = 500_000)]
    pub max_tokens: usize,

    /// Coarsest level at which a split between chunks is allowed. Outline-based levels
    /// require the PDF to have a bookmarks tree;
    /// otherwise they fall back to `page` with a warning. Mutually exclusive with
    /// `--split-at-depth`.
    #[arg(short = 's', long, value_enum, default_value_t = SplitAtArg::Chapter)]
    pub split_at: SplitAtArg,

    /// Coarsest outline depth at which a split is allowed. Equivalent to `--split-at chapter`
    /// at depth 1, `--split-at section` at depth 2, etc., but lets you target depths beyond
    /// the named flags (e.g. `--split-at-depth 4` for a deeply-nested outline). Mutually
    /// exclusive with `--split-at`.
    #[arg(long = "split-at-depth", value_name = "N")]
    pub split_at_depth: Option<u32>,

    /// Output directory (created if missing).
    #[arg(short = 'o', long, default_value = ".")]
    pub output_dir: PathBuf,

    /// Output filename prefix. Defaults to the input file's stem. Cannot be used
    /// when more than one input is given — rerun per file, or omit this flag.
    #[arg(short = 'p', long)]
    pub prefix: Option<String>,

    /// Tokenizer used to count tokens per page.
    #[arg(short = 't', long, value_enum, default_value_t = TokenizerKind::WordCount)]
    pub tokenizer: TokenizerKind,

    /// Path to a HuggingFace tokenizer.json. Requires `-t huggingface`.
    #[arg(long = "tokenizer-file", value_name = "PATH")]
    pub tokenizer_file: Option<PathBuf>,

    /// HuggingFace Hub model ID (e.g. `meta-llama/Llama-3-8B`) to fetch the tokenizer from.
    /// First use downloads and caches under $HF_HOME (default ~/.cache/huggingface);
    /// subsequent runs hit the cache. Requires `-t huggingface`.
    #[arg(long = "tokenizer-model", value_name = "HF_MODEL_ID")]
    pub tokenizer_model: Option<String>,

    /// Print per-chunk page ranges and token totals to stderr.
    #[arg(short = 'v', long, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Suppress warnings to stderr. Hard errors are still printed. Warnings remain
    /// recorded in the index sidecar.
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Worker threads for per-page extract/tokenize/image-scan. `1` = sequential; `0` =
    /// auto-detect (use all available cores). Chunk writing stays sequential.
    #[arg(short = 'j', long, default_value_t = 1)]
    pub jobs: usize,
}

impl Cli {
    /// Resolve the user's split-at request to a `SplitAt`. `--split-at-depth N` takes
    /// precedence over the named `--split-at` flag when both are supplied (clap's
    /// ArgGroup ensures they aren't, but be explicit anyway).
    pub fn resolved_split_at(&self) -> SplitAt {
        match self.split_at_depth {
            Some(n) => SplitAt::Depth(n),
            None => self.split_at.into(),
        }
    }

    /// Post-parse validation that clap can't express declaratively. Also expands
    /// any glob patterns in `inputs` (shells on Windows don't glob, so the tool does).
    pub fn validate(&mut self) -> anyhow::Result<()> {
        self.expand_inputs()?;

        if self.prefix.is_some() && self.inputs.len() > 1 {
            anyhow::bail!(
                "--prefix is ambiguous with multiple inputs ({} given); omit it so each input uses its own stem, or run tchunk-pdf once per file.",
                self.inputs.len()
            );
        }

        if let Some(p) = self.prefix.as_deref() {
            validate_prefix(p)?;
        }

        let has_hf_source = self.tokenizer_file.is_some() || self.tokenizer_model.is_some();
        match self.tokenizer {
            TokenizerKind::HuggingFace if !has_hf_source => anyhow::bail!(
                "-t huggingface requires --tokenizer-file <PATH> or --tokenizer-model <HF_MODEL_ID>"
            ),
            TokenizerKind::HuggingFace => {}
            _ if has_hf_source => anyhow::bail!(
                "--tokenizer-file / --tokenizer-model only apply with -t huggingface"
            ),
            _ => {}
        }

        Ok(())
    }

    /// Expand any glob patterns in `self.inputs`. Literal paths pass through.
    /// Rule: if the arg exists as a literal path, keep it as-is (protects
    /// filenames that legally contain `[`/`*`/`?` on Unix). Else, if it
    /// contains glob metacharacters, expand via `glob::glob`. Else, leave it
    /// and let downstream loading produce the normal "not found" error.
    ///
    /// Zero matches for a pattern is a hard error. Duplicates across patterns
    /// are deduped, preserving first-seen arg order.
    fn expand_inputs(&mut self) -> anyhow::Result<()> {
        let mut out: IndexSet<PathBuf> = IndexSet::with_capacity(self.inputs.len());

        for arg in self.inputs.drain(..) {
            if arg.exists() {
                out.insert(arg);
                continue;
            }

            let s = arg.to_string_lossy();
            let is_pattern = s.contains(['*', '?', '[']);
            if !is_pattern {
                out.insert(arg);
                continue;
            }

            let paths = glob::glob(&s)
                .map_err(|e| anyhow::anyhow!("invalid glob pattern {s:?}: {e}"))?;
            let mut matched = 0usize;
            for entry in paths {
                let p = entry.map_err(|e| {
                    anyhow::anyhow!("error while expanding pattern {s:?}: {e}")
                })?;
                out.insert(p);
                matched += 1;
            }
            if matched == 0 {
                anyhow::bail!("no files matched pattern: {s}");
            }
        }

        self.inputs = out.into_iter().collect();
        Ok(())
    }
}

/// Reject prefixes that could let output files escape `--output-dir`. A prefix is used
/// verbatim as the leading component of generated filenames, so it must be a single path
/// component with no separators or other path-meaningful characters.
fn validate_prefix(p: &str) -> anyhow::Result<()> {
    if p.is_empty() {
        anyhow::bail!("--prefix must not be empty");
    }
    if p == "." || p == ".." {
        anyhow::bail!("--prefix must not be {p:?}");
    }
    if let Some(c) = p.chars().find(|&c| matches!(c, '/' | '\\' | ':' | '\0')) {
        anyhow::bail!(
            "--prefix must not contain path separators, ':' or NUL (got {p:?}, found {c:?})"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_prefixes_accepted() {
        assert!(validate_prefix("book").is_ok());
        assert!(validate_prefix("my-document").is_ok());
        assert!(validate_prefix("chapter_01").is_ok());
        assert!(validate_prefix(".dotfile").is_ok());
        assert!(validate_prefix("name with spaces").is_ok());
    }

    #[test]
    fn empty_prefix_rejected() {
        assert!(validate_prefix("").is_err());
    }

    #[test]
    fn dot_and_dotdot_rejected() {
        assert!(validate_prefix(".").is_err());
        assert!(validate_prefix("..").is_err());
    }

    #[test]
    fn separator_rejected() {
        assert!(validate_prefix("foo/bar").is_err());
        assert!(validate_prefix("foo\\bar").is_err());
        assert!(validate_prefix("../escape").is_err());
        assert!(validate_prefix("/absolute").is_err());
        assert!(validate_prefix("\\absolute").is_err());
    }

    #[test]
    fn colon_rejected() {
        // Windows interprets these as drive-relative or alternate-data-stream paths,
        // either of which can land outside --output-dir.
        assert!(validate_prefix("C:foo").is_err());
        assert!(validate_prefix("file:stream").is_err());
    }

    #[test]
    fn nul_rejected() {
        assert!(validate_prefix("foo\0bar").is_err());
    }

    #[test]
    fn split_at_and_split_at_depth_are_mutually_exclusive() {
        use clap::Parser;
        let result = Cli::try_parse_from([
            "tchunk-pdf",
            "input.pdf",
            "--split-at",
            "chapter",
            "--split-at-depth",
            "2",
        ]);
        assert!(result.is_err(), "expected ArgGroup conflict, got: {:?}", result);
    }

    #[test]
    fn split_at_depth_resolves_to_depth_variant() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["tchunk-pdf", "input.pdf", "--split-at-depth", "5"])
            .expect("parse");
        assert_eq!(cli.resolved_split_at(), SplitAt::Depth(5));
    }

    #[test]
    fn split_at_chapter_resolves_to_depth_1() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["tchunk-pdf", "input.pdf", "--split-at", "chapter"])
            .expect("parse");
        assert_eq!(cli.resolved_split_at(), SplitAt::Depth(1));
    }

    #[test]
    fn split_at_any_bookmark_resolves_to_anybookmark() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["tchunk-pdf", "input.pdf", "--split-at", "any-bookmark"])
            .expect("parse");
        assert_eq!(cli.resolved_split_at(), SplitAt::AnyBookmark);
    }

    #[test]
    fn default_split_at_is_chapter_depth_1() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["tchunk-pdf", "input.pdf"]).expect("parse");
        assert_eq!(cli.resolved_split_at(), SplitAt::Depth(1));
    }
}
