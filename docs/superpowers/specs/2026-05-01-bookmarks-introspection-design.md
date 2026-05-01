# Bookmarks introspection (`--bookmarks-hist`, `--bookmarks-tree`) — design

Date: 2026-05-01
Status: approved, ready for implementation planning

## Motivation

The depth-as-primitive refactor (`docs/superpowers/specs/2026-05-01-depth-as-splitting-primitive-design.md`) made `--split-at-depth N` a peer of the named flags. That introduced a discoverability problem: a user picking a depth has no way to inspect a PDF's outline before running. Today the workflow is guess-and-check — pick a depth, run, look at the index sidecar's `split_at_effective`, adjust. For unfamiliar PDFs (especially deep textbooks) that's two or three full chunk runs to learn what `--split-at-depth 3` even *means* for that document.

The fix: two introspection-only flags that print outline metadata and exit before any chunking work.

- `--bookmarks-hist` — depth histogram with derived "would-be segments" stats.
- `--bookmarks-tree` — full indented outline tree with titles and page numbers.

Both flags are opt-in, independent, and combinable. Either one switches the program into **inspection mode**: a separate execution path that loads the PDF, walks the outline, prints, and exits — no tokenization, no planning, no chunk PDFs, no index sidecar.

This split keeps inspection cheap (no token extraction) and keeps the chunking code path unchanged. Inspection answers "what depth should I pick?"; chunking answers "given this depth, do the work."

## CLI (`cli.rs`)

Two new boolean flags on `Cli`:

```rust
/// Print a depth histogram of the input's outline and exit. Combinable with --bookmarks-tree.
#[arg(long = "bookmarks-hist")]
pub bookmarks_hist: bool,

/// Print the full indented outline tree with page numbers and exit. Combinable with --bookmarks-hist.
#[arg(long = "bookmarks-tree")]
pub bookmarks_tree: bool,
```

No clap `ArgGroup` between them and the chunking flags. Chunking-related flags (`-m`, `-s`, `--split-at-depth`, `-t`, `-o`, `-p`, `--tokenizer-file`, `--tokenizer-model`, `-j`) are silently inert when in inspection mode. Rationale: most have defaults, so distinguishing "user set" from "default" needs `ArgMatches` plumbing for marginal benefit. Inspection mode never reads them, so there's no incorrect output to mislead anyone.

`Cli::validate()` skips the `-t huggingface` source check when in inspection mode (no tokenizer is constructed). The `--prefix` validation and input-glob expansion still run — those validate user-supplied paths regardless of mode.

`-q` and `-v` are both inert in inspection mode. The output is data, not status — there are no progress messages to silence and no diagnostics to amplify. The `no outline present` line is part of the answer for that file, not a warning, so it is always emitted regardless of `-q`.

## New type and outline walk (`pdf.rs`)

A flat record type for one outline entry:

```rust
pub struct OutlineEntry {
    pub depth: u32,    // 1-based, matching Boundary::Bookmark { depth }
    pub page: u32,     // 1-based page number the entry targets
    pub title: String, // raw title text from the outline node; "" if missing
}
```

A new public method on `Pdf`:

```rust
pub fn outline_entries(&self) -> Vec<OutlineEntry>
```

Returns the outline in document order (depth-first preorder over the `/Outlines` tree, which is also the visual top-to-bottom order in any PDF reader). Empty `Vec` if no outline. Entries whose destination resolves outside the document's page range are skipped silently — same behavior as the existing `resolve_page` in `boundaries()`.

Implementation note: `lopdf::Outline::Destination` may not directly carry the bookmark title; the title lives in the outline node's `/Title` entry (a PDF text string, possibly with PDFDocEncoding or UTF-16BE BOM). If `get_outlines()` strips titles during the walk, `outline_entries()` traverses the raw `/Outlines` tree via `get_dictionary` to read `/Title` from each node directly. This is an implementation detail to resolve in the plan, not the spec; what the spec guarantees is the API shape and the document-order requirement.

The existing `boundaries()` method is unchanged. It synthesizes a depth-1 bookmark on page 1 to keep the planner working on outlineless PDFs; that synthetic marker is a planner concern and does not appear in `outline_entries()` (which reflects the actual outline only).

## New module (`inspect.rs`)

Pure presentation. Two public functions:

```rust
pub fn print_histogram<W: Write>(
    out: &mut W,
    entries: &[OutlineEntry],
    page_count: usize,
) -> io::Result<()>;

pub fn print_tree<W: Write>(
    out: &mut W,
    entries: &[OutlineEntry],
    page_count: usize,
) -> io::Result<()>;
```

`page_count` is needed for the histogram's `pages-long` math and for sizing the page-number prefix in the tree.

Stateless, no I/O beyond the writer. The caller provides `&mut io::stdout().lock()` in production code; tests pass `&mut Vec<u8>` and assert on the resulting string.

### Histogram format

Per file (no leading framing — the caller handles `=== file.pdf (i/N) ===`):

```
423 pages, 312 bookmarks, max depth 4
  at depth 1:  12 bookmarks  → 12 segments, 5-89 pages long
  at depth 2:  87 bookmarks  → 99 segments, 1-23 pages long
  at depth 3: 200 bookmarks  → 299 segments, 1-12 pages long
  at depth 4:  13 bookmarks  → 312 segments, 1-8 pages long
```

Header line: `<page_count> pages, <total_bookmarks> bookmarks, max depth <max_depth>`. When the outline is missing or empty: `<page_count> pages, no outline present` and no histogram body.

Histogram rows, one per depth from 1 to max_depth (inclusive), even for depths with zero entries (a gap in the outline shape is information). Per-row data:

- **Left half — raw count**: `at depth N: K bookmarks` where K is the count of `OutlineEntry` records at exactly that depth.
- **Right half — derived segment stats** (separated by `→`): if the user splits at depth N, the document divides into S segments where S = sum of bookmarks at depth ≤ N (every such bookmark is a cut point). Page-span min and max across those S segments, computed by walking entries in document order and taking page-deltas between consecutive cut points (the final segment ends at `page_count`).

Caveat for the rare PDF whose outline does not cover page 1 at depth ≤ 1: the planner's `boundaries()` injects a synthetic depth-1 cut at page 1 to keep splitting well-defined, so the actual chunk count at depth N would be S + 1 in that case. The histogram reports outline truth (S), not planner truth (S + 1). Documenting this caveat in `--help`/README rather than complicating the histogram with a synthesized-cut row keeps the output legible and the math honest about what's in the file.

Range collapse: when min == max, format as `N pages long` (and `N page long` when N == 1, drop the `s` for grammar). Otherwise `min-max pages long`.

Numeric alignment: right-align the `K` column (entries) so digits line up across rows. The `S segments` and `min-max pages long` parts vary in width naturally — no alignment needed there.

### Tree format

Per file:

```
[p1]    Front Matter
[p3]      Acknowledgements
[p5]      Preface
[p9]    Chapter 1 — Introduction
[p11]     1.1 Background
[p18]     1.2 Motivation
[p27]       1.2.1 Prior Work
[p33]       1.2.2 Open Problems
[p38]     1.3 Outline of the Book
[p45]   Chapter 2 — Foundations
```

- Page prefix: `[pN]` left-aligned, fixed width sized to the document's max page count (e.g. `[p1]` and `[p423]` align if the doc has 423 pages, so `[pN]` occupies `len("[p") + ceil(log10(page_count + 1)) + len("]")` columns, padded with spaces on the right).
- Indent: 2 spaces per depth level. Depth 1 starts immediately after the page-prefix column, depth 2 indents 2 more, etc.
- Title: rendered verbatim from `OutlineEntry::title`. No truncation; long titles wrap in the terminal naturally. Empty title → `(untitled)`. Control characters (newlines, tabs) inside titles are passed through unmodified — escaping policy is a downstream filter concern.
- No outline: `<page_count> pages, no outline present` (same line as the histogram's no-outline form, single line, no tree body).

## Control flow (`main.rs`)

`run()` (the existing entry point past parsing) gains an early branch:

```rust
fn run(mut cli: Cli) -> Result<(), RunError> {
    cli.validate().map_err(RunError::Input)?;

    if cli.bookmarks_hist || cli.bookmarks_tree {
        return run_inspect(&cli);
    }

    // existing chunking path unchanged
    ...
}
```

`run_inspect()` is a new function. Sketch (real implementation handles `io::Error → RunError::Output` mapping):

```rust
fn run_inspect(cli: &Cli) -> Result<(), RunError> {
    let multi = cli.inputs.len() > 1;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for (idx, input) in cli.inputs.iter().enumerate() {
        if multi {
            if idx > 0 {
                writeln!(out).map_err(|e| RunError::Output(anyhow::anyhow!(e)))?;
            }
            writeln!(out, "=== {} ({}/{}) ===", input.display(), idx + 1, cli.inputs.len())
                .map_err(|e| RunError::Output(anyhow::anyhow!(e)))?;
        }
        let pdf = Pdf::load(input).map_err(RunError::Input)?;
        if pdf.page_count() == 0 {
            return Err(RunError::Input(anyhow::anyhow!(
                "PDF contains no pages: {}", input.display()
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
```

The inspect path skips: tokenizer construction, thread pool construction, text extraction, tokenization, image counting, planning, chunk writing, index sidecar generation. It shares only `Pdf::load` and the multi-file iteration shell with the chunking path.

I/O routing: the inspection output goes to **stdout** (the program's "answer") so it pipes cleanly (`tchunk-pdf foo.pdf --bookmarks-hist | grep depth`). The `=== file.pdf (i/N) ===` framing also goes to stdout — it's structural metadata that belongs alongside the data. PDF load failures and other genuine errors continue to go to stderr via the existing `RunError` machinery.

## Sidecar / index (`index.rs`)

Unchanged. Inspection mode does not produce a sidecar.

## Tests

### Unit tests in `inspect.rs`

- `print_histogram_no_outline_emits_single_line`: empty `entries` → `"<N> pages, no outline present\n"`.
- `print_histogram_basic`: synthetic entries spanning depths 1-3 → expected histogram text. Verifies row count, bookmark counts, segment counts (cumulative), min-max page-span math, alignment.
- `print_histogram_singular_grammar`: a depth row whose segments are all 1 page → renders `1 page long`, not `1 pages long`.
- `print_histogram_min_eq_max_collapse`: a depth row where min == max → renders `N pages long`, not `N-N pages long`.
- `print_histogram_includes_zero_count_rows`: outline with depths 1, 3 (no depth-2 entries) → histogram still emits a row for depth 2 with `0 bookmarks` and the segment stats reflecting cumulative-so-far.
- `print_tree_no_outline`: empty `entries` → same single-line "no outline present" form.
- `print_tree_basic`: synthetic entries with mixed depths → indented output, `[pN]` prefix correctly width-padded for the document's page count.
- `print_tree_empty_title_renders_untitled`: an `OutlineEntry { title: "".into(), .. }` → `(untitled)` in the output.

### Unit tests in `pdf.rs`

- `outline_entries_empty_when_no_outline`: a synthetic PDF without `/Outlines` → empty vec.
- `outline_entries_in_document_order`: a synthetic PDF with a known nested outline → entries returned in depth-first preorder, depths reflect nesting.
- `outline_entries_extracts_titles`: synthesized entries' titles match expected strings (cover PDFDocEncoding and UTF-16BE-with-BOM by writing both encodings explicitly into `/Title` values).
- `outline_entries_skips_out_of_range_destinations`: synthesized PDF with an outline entry pointing at a page beyond `page_count` → that entry is skipped, others kept.

### Test infrastructure addition

The existing `tests/end_to_end.rs::synthesize_pdf` builds outline-less PDFs. The implementation adds a sibling helper (`synthesize_pdf_with_outline` or an extension to the existing builder) that takes a list of `(depth, page, title)` triples and emits a PDF with a valid `/Outlines` tree. The `pdf.rs` unit tests above and the integration tests below depend on it.

### Integration tests in `tests/end_to_end.rs`

- `inspection_mode_writes_no_chunks_or_sidecar`: run the binary with `--bookmarks-hist` against a synthesized PDF in a tempdir, assert exit 0, no `*.pdf` chunks created, no `*.index.json` created, and stdout contains the expected histogram body.
- `inspection_mode_multi_file_framing`: two synthesized PDFs, assert both `=== ===` frames appear on stdout, the per-file blocks are separated by a blank line, and stderr stays empty.
- `inspection_mode_combined_flags`: same PDF run with both `--bookmarks-hist --bookmarks-tree`, assert the histogram block precedes the tree block in stdout.

## Edge cases & decisions

- **Multiple outline entries pointing at the same page**: each is its own `OutlineEntry` (no deduplication). The existing `boundaries()` keeps the coarsest depth per page for planning purposes; that policy is planner-internal and does not propagate here. The histogram counts each entry once at its depth; the tree shows each line.
- **Outline entry resolves outside document range**: skipped silently (matches existing `resolve_page` behavior).
- **Outline title is empty or missing `/Title`**: rendered as `(untitled)` in the tree, counted normally in the histogram.
- **Outline title contains control characters**: passed through verbatim. Users pipe through their own filter if they care.
- **Single-bookmark document**: histogram renders `1 bookmark` (singular) and `1 segment` (singular). The "max depth 1" header line stays grammatical.
- **Zero-page document**: rejected by the existing `process_input` check; same check applies to `run_inspect`. Hard-error before any printing.
- **PDF that fails to load**: the existing `RunError::Input` path triggers; the user sees the error on stderr and the program exits non-zero. In multi-file mode, a load failure on input N exits before processing N+1 (consistent with the existing chunking loop's fail-fast behavior).

## Out of scope (not in this change)

- JSON output for scripting (`--bookmarks-hist=json`). Plain text is sufficient for the user's stated workflow; revisit if a real script consumer surfaces.
- Token-aware preview (the rejected option C from brainstorming) — "if I split at depth N, here's how many segments would overrun budget." Out of scope because it requires the full extract+tokenize pipeline, which inspection mode is explicitly meant to skip.
- Replacing the existing `split_at_effective` post-run report with a recommendation engine. Out of scope; the introspection flags exist precisely so users can pre-compute that themselves.
- Changes to the `next-steps.md` items (oversized-page warning text, `split_at_effective` rename, HF tokenizer fetch progress).

## Risks

- **lopdf title extraction**: if `lopdf::Document::get_outlines()` does not expose titles, `outline_entries()` needs a second walk over the raw `/Outlines` dict. The implementation plan resolves which API path is used; the spec's contract (return `Vec<OutlineEntry>` in document order with titles) is unchanged either way. If lopdf's API genuinely cannot recover titles, fallback is to emit titles as `""` (rendered as `(untitled)`) — degraded but not broken.
- **Title encoding**: PDF text strings are either PDFDocEncoding or UTF-16BE with BOM. lopdf's `Object::as_string` returns the raw bytes; decoding is the caller's job. Implementation must handle both encodings to avoid mojibake on the tree output.
- **Very deep outlines (depth > ~10)**: the histogram emits one row per depth so a 30-deep outline is a tall block. This is acceptable: deep outlines exist in real-world technical documents, and truncating would hide information the user is asking for. No special handling.
- **Very large outlines (1000+ entries)**: the tree output is long. `| less` and `| grep` solve this; no in-tool pagination.
