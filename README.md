# tchunk

Token-aware splitters for documents that need to be fed into tools (NotebookLM, etc.) with per-source size limits.

This is a Cargo workspace. Each member is a standalone CLI for a specific document type:

- [`tchunk-pdf`](./tchunk-pdf) — splits a PDF into smaller PDFs at page boundaries, each under a token budget. Preserves original page content (fonts, layout, images).

## Quick start (PDF)

Install directly from git:

```sh
cargo install --git https://github.com/gechelberger/tchunk --bin tchunk-pdf
tchunk-pdf my-book.pdf --max-tokens 500000 --output-dir ./out/
```

Or from a local checkout:

```sh
cargo install --path tchunk-pdf
tchunk-pdf my-book.pdf --max-tokens 500000 --output-dir ./out/
```

See [`tchunk-pdf/README.md`](./tchunk-pdf/README.md) for full options, splitting rules, and OCR preprocessing recommendations.
