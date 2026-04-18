use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};

use tchunk_pdf::cli::Cli;
use tchunk_pdf::pdf::Pdf;
use tchunk_pdf::plan::{plan_chunks, BoundaryLevel, Diagnostic};
use tchunk_pdf::tokenize::{TiktokenTokenizer, Tokenizer};

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

    let tokenizer = TiktokenTokenizer::new(cli.tokenizer.as_str()).map_err(RunError::Input)?;

    let extract_pb = spinner(&format!("Extracting text from {page_count} pages..."));
    let texts = pdf.page_texts();
    extract_pb.finish_and_clear();

    let tok_pb = page_bar(page_count as u64, "Tokenizing");
    let tokens: Vec<usize> = texts
        .iter()
        .map(|t| {
            let n = tokenizer.count(t);
            tok_pb.inc(1);
            n
        })
        .collect();
    tok_pb.finish_and_clear();

    let images = pdf.image_counts();

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
    write_pb.finish_and_clear();

    Ok(())
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
            "{msg} [{bar:30.cyan/blue}] {pos}/{len} pages ({percent}%) eta {eta}",
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
            "{msg} [{bar:30.cyan/blue}] {pos}/{len} chunks ({percent}%) eta {eta}",
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
