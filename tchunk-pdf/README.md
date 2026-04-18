# tchunk-pdf

Splits a PDF into smaller PDFs at page boundaries, each under a configurable token budget. Output PDFs preserve the original page content byte-for-byte (fonts, layout, embedded images) — pages are not re-rendered.

Built for prepping large PDFs (textbooks, legal codes, reference manuals) for upload to tools that cap source size, like NotebookLM.

## Install

```sh
cargo install --path .
```

Or build directly from the workspace root:

```sh
cargo build --release -p tchunk-pdf
# binary at target/release/tchunk-pdf
```

## Usage

```sh
tchunk-pdf <INPUT.pdf>... [OPTIONS]
```

Common cases:

```sh
# default: 500k token budget, splits at any page boundary, output beside CWD
tchunk-pdf my-book.pdf

# tighter budget, write to a specific dir
tchunk-pdf my-book.pdf -m 200000 -o ./out/

# only break at chapter boundaries (requires PDF outline / bookmarks)
tchunk-pdf my-book.pdf --split-at chapter

# verbose: per-chunk page ranges and token totals
tchunk-pdf my-book.pdf -v

# multiple inputs: each is chunked independently, outputs keyed by that input's stem
tchunk-pdf book-a.pdf book-b.pdf -o ./out/
```

### Multiple inputs

Any number of input PDFs may be passed; each is processed independently and writes its
own `{stem}_NNN.pdf` chunks and `{stem}.index.json` sidecar into `--output-dir`. `--prefix`
is rejected as an error when more than one input is given (it would be ambiguous across
files) — run `tchunk-pdf` once per file in that case.

### Options

| Short | Long             | Default       | Description |
|-------|------------------|---------------|-------------|
| `-m`  | `--max-tokens`   | `500000`      | Target maximum tokens per output chunk. |
| `-s`  | `--split-at`     | `page`        | Coarsest level a split is allowed at: `page`, `any-bookmark`, `subsection`, `section`, `chapter`. |
| `-o`  | `--output-dir`   | `.`           | Output directory (created if missing). |
| `-p`  | `--prefix`       | input stem    | Output filename prefix. Rejected if more than one input is given. |
| `-t`  | `--tokenizer`    | `cl100k_base` | `cl100k_base` or `o200k_base`, or `word_count` |
| `-v`  | `--verbose`      | off           | Print per-chunk page ranges and token totals to stderr. |
| `-q`  | `--quiet`        | off           | Suppress warnings on stderr. Errors still print; warnings remain in the index sidecar. |
| `-j`  | `--jobs`         | `1`           | N threads for extract/tokenize/image-scan. `1` sequential, `0` auto-detect. |

## Output

Files are written as `{prefix}_{NNN}.pdf` with zero-padded sequence numbers (≥3 digits, scaled wider for very large outputs so all files in one run sort lexicographically):

```
my-book_001.pdf
my-book_002.pdf
my-book_003.pdf
```

Single-chunk runs still get the `_001` suffix — no special case for "did it split?". Existing files at the target paths are overwritten without prompting.

### Index sidecar

Alongside the PDFs, a JSON sidecar is written at `{prefix}.index.json` describing the run: source file, config, per-chunk page ranges and token counts, and any structured warnings that were raised. Intended for downstream tooling that needs to know what's inside each chunk without re-parsing the PDFs.

```json
{
  "tool": "tchunk-pdf",
  "version": "0.1.0",
  "source": { "path": "my-book.pdf", "page_count": 320 },
  "config": {
    "tokenizer": "cl100k_base",
    "max_tokens": 500000,
    "split_at_requested": "chapter",
    "split_at_effective": "section"
  },
  "chunks": [
    { "filename": "my-book_001.pdf", "pages": { "start": 1, "end": 112, "count": 112 }, "token_count": 487234, "effective_level": "chapter" },
    { "filename": "my-book_002.pdf", "pages": { "start": 113, "end": 220, "count": 108 }, "token_count": 441200, "effective_level": "section" },
    { "filename": "my-book_003.pdf", "pages": { "start": 221, "end": 320, "count": 100 }, "token_count": 412118, "effective_level": "section" }
  ],
  "warnings": []
}
```

`split_at_effective` is the *finest* level actually used across chunks (the worst-case view of how far recursion had to descend). `effective_level` on each chunk is the level at which *that chunk's* adjacent cuts were taken — for chunks that fit cleanly at the requested level it matches the request; for chunks produced by recursing into an over-budget unit it shows the finer level the recursion landed on.

Warning entries are tagged objects: `scan_like`, `image_dominant`, `outline_missing`, `oversized_page`. The same warnings are still printed to stderr; the sidecar just makes them machine-readable.

## Splitting behavior

- **Page is atomic.** A page is never split mid-page; chunks are always whole-page subsets.
- **Greedy packing** from the front of the document, with a **rebalance pass on the last two chunks** so a near-budget chunk isn't paired with a tiny remainder. Both halves of the rebalance stay under budget.
- **Structural splits** via `--split-at chapter|section|subsection|any-bookmark` use the PDF outline (bookmarks). Outline depth maps to level: depth 1 → chapter, depth 2 → section, depth 3 → subsection, deeper → any-bookmark.
- **Outline missing?** `--split-at` levels above `page` fall back to `page` with a stderr warning.
- **Over-budget units recurse.** If a single unit (e.g. one chapter) exceeds `--max-tokens`, tchunk-pdf treats that unit as its own sub-problem and re-plans it at the next finer level (chapter → section → subsection → any-bookmark → page), balancing its sibling sub-chunks against each other rather than packing greedy-first-fit. Recursion falls through any level with no interior boundaries. Per-chunk `effective_level` in the index sidecar shows which level each chunk's cuts were actually taken at.
- **Oversized pages.** A single page whose token count exceeds `--max-tokens` becomes its own output chunk with a warning.

## Tokenizers

Three options, selected with `-t/--tokenizer`:

- **`cl100k_base`** (default) — tiktoken BPE used by GPT-3.5/4 and many other LLMs. Good general-purpose proxy for LLM token counts.
- **`o200k_base`** — tiktoken BPE used by GPT-4o and newer OpenAI models.
- **`word_count`** — whitespace-split word count with non-alphanumeric chars treated as word boundaries (so `"hello,world"` is 2, `"don't"` is 2). Simple and fast, no model data loaded. Useful when you want "words per chunk" as the budget unit rather than LLM tokens.

NotebookLM doesn't publish its tokenizer, so the BPE options are generic LLM-token proxies — close enough for sizing, not exact.

Per-page text is extracted via `lopdf::Document::extract_text`, which is fast but lower fidelity than dedicated extractors. For our purposes — *counting* tokens to size chunks — approximate is fine; a few percent off doesn't change which side of the budget a chunk lands on.

## Warnings

To stderr (suppressible with `-q/--quiet`; structured copies are always recorded in the index sidecar):

- **Scan-like PDF** — ≥50% of pages have <20 extractable tokens. Strong signal the PDF is image-only / unsearchable. Token-based splitting won't reflect actual content size; OCR preprocessing recommended (see below).
- **Image-dominant pages** — pages with at least one embedded image and <50 tokens of text. Token counts underestimate their effective size; downstream tools may treat them differently.
- **Oversized page** — a single page exceeds `--max-tokens`; emitted as its own chunk.

When a structural unit overruns, tchunk-pdf silently recurses into it at the next finer split level instead of warning — the per-chunk `effective_level` in the index sidecar shows where recursion landed, so tooling can detect and report it without a separate warning channel.

Warnings do not change the exit code.

## Scanned / image-only PDFs

tchunk-pdf doesn't ship OCR. If your PDF is scanned (no text layer), preprocess with [ocrmypdf](https://github.com/ocrmypdf/OCRmyPDF):

```sh
ocrmypdf input.pdf input-ocr.pdf
tchunk-pdf input-ocr.pdf
```

`ocrmypdf` adds a searchable text layer while preserving the original page images. It wraps Tesseract and handles deskew, rotation, and multi-language detection.

## Exit codes

- `0` — success.
- `1` — input file missing, unreadable, or not a valid PDF.
- `2` — CLI argument error (handled by `clap`).
- `3` — output path not writable.

## Performance

Extract and tokenize are the two dominant costs and are both per-page, so both parallelize with `-j/--jobs`. Chunk writing stays sequential (it's I/O-bound and a small share of total time).

Benchmark: [USCODE-2011-title26.pdf](https://www.govinfo.gov/content/pkg/USCODE-2011-title26/pdf/USCODE-2011-title26.pdf) (~3800 pages, 15 output chunks at the default 500k token budget).

| Stage      | `-j 1` | `-j 4` | `-j 0` (auto) |
|------------|-------:|-------:|--------------:|
| extract    | 20.67s |  6.11s |         5.01s |
| tokenize   |  5.82s |  3.26s |         2.56s |
| image-scan |    5ms |    3ms |           3ms |
| write (14) |  818ms |  855ms |         1.01s |

## Limitations / deferred

- **No overlap window between chunks.** When chunks are fed to a downstream LLM directly (RAG pipelines, do-it-yourself retrieval), it's common to have each chunk's start repeat the last few pages of the previous chunk so that passages spanning the cut are fully contained in at least one chunk.
- No font-size-based heading detection for PDFs without an outline.
- No OCR (use `ocrmypdf` upstream).
- Encrypted PDFs are not supported.
- No streaming `stdin` input.

## License

Apache-2.0
