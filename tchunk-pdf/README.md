# tchunk-pdf

Splits a PDF into smaller PDFs along structural boundaries — by default at chapter cuts from the PDF's outline, falling back to page boundaries if no outline is present — each under a configurable token budget. Pages are atomic; chunks are always whole-page subsets and the original page content is preserved byte-for-byte (fonts, layout, embedded images), not re-rendered.

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
# default: 500k token budget, splits at chapter boundaries (falls back to page if the
# PDF has no outline), output beside CWD
tchunk-pdf my-book.pdf

# tighter budget, write to a specific dir
tchunk-pdf my-book.pdf -m 200000 -o ./out/

# break anywhere on a page boundary (ignore the outline)
tchunk-pdf my-book.pdf --split-at page

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
| `-s`  | `--split-at`     | `chapter`     | Coarsest level a split is allowed at: `page`, `any-bookmark`, `subsection`, `section`, `chapter`. Named flags are sugar for specific outline depths (`chapter`=1, `section`=2, `subsection`=3); `any-bookmark` matches any depth. Outline-based levels fall back to `page` with a warning if the PDF has no bookmarks. Mutually exclusive with `--split-at-depth`. |
|       | `--split-at-depth` | —           | Split at a specific outline depth (e.g. `--split-at-depth 4` for outlines deeper than the named flags reach). Mutually exclusive with `--split-at`. |
| `-o`  | `--output-dir`   | `.`           | Output directory (created if missing). |
| `-p`  | `--prefix`       | input stem    | Output filename prefix. Rejected if more than one input is given. |
| `-t`  | `--tokenizer`    | `word_count`  | `cl100k_base`, `o200k_base`, `word_count`, or `huggingface` (see [Tokenizers](#tokenizers)). |
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
  "source": { "path": "my-book.pdf", "page_count": 320, "total_tokens": 1340552 },
  "config": {
    "tokenizer": "o200k_base",
    "max_tokens": 500000,
    "split_at_requested": "depth-1",
    "split_at_effective": "depth-2"
  },
  "chunks": [
    { "filename": "my-book_001.pdf", "pages": { "start": 1, "end": 112, "count": 112 }, "token_count": 487234, "effective_level": "depth-1" },
    { "filename": "my-book_002.pdf", "pages": { "start": 113, "end": 220, "count": 108 }, "token_count": 441200, "effective_level": "depth-2" },
    { "filename": "my-book_003.pdf", "pages": { "start": 221, "end": 320, "count": 100 }, "token_count": 412118, "effective_level": "depth-2" }
  ],
  "warnings": []
}
```

`split_at_effective` is the *finest* level actually used across chunks (the worst-case view of how far recursion had to descend). `effective_level` on each chunk is the level at which *that chunk's* adjacent cuts were taken — for chunks that fit cleanly at the requested level it matches the request; for chunks produced by recursing into an over-budget unit it shows the finer level the recursion landed on. Both fields use the canonical depth strings (`"page"`, `"any-bookmark"`, or `"depth-N"`); the named CLI flags (`chapter`, `section`, `subsection`) are sugar for specific depths on input only and don't round-trip into the sidecar.

Warning entries are tagged objects: `scan_like`, `image_dominant`, `outline_missing`, `oversized_page`. The same warnings are still printed to stderr; the sidecar just makes them machine-readable.

## Splitting behavior

- **Page is atomic.** A page is never split mid-page; chunks are always whole-page subsets.
- **Greedy packing** from the front of the document, with a **rebalance pass on the last two chunks** so a near-budget chunk isn't paired with a tiny remainder. Both halves of the rebalance stay under budget.
- **Structural splits** are the default. `--split-at chapter` (the default), `section`, `subsection`, and `any-bookmark` all use the PDF outline (bookmarks). Named flags map to specific outline depths: `chapter`=1, `section`=2, `subsection`=3. `any-bookmark` matches every outline entry regardless of depth. For outlines whose top level isn't called "chapter" (e.g. Parts/Chapters books, where chapters are at depth 2), use `--split-at-depth N` to target the actual depth. Use `--split-at page` to ignore the outline entirely.
- **Outline missing?** `--split-at` levels above `page` fall back to `page` with a stderr warning.
- **Over-budget units recurse.** If a single unit (e.g. one chapter) exceeds `--max-tokens`, tchunk-pdf treats that unit as its own sub-problem and re-plans it at the next finer outline depth (depth-1 → depth-2 → depth-3 → ... → page), balancing its sibling sub-chunks against each other rather than packing greedy-first-fit. Recursion falls through any depth with no interior boundaries. Per-chunk `effective_level` in the index sidecar shows which depth each chunk's cuts were actually taken at.
- **Oversized pages.** A single page whose token count exceeds `--max-tokens` becomes its own output chunk with a warning.

## Inspecting a PDF's outline

Before running a chunk job, you can inspect a PDF's outline to choose the right
`--split-at-depth N`. Two opt-in flags switch the program into inspection mode
(no chunking, no sidecar, no PDFs written):

- `--bookmarks-hist` — print a depth histogram. For each outline depth, shows
  the bookmark count, the cumulative number of segments produced if you split
  at that depth, and the min/max page span across those segments.
- `--bookmarks-tree` — print the full indented outline tree with page numbers.

Both flags are independent and combinable. With both set, the histogram prints
first, then the tree.

Example:

```
$ tchunk-pdf my-textbook.pdf --bookmarks-hist
423 pages, 312 bookmarks, max depth 4
  at depth 1:  12 bookmarks  → 12 segments, 5-89 pages long
  at depth 2:  87 bookmarks  → 99 segments, 1-23 pages long
  at depth 3: 200 bookmarks  → 299 segments, 1-12 pages long
  at depth 4:  13 bookmarks  → 312 segments, 1-8 pages long
```

In inspection mode all chunking-related flags (`-m`, `-s`, `--split-at-depth`,
`-t`, `-o`, `-p`, `-j`) are silently ignored. With multiple inputs, each file
is printed in its own `=== file.pdf (i/N) ===` block, separated by a blank line.

### Caveat: synthetic page-1 cut

The histogram counts only real outline entries. For the rare PDF whose outline
does not target page 1 at depth 1, the planner injects a synthetic depth-1 cut
at page 1 so splitting is well-defined; the actual chunk count at depth N would
be `S + 1` rather than the `S` shown in the histogram. Most real-world PDFs
include a page-1 entry already, in which case the histogram is exact.

## Tokenizers

Four options, selected with `-t/--tokenizer`:

- **`word_count`** (default) — whitespace-split word count with non-alphanumeric chars treated as word boundaries (so `"hello,world"` is 2, `"don't"` is 2). Simple and fast, no model data loaded. Useful when you want "words per chunk" as the budget unit rather than LLM tokens.
- **`o200k_base`** — tiktoken BPE used by GPT-4o and newer OpenAI models. Good general-purpose proxy for modern LLM token counts.
- **`cl100k_base`** — tiktoken BPE used by GPT-3.5/4 and many older LLMs.
- **`huggingface`** — any tokenizer that ships a `tokenizer.json` (Llama, Mistral, Gemma, Qwen, DeepSeek, BERT-family, etc.). Requires either `--tokenizer-file <PATH>` or `--tokenizer-model <HF_MODEL_ID>`.

NotebookLM doesn't publish its tokenizer, so the BPE options are generic LLM-token proxies — close enough for sizing, not exact.

Per-page text is extracted via `lopdf::Document::extract_text`, which is fast but lower fidelity than dedicated extractors. For our purposes — *counting* tokens to size chunks — approximate is fine; a few percent off doesn't change which side of the budget a chunk lands on.

### HuggingFace tokenizers

```sh
# from a local tokenizer.json
tchunk-pdf -t huggingface --tokenizer-file ./llama3-tokenizer.json my-book.pdf

# fetched once from the Hub and cached for subsequent runs
tchunk-pdf -t huggingface --tokenizer-model meta-llama/Meta-Llama-3-8B my-book.pdf

# ungated model, good for a first smoke test
tchunk-pdf -t huggingface --tokenizer-model gpt2 my-book.pdf
```

**Finding a model**: there's no built-in listing. Browse <https://huggingface.co/models>, filter by task, and look for a `tokenizer.json` in the repo's Files tab. Any repo that has one will work. Repos with only `vocab.txt` / `sentencepiece.model` and no `tokenizer.json` won't.

**Caching**: `--tokenizer-model` caches under `$HF_HOME/hub` (default `~/.cache/huggingface/hub`).

**Gated models** (Llama, Gemma, some Mistral) require you to accept the model's license on the Hub and authenticate. tchunk-pdf looks for a token in this order (matching the Python `huggingface_hub` library):

1. `HF_TOKEN` env var
2. `HUGGING_FACE_HUB_TOKEN` env var (legacy)
3. `$HF_HOME/token` file, written by `huggingface-cli login`

```sh
export HF_TOKEN=hf_xxx...
# or: huggingface-cli login   (one-time, writes ~/.cache/huggingface/token)
# or: $env:HF_TOKEN = "hf_xxx"   (powershell)
tchunk-pdf -t huggingface --tokenizer-model meta-llama/Meta-Llama-3-8B my-book.pdf
```

The index sidecar records the tokenizer as `huggingface:<basename>` for file-based runs or `huggingface:<model_id>` for Hub-fetched ones, so you can tell after the fact which tokenizer a chunk set was sized against.

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

Dual-licensed under [MIT](../LICENSE-MIT) or [Apache-2.0](../LICENSE-APACHE) at your option. See the [workspace README](../README.md#license) for details.
