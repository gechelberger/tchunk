use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rayon::ThreadPool;

use tchunk_pdf::cli::{Cli, TokenizerKind};
use tchunk_pdf::pdf::Pdf;
use tchunk_pdf::plan::{plan_chunks, BoundaryLevel, Diagnostic};
use tchunk_pdf::tokenize::{TiktokenTokenizer, Tokenizer, WordCountTokenizer};

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

fn run(cli: Cli) -> Result<(), RunError> {
    let pdf = Pdf::load(&cli.input).map_err(RunError::Input)?;
    let page_count = pdf.page_count();
    if page_count == 0 {
        return Err(RunError::Input(anyhow::anyhow!(
            "PDF contains no pages: {}",
            cli.input.display()
        )));
    }

    let tokenizer: Box<dyn Tokenizer + Send + Sync> = match cli.tokenizer {
        TokenizerKind::WordCount => Box::new(WordCountTokenizer),
        other => Box::new(TiktokenTokenizer::new(other.as_str()).map_err(RunError::Input)?),
    };

    let pool = build_pool(cli.jobs).map_err(RunError::Input)?;
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

    let tok_pb = page_bar(page_count as u64, "Tokenizing");
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

    emit_content_warnings(&cli.input, &tokens, &images);

    let mut split_at: BoundaryLevel = cli.split_at.into();
    let mut boundaries = pdf.boundaries();

    if split_at > BoundaryLevel::Page && !pdf.has_outline() {
        eprintln!(
            "warning: no outline present in PDF; --split-at {:?} ignored, falling back to page.",
            cli.split_at
        );
        split_at = BoundaryLevel::Page;
        boundaries = vec![BoundaryLevel::Page; page_count];
    }

    let plan = plan_chunks(&tokens, &boundaries, split_at, cli.max_tokens);

    for diag in &plan.diagnostics {
        match diag {
            Diagnostic::OversizedPage { page, tokens: t } => {
                eprintln!(
                    "warning: page {page} has {t} tokens, which exceeds --max-tokens {}; emitted as its own chunk.",
                    cli.max_tokens
                );
            }
            Diagnostic::ForcedMidLevelCut { after_page } => {
                eprintln!(
                    "warning: forced page-level cut after page {after_page} — no allowed cut at --split-at level was reachable within budget."
                );
            }
        }
    }

    let prefix = match cli.prefix {
        Some(p) => p,
        None => cli
            .input
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
            "tchunk-pdf: {} pages -> {} chunks (budget {} tokens, split-at {:?}, tokenizer {})",
            page_count,
            total,
            cli.max_tokens,
            split_at,
            tokenizer.name(),
        );
    }

    let write_pb = if cli.verbose {
        ProgressBar::hidden()
    } else {
        chunk_bar(total as u64, "Writing chunks")
    };

    let t_write = Instant::now();
    for (i, page_nums) in plan.chunks.iter().enumerate() {
        let idx = i + 1;
        let filename = format!("{prefix}_{idx:0width$}.pdf", width = pad);
        let out_path: PathBuf = cli.output_dir.join(&filename);

        if cli.verbose {
            let tok_sum: usize = page_nums
                .iter()
                .map(|&p| tokens[(p - 1) as usize])
                .sum();
            let first = *page_nums.first().unwrap();
            let last = *page_nums.last().unwrap();
            eprintln!(
                "  {filename}: pages {first}-{last} ({} pages, {tok_sum} tokens)",
                page_nums.len()
            );
        }

        pdf.write_chunk(page_nums, &out_path)
            .map_err(RunError::Output)?;
        write_pb.inc(1);
    }
    write_pb.finish();
    let write_elapsed = t_write.elapsed();

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

fn page_bar(total: u64, label: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "{msg} [{bar:30.cyan/blue}] {pos}/{len} pages ({percent}%) [{elapsed}/eta {eta}]",
        )
        .unwrap()
        .progress_chars("=>-"),
    );
    pb.set_message(label.to_string());
    pb
}

fn chunk_bar(total: u64, label: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "{msg} [{bar:30.cyan/blue}] {pos}/{len} chunks ({percent}%) [{elapsed}/eta {eta}]",
        )
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

fn emit_content_warnings(input: &std::path::Path, tokens: &[usize], images: &[usize]) {
    let total_pages = tokens.len();
    if total_pages == 0 {
        return;
    }
    let near_empty = tokens
        .iter()
        .filter(|&&t| t < SCAN_LIKE_TOKEN_THRESHOLD)
        .count();
    if near_empty * 100 / total_pages >= SCAN_LIKE_PAGE_RATIO_PCT {
        eprintln!(
            "warning: {} appears to be scanned/image-based ({}/{} pages have almost no extractable text). \
             Token-based splitting will not reflect actual content size. Consider running the PDF through \
             an OCR tool such as ocrmypdf (https://github.com/ocrmypdf/OCRmyPDF) first — it adds a \
             searchable text layer while preserving the original images.",
            input.display(),
            near_empty,
            total_pages
        );
        return;
    }

    let image_dominant = tokens
        .iter()
        .zip(images.iter())
        .filter(|(&t, &n)| n >= 1 && t < IMAGE_DOMINANT_TOKEN_THRESHOLD)
        .count();
    if image_dominant > 0 {
        eprintln!(
            "warning: {} of {} pages contain significant non-text content (images/figures). \
             Token counts underestimate their size; downstream tools may handle them differently.",
            image_dominant, total_pages
        );
    }
}
