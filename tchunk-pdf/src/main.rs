use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rayon::ThreadPool;

use tchunk_pdf::cli::{Cli, TokenizerKind};
use tchunk_pdf::index::{ChunkEntry, Config, Index, Pages, Source, Warning};
use tchunk_pdf::inspect;
use tchunk_pdf::pdf::Pdf;
use tchunk_pdf::plan::{plan_chunks, Boundary, Diagnostic, SplitAt};
use tchunk_pdf::tokenize::{HuggingFaceTokenizer, TiktokenTokenizer, Tokenizer, WordCountTokenizer};

const SCAN_LIKE_TOKEN_THRESHOLD: usize = 20;
const SCAN_LIKE_PAGE_RATIO_PCT: usize = 50;
const IMAGE_DOMINANT_TOKEN_THRESHOLD: usize = 50;

const EXIT_INPUT_ERROR: u8 = 1;
const EXIT_OUTPUT_ERROR: u8 = 3;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(RunError::Input(e)) => {
            eprintln!("error: {e:#}");
            ExitCode::from(EXIT_INPUT_ERROR)
        }
        Err(RunError::Output(e)) => {
            eprintln!("error: {e:#}");
            ExitCode::from(EXIT_OUTPUT_ERROR)
        }
    }
}

enum RunError {
    Input(anyhow::Error),
    Output(anyhow::Error),
}

fn run(mut cli: Cli) -> Result<(), RunError> {
    cli.validate().map_err(RunError::Input)?;

    if cli.inspection_mode() {
        return run_inspect(&cli);
    }

    let tokenizer: Box<dyn Tokenizer + Send + Sync> = match cli.tokenizer {
        TokenizerKind::WordCount => Box::new(WordCountTokenizer),
        TokenizerKind::HuggingFace => {
            let hf = if let Some(path) = cli.tokenizer_file.as_deref() {
                HuggingFaceTokenizer::from_file(path)
            } else if let Some(model_id) = cli.tokenizer_model.as_deref() {
                HuggingFaceTokenizer::from_model_id(model_id)
            } else {
                unreachable!("Cli::validate guarantees one of --tokenizer-file / --tokenizer-model is set");
            };
            Box::new(hf.map_err(RunError::Input)?)
        }
        other => Box::new(TiktokenTokenizer::new(other.as_str()).map_err(RunError::Input)?),
    };

    let pool = build_pool(cli.jobs).map_err(RunError::Input)?;

    let multi = cli.inputs.len() > 1;
    for (idx, input) in cli.inputs.iter().enumerate() {
        if multi {
            if idx > 0 {
                eprintln!();
            }
            eprintln!("=== {} ({}/{}) ===", input.display(), idx + 1, cli.inputs.len());
        }
        process_input(&cli, input, &pool, tokenizer.as_ref())?;
    }
    Ok(())
}

fn run_inspect(cli: &Cli) -> Result<(), RunError> {
    let multi = cli.inputs.len() > 1;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for (idx, input) in cli.inputs.iter().enumerate() {
        if multi {
            if idx > 0 {
                writeln!(out)
                    .map_err(|e| RunError::Output(anyhow::anyhow!(e)))?;
            }
            writeln!(out, "=== {} ({}/{}) ===", input.display(), idx + 1, cli.inputs.len())
                .map_err(|e| RunError::Output(anyhow::anyhow!(e)))?;
        }
        let pdf = Pdf::load(input).map_err(RunError::Input)?;
        if pdf.page_count() == 0 {
            return Err(RunError::Input(anyhow::anyhow!(
                "PDF contains no pages: {}",
                input.display()
            )));
        }
        let entries = pdf.outline_entries();
        let page_count = pdf.page_count();
        if cli.bookmarks_hist {
            inspect::print_histogram(&mut out, &entries, page_count)
                .map_err(|e| RunError::Output(anyhow::anyhow!(e)))?;
        }
        if cli.bookmarks_tree {
            inspect::print_tree(&mut out, &entries, page_count)
                .map_err(|e| RunError::Output(anyhow::anyhow!(e)))?;
        }
    }
    Ok(())
}

fn process_input(
    cli: &Cli,
    input: &Path,
    pool: &ThreadPool,
    tokenizer: &(dyn Tokenizer + Send + Sync),
) -> Result<(), RunError> {
    let pdf = Pdf::load(input).map_err(RunError::Input)?;
    let page_count = pdf.page_count();
    if page_count == 0 {
        return Err(RunError::Input(anyhow::anyhow!(
            "PDF contains no pages: {}",
            input.display()
        )));
    }

    let page_nums = pdf.page_nums();

    let extract_pb = spinner(&format!("Extracting text from {page_count} pages..."));
    let t_extract = Instant::now();
    let texts: Vec<String> = pool.install(|| {
        page_nums
            .par_iter()
            .map(|&n| pdf.page_text(n))
            .collect()
    });
    let extract_elapsed = t_extract.elapsed();
    extract_pb.finish();

    let tok_pb = unit_bar(page_count as u64, "Tokenizing", "pages");
    let t_tokenize = Instant::now();
    let tokens: Vec<usize> = pool.install(|| {
        texts
            .par_iter()
            .map(|t| {
                let n = tokenizer.count(t);
                tok_pb.inc(1);
                n
            })
            .collect()
    });
    let tokenize_elapsed = t_tokenize.elapsed();
    tok_pb.finish();

    let t_images = Instant::now();
    let images: Vec<usize> = pool.install(|| {
        page_nums
            .par_iter()
            .map(|&n| pdf.image_count(n))
            .collect()
    });
    let images_elapsed = t_images.elapsed();

    let mut warnings: Vec<Warning> = Vec::new();
    warnings.extend(emit_content_warnings(input, &tokens, &images, cli.quiet));

    let requested_split_at: SplitAt = cli.resolved_split_at();
    let mut split_at = requested_split_at;
    let mut boundaries = pdf.boundaries();

    if split_at != SplitAt::Page && !pdf.has_outline() {
        if !cli.quiet {
            eprintln!(
                "warning: no outline present in PDF; --split-at {requested_split_at} ignored, falling back to page."
            );
        }
        warnings.push(Warning::OutlineMissing {
            requested: requested_split_at.to_string(),
        });
        split_at = SplitAt::Page;
        boundaries = vec![Boundary::Page; page_count];
    }

    let plan = plan_chunks(&tokens, &boundaries, split_at, cli.max_tokens);

    for diag in &plan.diagnostics {
        match diag {
            Diagnostic::OversizedPage { page, tokens: t } => {
                if !cli.quiet {
                    eprintln!(
                        "warning: page {page} has {t} tokens, which exceeds --max-tokens {}; emitted as its own chunk.",
                        cli.max_tokens
                    );
                }
                warnings.push(Warning::OversizedPage { page: *page, tokens: *t });
            }
        }
    }

    let prefix = match &cli.prefix {
        Some(p) => p.clone(),
        None => input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("chunk")
            .to_string(),
    };

    std::fs::create_dir_all(&cli.output_dir)
        .map_err(|e| RunError::Output(anyhow::anyhow!("failed to create output dir: {e}")))?;

    let total = plan.chunks.len();
    let pad = pad_width(total);

    if cli.verbose {
        eprintln!(
            "tchunk-pdf: {} pages -> {} chunks (budget {} tokens, split-at {split_at}, tokenizer {})",
            page_count,
            total,
            cli.max_tokens,
            tokenizer.name(),
        );
    }

    let write_pb = if cli.verbose {
        ProgressBar::hidden()
    } else {
        unit_bar(total as u64, "Writing chunks", "chunks")
    };

    let t_write = Instant::now();
    let mut chunk_entries: Vec<ChunkEntry> = Vec::with_capacity(total);
    for (i, chunk) in plan.chunks.iter().enumerate() {
        let idx = i + 1;
        let filename = format!("{prefix}_{idx:0width$}.pdf", width = pad);
        let out_path: PathBuf = cli.output_dir.join(&filename);

        let page_nums = &chunk.pages;
        let tok_sum: usize = page_nums
            .iter()
            .map(|&p| tokens[(p - 1) as usize])
            .sum();
        let first = *page_nums.first().unwrap();
        let last = *page_nums.last().unwrap();

        if cli.verbose {
            eprintln!(
                "  {filename}: pages {first}-{last} ({} pages, {tok_sum} tokens, level {})",
                page_nums.len(),
                chunk.effective_level,
            );
        }

        pdf.write_chunk(page_nums, &out_path)
            .map_err(RunError::Output)?;
        write_pb.inc(1);

        chunk_entries.push(ChunkEntry {
            filename,
            pages: Pages {
                start: first,
                end: last,
                count: page_nums.len(),
            },
            token_count: tok_sum,
            effective_level: chunk.effective_level.to_string(),
        });
    }
    write_pb.finish();
    let write_elapsed = t_write.elapsed();

    // split_at_effective reports the finest level actually used across chunks, so a user can
    // see at a glance whether their requested level was honored everywhere (same as requested)
    // or whether any unit had to be recursed to a finer level (shows the finest such level).
    // SplitAt's derived Ord goes coarsest→finest, so .max() picks the finest.
    let effective_level: SplitAt = plan
        .chunks
        .iter()
        .map(|c| c.effective_level)
        .max()
        .unwrap_or(split_at);

    let index = Index {
        tool: "tchunk-pdf",
        version: env!("CARGO_PKG_VERSION"),
        source: Source {
            path: input.display().to_string(),
            page_count,
            total_tokens: tokens.iter().sum(),
        },
        config: Config {
            tokenizer: tokenizer.name().to_string(),
            max_tokens: cli.max_tokens,
            split_at_requested: requested_split_at.to_string(),
            split_at_effective: effective_level.to_string(),
        },
        chunks: chunk_entries,
        warnings,
    };
    let index_path = cli.output_dir.join(format!("{prefix}.index.json"));
    index.write(&index_path).map_err(RunError::Output)?;

    eprintln!(
        "timing: extract {} | tokenize {} | image-scan {} | write {} chunks {}",
        fmt_dur(extract_elapsed),
        fmt_dur(tokenize_elapsed),
        fmt_dur(images_elapsed),
        total,
        fmt_dur(write_elapsed),
    );

    Ok(())
}

fn build_pool(jobs: usize) -> anyhow::Result<ThreadPool> {
    let mut builder = rayon::ThreadPoolBuilder::new();
    if jobs != 0 {
        builder = builder.num_threads(jobs);
    }
    builder
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build thread pool: {e}"))
}

fn fmt_dur(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs >= 60.0 {
        let m = (secs / 60.0).floor() as u64;
        let s = secs - (m as f64) * 60.0;
        format!("{m}m{s:.1}s")
    } else if secs >= 1.0 {
        format!("{secs:.2}s")
    } else {
        format!("{}ms", d.as_millis())
    }
}

fn spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg} [{elapsed}]")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

fn unit_bar(total: u64, label: &str, unit: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    let template = format!(
        "{{msg}} [{{bar:30.cyan/blue}}] {{pos}}/{{len}} {unit} ({{percent}}%) [{{elapsed}}/eta {{eta}}]"
    );
    pb.set_style(
        ProgressStyle::with_template(&template)
            .unwrap()
            .progress_chars("=>-"),
    );
    pb.set_message(label.to_string());
    pb
}

fn pad_width(total: usize) -> usize {
    let natural = total.checked_ilog10().map(|l| l as usize + 1).unwrap_or(1);
    natural.max(3)
}

fn emit_content_warnings(
    input: &std::path::Path,
    tokens: &[usize],
    images: &[usize],
    quiet: bool,
) -> Vec<Warning> {
    let mut out = Vec::new();
    let total_pages = tokens.len();
    if total_pages == 0 {
        return out;
    }
    let near_empty = tokens
        .iter()
        .filter(|&&t| t < SCAN_LIKE_TOKEN_THRESHOLD)
        .count();
    if near_empty * 100 / total_pages >= SCAN_LIKE_PAGE_RATIO_PCT {
        if !quiet {
            eprintln!(
                "warning: {} appears to be scanned/image-based ({}/{} pages have almost no extractable text). \
                 Token-based splitting will not reflect actual content size. Consider running the PDF through \
                 an OCR tool such as ocrmypdf (https://github.com/ocrmypdf/OCRmyPDF) first — it adds a \
                 searchable text layer while preserving the original images.",
                input.display(),
                near_empty,
                total_pages
            );
        }
        out.push(Warning::ScanLike {
            near_empty_pages: near_empty,
            total_pages,
        });
        return out;
    }

    let image_dominant = tokens
        .iter()
        .zip(images.iter())
        .filter(|(&t, &n)| n >= 1 && t < IMAGE_DOMINANT_TOKEN_THRESHOLD)
        .count();
    if image_dominant > 0 {
        if !quiet {
            eprintln!(
                "warning: {} of {} pages contain significant non-text content (images/figures). \
                 Token counts underestimate their size; downstream tools may handle them differently.",
                image_dominant, total_pages
            );
        }
        out.push(Warning::ImageDominant {
            pages_affected: image_dominant,
            total_pages,
        });
    }
    out
}
