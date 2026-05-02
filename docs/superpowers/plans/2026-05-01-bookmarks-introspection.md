# Bookmarks introspection — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two introspection-only flags `--bookmarks-hist` and `--bookmarks-tree` that print outline metadata for a PDF (depth histogram and indented outline tree, respectively) and exit before any chunking work, so users picking `--split-at-depth N` can see what's actually in the outline first.

**Architecture:** Either flag switches the program into "inspection mode," a separate execution path off `run()` that loads the PDF, fetches outline entries via lopdf's existing `Document::get_toc()`, prints to stdout via writer-parameterized print functions, and exits. No tokenization, no thread pool, no planning, no chunk PDFs, no index sidecar. The print functions live in a new `inspect` module and take `&mut impl Write` to make unit tests trivial (assert on a `Vec<u8>` buffer). The chunking code path is unchanged.

**Tech Stack:** Rust, clap (CLI), lopdf (PDF parsing — uses `Document::get_toc()` for title-decoded outline extraction).

**Source-of-truth design doc:** `docs/superpowers/specs/2026-05-01-bookmarks-introspection-design.md`. Read it before starting.

**Helpful context for the implementer:** lopdf 0.36's `Document::get_toc()` (in `src/toc.rs`) returns `Result<Toc>` where `Toc.toc` is `Vec<TocType { level: usize, title: String, page: usize }>` — the level is 1-based, page is the resolved 1-based document page number, and title decoding (PDFDocEncoding via lossy UTF-8, UTF-16BE/LE with BOM) is already handled. Returns `Err(Error::NoOutline)` when no outline exists. Out-of-range pages are filtered out by the underlying page-id resolution, so we don't need to filter again. `pdf.rs` does not currently call `get_toc()` — `boundaries()` does its own outline walk because it needs per-page output, not a flat list.

---

## File map

| File                                    | Change                                                                  |
|-----------------------------------------|-------------------------------------------------------------------------|
| `tchunk-pdf/src/pdf.rs`                 | Add `OutlineEntry` struct and `outline_entries()` method.               |
| `tchunk-pdf/src/inspect.rs`             | **New file** — `print_histogram` and `print_tree`.                      |
| `tchunk-pdf/src/lib.rs`                 | Export new `inspect` module.                                            |
| `tchunk-pdf/src/cli.rs`                 | Add `--bookmarks-hist`, `--bookmarks-tree` flags. Skip HF check when set.|
| `tchunk-pdf/src/main.rs`                | Add `run_inspect()`. Branch in `run()` when either inspection flag set. |
| `tchunk-pdf/tests/end_to_end.rs`        | Add `synthesize_pdf_with_outline` helper + integration tests.           |
| `tchunk-pdf/README.md`                  | Document the two new flags + the synthetic-marker caveat.               |

---

## Task 1: `OutlineEntry` struct and stub `outline_entries()`

Define the new type and a stub method that always returns an empty `Vec`. Add an integration test confirming the stub returns empty for an outlineless PDF (the existing `synthesize_pdf` produces those). This task lays the API surface; Task 2 fills in real logic.

**Files:**
- Modify: `tchunk-pdf/src/pdf.rs`
- Modify: `tchunk-pdf/tests/end_to_end.rs`

- [ ] **Step 1: Add the `OutlineEntry` struct and stub method to `pdf.rs`**

In `tchunk-pdf/src/pdf.rs`, add this struct definition immediately after the existing `use` block at the top (after line 8):

```rust
/// One entry in a PDF's outline (a.k.a. bookmark). Returned by `Pdf::outline_entries`,
/// consumed by the `inspect` module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineEntry {
    /// 1-based outline depth, matching `Boundary::Bookmark { depth }` from `plan.rs`.
    pub depth: u32,
    /// 1-based page number the entry targets.
    pub page: u32,
    /// Decoded title text. Empty string when the outline node has no `/Title` or its
    /// decoding produced an empty string.
    pub title: String,
}
```

Then add the stub method on `Pdf` immediately after the existing `has_outline` method (after line 159):

```rust
    /// Outline entries flattened in document order (depth-first preorder over `/Outlines`).
    /// Returns an empty `Vec` when the PDF has no outline. Entries whose destination
    /// resolves outside the document's page range are skipped silently.
    pub fn outline_entries(&self) -> Vec<OutlineEntry> {
        Vec::new()
    }
```

- [ ] **Step 2: Add the failing test to `tests/end_to_end.rs`**

Append this test to the end of `tchunk-pdf/tests/end_to_end.rs`:

```rust
#[test]
fn outline_entries_empty_when_no_outline() {
    let bytes = synthesize_pdf(3);
    let dir = std::env::temp_dir().join(format!(
        "tchunk-pdf-test-outline-empty-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let input_path = dir.join("input.pdf");
    std::fs::write(&input_path, &bytes).unwrap();

    let pdf = Pdf::load(&input_path).expect("load");
    assert!(pdf.outline_entries().is_empty(), "expected empty Vec for outlineless PDF");
}
```

Add `OutlineEntry` to the existing `use tchunk_pdf::pdf::Pdf;` import at the top of the file:

```rust
use tchunk_pdf::pdf::{OutlineEntry, Pdf};
```

(The `OutlineEntry` import won't be used in Task 1's test but earns its keep in Task 2; adding it now keeps the import line stable.)

- [ ] **Step 3: Run the test, expect it to pass**

Run: `cargo test -p tchunk-pdf --test end_to_end outline_entries_empty_when_no_outline`
Expected: PASS (the stub returns empty, the test asserts empty).

If it fails, fix the import or struct visibility before continuing.

- [ ] **Step 4: Run all existing tests to make sure nothing regressed**

Run: `cargo test -p tchunk-pdf`
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add tchunk-pdf/src/pdf.rs tchunk-pdf/tests/end_to_end.rs
git commit -m "[feat] pdf.rs: add OutlineEntry type and stub outline_entries()"
```

---

## Task 2: Real `outline_entries()` via `get_toc` + `synthesize_pdf_with_outline` helper

Add a test helper that synthesizes a PDF with a valid `/Outlines` tree from a flat list of `(depth, page, title)` triples. Use it in a new test that asserts `outline_entries()` returns the expected entries in document order. Replace the stub with a real implementation that delegates to lopdf's `Document::get_toc()`.

**Files:**
- Modify: `tchunk-pdf/src/pdf.rs`
- Modify: `tchunk-pdf/tests/end_to_end.rs`

- [ ] **Step 1: Add the `synthesize_pdf_with_outline` helper to `tests/end_to_end.rs`**

Add this function near the existing `synthesize_pdf` (around line 66, before the `#[test]` attributes). It builds a base PDF (extracted from `synthesize_pdf`) and overlays an outline tree built from `(depth, page, title)` triples.

```rust
/// Synthesize an N-page PDF with the given outline. Each outline entry is a
/// `(depth, page_num, title)` triple. Depth is 1-based; entries must be given
/// in document order. The function constructs the parent/child/sibling references
/// of a valid PDF outline tree from this flat list.
fn synthesize_pdf_with_outline(
    page_count: usize,
    outline: &[(u32, u32, &str)],
) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut page_ids: Vec<Object> = Vec::with_capacity(page_count);
    for i in 1..=page_count {
        let text = format!("Page {i}");
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![100.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id =
            doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        page_ids.push(page_id.into());
    }

    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => page_ids.clone(),
        "Count" => page_count as i64,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));

    // Reserve object IDs for each outline item up front so /First /Last /Next /Prev /Parent
    // references can use them before the items themselves exist.
    let item_ids: Vec<lopdf::ObjectId> =
        (0..outline.len()).map(|_| doc.new_object_id()).collect();
    let outlines_id = doc.new_object_id();

    // Build parent/sibling/child references by walking entries with a depth stack.
    // `parent_at_depth[d]` = item id of the most-recent open item at depth d (if any).
    // `last_sibling_at_depth[d]` = item id of the most-recent item at depth d under the
    //   current parent, used to wire /Next /Prev links.
    let mut parent_at_depth: Vec<Option<lopdf::ObjectId>> = vec![None; 32];
    let mut last_sibling_at_depth: Vec<Option<lopdf::ObjectId>> = vec![None; 32];
    let mut first_child_of: std::collections::HashMap<lopdf::ObjectId, lopdf::ObjectId> =
        std::collections::HashMap::new();
    let mut last_child_of: std::collections::HashMap<lopdf::ObjectId, lopdf::ObjectId> =
        std::collections::HashMap::new();
    let mut child_count_of: std::collections::HashMap<lopdf::ObjectId, i64> =
        std::collections::HashMap::new();
    let mut top_level_count: i64 = 0;
    let mut top_level_first: Option<lopdf::ObjectId> = None;
    let mut top_level_last: Option<lopdf::ObjectId> = None;

    for (i, &(depth, _page, _title)) in outline.iter().enumerate() {
        let d = depth as usize;
        // When we step shallower or to a sibling, clear deeper levels' state.
        for deeper in (d + 1)..parent_at_depth.len() {
            parent_at_depth[deeper] = None;
            last_sibling_at_depth[deeper] = None;
        }
        // Record this item as the parent for any deeper items that follow.
        parent_at_depth[d] = Some(item_ids[i]);

        // Wire as sibling under current parent.
        let parent_id = if d == 1 { outlines_id } else { parent_at_depth[d - 1].expect("orphan outline entry: depth > 1 with no parent at depth-1") };
        if d == 1 {
            top_level_count += 1;
            if top_level_first.is_none() {
                top_level_first = Some(item_ids[i]);
            }
            top_level_last = Some(item_ids[i]);
        } else {
            *child_count_of.entry(parent_id).or_insert(0) += 1;
            first_child_of.entry(parent_id).or_insert(item_ids[i]);
            last_child_of.insert(parent_id, item_ids[i]);
        }
        last_sibling_at_depth[d] = Some(item_ids[i]);
    }

    // Now build sibling /Next /Prev links via a second pass that, for each item, finds
    // its previous sibling and next sibling under the same parent.
    let mut prev_sibling: Vec<Option<lopdf::ObjectId>> = vec![None; outline.len()];
    let mut next_sibling: Vec<Option<lopdf::ObjectId>> = vec![None; outline.len()];
    {
        // For each parent (including outlines_id for top-level), collect children in order
        // and link them.
        let mut children_of: std::collections::HashMap<lopdf::ObjectId, Vec<usize>> =
            std::collections::HashMap::new();
        let mut local_parent_at_depth: Vec<Option<lopdf::ObjectId>> = vec![None; 32];
        for (i, &(depth, _page, _title)) in outline.iter().enumerate() {
            let d = depth as usize;
            for deeper in (d + 1)..local_parent_at_depth.len() {
                local_parent_at_depth[deeper] = None;
            }
            let parent_id = if d == 1 { outlines_id } else { local_parent_at_depth[d - 1].expect("orphan") };
            children_of.entry(parent_id).or_default().push(i);
            local_parent_at_depth[d] = Some(item_ids[i]);
        }
        for siblings in children_of.values() {
            for w in siblings.windows(2) {
                next_sibling[w[0]] = Some(item_ids[w[1]]);
                prev_sibling[w[1]] = Some(item_ids[w[0]]);
            }
        }
    }

    // Emit each outline item dictionary at its reserved ID.
    for (i, &(depth, page, title)) in outline.iter().enumerate() {
        let d = depth as usize;
        let parent_id = if d == 1 {
            outlines_id
        } else {
            // The parent at depth d-1 captured during the first pass. Re-derive it here
            // by scanning backwards for the closest entry at depth d-1.
            let mut p: Option<lopdf::ObjectId> = None;
            for j in (0..i).rev() {
                if outline[j].0 == depth - 1 {
                    p = Some(item_ids[j]);
                    break;
                }
                if outline[j].0 < depth - 1 {
                    panic!("orphan outline entry at index {i}: jumped from depth {} to {}", outline[j].0, depth);
                }
            }
            p.expect("no parent found for non-top-level outline entry")
        };
        let page_ref = if (page as usize) >= 1 && (page as usize) <= page_count {
            page_ids[(page - 1) as usize].clone()
        } else {
            // Out-of-range: emit a literal page number that won't resolve. lopdf's
            // resolve_page handles Object::Integer too, so we use a literal integer
            // beyond the range to force a skip.
            Object::Integer((page as i64) - 1) // 0-based for Integer destinations
        };
        let mut item = dictionary! {
            "Title" => Object::string_literal(title),
            "Parent" => Object::Reference(parent_id),
            "Dest" => Object::Array(vec![
                page_ref,
                Object::Name(b"Fit".to_vec()),
            ]),
        };
        if let Some(p) = prev_sibling[i] {
            item.set("Prev", Object::Reference(p));
        }
        if let Some(n) = next_sibling[i] {
            item.set("Next", Object::Reference(n));
        }
        if let Some(&fc) = first_child_of.get(&item_ids[i]) {
            item.set("First", Object::Reference(fc));
            item.set("Last", Object::Reference(*last_child_of.get(&item_ids[i]).unwrap()));
            item.set("Count", *child_count_of.get(&item_ids[i]).unwrap_or(&0));
        }
        doc.objects.insert(item_ids[i], Object::Dictionary(item));
    }

    // Emit the root /Outlines dictionary.
    let mut outlines_dict = dictionary! {
        "Type" => "Outlines",
        "Count" => top_level_count,
    };
    if let Some(f) = top_level_first {
        outlines_dict.set("First", Object::Reference(f));
    }
    if let Some(l) = top_level_last {
        outlines_dict.set("Last", Object::Reference(l));
    }
    doc.objects.insert(outlines_id, Object::Dictionary(outlines_dict));

    // Catalog references both /Pages and /Outlines.
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "Outlines" => Object::Reference(outlines_id),
    });
    doc.trailer.set("Root", catalog_id);
    doc.compress();

    let mut out = Vec::new();
    doc.save_to(&mut out).expect("save_to memory buffer");
    out
}
```

Note: this helper deliberately avoids using `lopdf::Outline`/`Destination` builder types because lopdf's high-level outline-construction API is undocumented and asymmetric to the read API. Building the dict directly is verbose but mechanical and matches the existing helper style in this file.

- [ ] **Step 2: Add a failing test that exercises the helper and the real implementation**

Append to `tests/end_to_end.rs`:

```rust
#[test]
fn outline_entries_in_document_order() {
    let outline: Vec<(u32, u32, &str)> = vec![
        (1, 1, "Chapter 1"),
        (2, 2, "Section 1.1"),
        (2, 3, "Section 1.2"),
        (1, 4, "Chapter 2"),
        (2, 5, "Section 2.1"),
    ];
    let bytes = synthesize_pdf_with_outline(5, &outline);
    let dir = std::env::temp_dir().join(format!(
        "tchunk-pdf-test-outline-order-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let input_path = dir.join("input.pdf");
    std::fs::write(&input_path, &bytes).unwrap();

    let pdf = Pdf::load(&input_path).expect("load");
    let entries = pdf.outline_entries();

    let expected: Vec<OutlineEntry> = outline
        .iter()
        .map(|&(d, p, t)| OutlineEntry {
            depth: d,
            page: p,
            title: t.to_string(),
        })
        .collect();
    assert_eq!(entries, expected);
}
```

- [ ] **Step 3: Run the test, observe failure**

Run: `cargo test -p tchunk-pdf --test end_to_end outline_entries_in_document_order`
Expected: FAIL — `outline_entries()` still returns `Vec::new()` (the stub from Task 1).

- [ ] **Step 4: Replace the stub with a real implementation in `pdf.rs`**

Replace the body of `outline_entries()` (added in Task 1) with:

```rust
    pub fn outline_entries(&self) -> Vec<OutlineEntry> {
        let toc = match self.doc.get_toc() {
            Ok(toc) => toc,
            Err(_) => return Vec::new(),
        };
        toc.toc
            .into_iter()
            .filter(|t| t.page >= 1 && t.page <= self.pages.len())
            .map(|t| OutlineEntry {
                depth: t.level as u32,
                page: t.page as u32,
                title: t.title,
            })
            .collect()
    }
```

The page-range filter is defensive — `get_toc` already drops entries whose page IDs don't resolve, but the filter handles the edge case where someone passes an `Object::Integer` destination that resolves to an out-of-range page-number index.

- [ ] **Step 5: Run the test, observe pass**

Run: `cargo test -p tchunk-pdf --test end_to_end outline_entries_in_document_order`
Expected: PASS.

- [ ] **Step 6: Run the full test suite to confirm no regression**

Run: `cargo test -p tchunk-pdf`
Expected: all tests PASS.

- [ ] **Step 7: Commit**

```bash
git add tchunk-pdf/src/pdf.rs tchunk-pdf/tests/end_to_end.rs
git commit -m "[feat] pdf.rs: implement outline_entries via lopdf get_toc"
```

---

## Task 3: Cover title encoding and out-of-range page edge cases for `outline_entries`

Add two more tests that pin down behavior `get_toc` handles for us — title encoding (UTF-16BE BOM and lossy-UTF-8 fallback) and out-of-range pages. These are regression guards, not behavior-change tasks. No production code should change.

**Files:**
- Modify: `tchunk-pdf/tests/end_to_end.rs`

- [ ] **Step 1: Add a UTF-16BE title encoding test**

Append to `tests/end_to_end.rs`:

```rust
#[test]
fn outline_entries_decodes_utf16be_bom_titles() {
    // Build a PDF with a UTF-16BE-encoded title (BOM + big-endian UTF-16). lopdf's get_toc
    // is documented in toc.rs to decode this; the test pins that behavior.
    let dir = std::env::temp_dir().join(format!(
        "tchunk-pdf-test-outline-utf16-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    // Synthesize a base PDF with an ASCII title, then patch the title bytes to UTF-16BE+BOM.
    let bytes = synthesize_pdf_with_outline(2, &[(1, 1, "PLACEHOLDER")]);
    let input_path = dir.join("input.pdf");
    std::fs::write(&input_path, &bytes).unwrap();

    // Reload via lopdf, find the outline item, replace its /Title with a UTF-16BE-encoded
    // string for "Café", and re-save.
    let mut doc = Document::load(&input_path).unwrap();
    let object_ids: Vec<lopdf::ObjectId> = doc.objects.keys().copied().collect();
    for id in object_ids {
        if let Ok(dict) = doc.get_object_mut(id).and_then(Object::as_dict_mut) {
            if dict.has(b"Title") && dict.has(b"Parent") {
                // UTF-16BE BOM (0xFE 0xFF) followed by big-endian UTF-16 bytes for "Café".
                let bytes: Vec<u8> = vec![
                    0xFE, 0xFF, // BOM
                    0x00, 0x43, // 'C'
                    0x00, 0x61, // 'a'
                    0x00, 0x66, // 'f'
                    0x00, 0xE9, // 'é'
                ];
                dict.set("Title", Object::String(bytes, lopdf::StringFormat::Hexadecimal));
            }
        }
    }
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    std::fs::write(&input_path, &out).unwrap();

    let pdf = Pdf::load(&input_path).expect("load");
    let entries = pdf.outline_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title, "Café", "expected UTF-16BE decoded title, got {:?}", entries[0].title);
}
```

You may need to add `use lopdf::StringFormat;` or fully-qualify it as shown — verify the `lopdf` re-exports in the existing imports.

- [ ] **Step 2: Add an out-of-range page test**

Append:

```rust
#[test]
fn outline_entries_skips_out_of_range_pages() {
    // Outline references page 99 in a 3-page document. The entry should be silently dropped.
    let outline: Vec<(u32, u32, &str)> = vec![
        (1, 1, "Real"),
        (1, 99, "Out of range"),
        (1, 3, "Also real"),
    ];
    let bytes = synthesize_pdf_with_outline(3, &outline);
    let dir = std::env::temp_dir().join(format!(
        "tchunk-pdf-test-outline-oor-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let input_path = dir.join("input.pdf");
    std::fs::write(&input_path, &bytes).unwrap();

    let pdf = Pdf::load(&input_path).expect("load");
    let entries = pdf.outline_entries();

    // The "Out of range" entry should be dropped; the other two stay.
    let titles: Vec<&str> = entries.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, vec!["Real", "Also real"]);
}
```

- [ ] **Step 3: Run both new tests, expect pass**

Run: `cargo test -p tchunk-pdf --test end_to_end outline_entries_decodes_utf16be_bom_titles`
Run: `cargo test -p tchunk-pdf --test end_to_end outline_entries_skips_out_of_range_pages`
Expected: both PASS (the implementation in Task 2 should already cover these via `get_toc` and the page-range filter).

If a test fails, debug and fix. The most likely UTF-16BE failure is StringFormat — try `Object::String(bytes, lopdf::StringFormat::Literal)` if Hexadecimal doesn't survive lopdf's save/load roundtrip.

- [ ] **Step 4: Commit**

```bash
git add tchunk-pdf/tests/end_to_end.rs
git commit -m "[test] pdf.rs: pin outline_entries title encoding and out-of-range behavior"
```

---

## Task 4: `inspect.rs` module with `print_histogram` (empty + basic)

Create the new module with `print_histogram`. Cover the empty-outline case and one basic case. Edge-case handling (singular grammar, min/max collapse, zero-count rows) lands in Task 5.

**Files:**
- Create: `tchunk-pdf/src/inspect.rs`
- Modify: `tchunk-pdf/src/lib.rs`

- [ ] **Step 1: Add the new module to `lib.rs`**

In `tchunk-pdf/src/lib.rs`, add:

```rust
pub mod inspect;
```

(Place it alphabetically — between `index` and `pdf`.)

- [ ] **Step 2: Create `tchunk-pdf/src/inspect.rs` with the empty-outline case**

Write the file with this content:

```rust
use std::io::{self, Write};

use crate::pdf::OutlineEntry;

/// Print a depth histogram of the outline to `out`. Header line followed by one row per
/// depth from 1 to max(depth). When `entries` is empty, prints "<page_count> pages, no
/// outline present" and nothing else.
pub fn print_histogram<W: Write>(
    out: &mut W,
    entries: &[OutlineEntry],
    page_count: usize,
) -> io::Result<()> {
    if entries.is_empty() {
        writeln!(out, "{page_count} pages, no outline present")?;
        return Ok(());
    }
    todo!("implement non-empty histogram in next step")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_histogram_no_outline_emits_single_line() {
        let mut buf = Vec::new();
        print_histogram(&mut buf, &[], 423).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "423 pages, no outline present\n");
    }
}
```

- [ ] **Step 3: Run the empty-outline test, expect pass**

Run: `cargo test -p tchunk-pdf --lib inspect::tests::print_histogram_no_outline_emits_single_line`
Expected: PASS.

- [ ] **Step 4: Add a failing test for the basic non-empty case**

Add this test to the `tests` module in `inspect.rs`:

```rust
    #[test]
    fn print_histogram_basic() {
        let entries = vec![
            OutlineEntry { depth: 1, page: 1, title: "C1".into() },
            OutlineEntry { depth: 2, page: 3, title: "C1.1".into() },
            OutlineEntry { depth: 2, page: 5, title: "C1.2".into() },
            OutlineEntry { depth: 1, page: 10, title: "C2".into() },
            OutlineEntry { depth: 2, page: 14, title: "C2.1".into() },
        ];
        let mut buf = Vec::new();
        print_histogram(&mut buf, &entries, 20).unwrap();
        let got = String::from_utf8(buf).unwrap();
        // Header: 20 pages, 5 bookmarks, max depth 2
        // Depth 1: 2 entries → 2 segments at cumulative cuts [page 1, page 10] → spans [1-9, 10-20] → 9 and 11 pages
        // Depth 2: 3 entries → 5 segments at cumulative cuts [1, 3, 5, 10, 14] → spans [1-2, 3-4, 5-9, 10-13, 14-20] → 2, 2, 5, 4, 7
        let expected = "\
20 pages, 5 bookmarks, max depth 2
  at depth 1: 2 bookmarks  → 2 segments, 9-11 pages long
  at depth 2: 3 bookmarks  → 5 segments, 2-7 pages long
";
        assert_eq!(got, expected, "got:\n{got}\nexpected:\n{expected}");
    }
```

- [ ] **Step 5: Run the new test, observe failure**

Run: `cargo test -p tchunk-pdf --lib inspect::tests::print_histogram_basic`
Expected: FAIL with `todo!` panic.

- [ ] **Step 6: Implement the histogram body**

Replace the `todo!(...)` in `print_histogram` with this body:

```rust
    let max_depth = entries.iter().map(|e| e.depth).max().unwrap_or(0);
    let total_bookmarks = entries.len();
    writeln!(
        out,
        "{page_count} pages, {total_bookmarks} {bookmark_word}, max depth {max_depth}",
        bookmark_word = if total_bookmarks == 1 { "bookmark" } else { "bookmarks" },
    )?;

    // Count column width — width of the largest at-depth count, for right-alignment.
    let mut counts_per_depth: Vec<usize> = vec![0; (max_depth + 1) as usize];
    for e in entries {
        counts_per_depth[e.depth as usize] += 1;
    }
    let max_count = counts_per_depth.iter().copied().max().unwrap_or(0);
    let count_width = max_count.to_string().len();

    // For each depth d in 1..=max_depth, compute cumulative cut points (every entry at
    // depth ≤ d is a cut point) and the segment min/max page span. The pages array goes
    // from 1 to page_count inclusive; each cut point starts a new segment, and the final
    // segment runs to page_count.
    for d in 1..=max_depth {
        let count_at_d = counts_per_depth[d as usize];
        // Cut-point pages, in document order, for entries with depth ≤ d.
        let mut cuts: Vec<u32> = entries.iter().filter(|e| e.depth <= d).map(|e| e.page).collect();
        cuts.sort_unstable();
        cuts.dedup();
        let segments = cuts.len();

        // Span of each segment: cuts[i+1] - cuts[i] for i in 0..segments-1, plus
        // (page_count + 1 - cuts[last]) for the trailing segment.
        let mut min_span: u32 = u32::MAX;
        let mut max_span: u32 = 0;
        for i in 0..cuts.len() {
            let span = if i + 1 < cuts.len() {
                cuts[i + 1] - cuts[i]
            } else {
                (page_count as u32 + 1).saturating_sub(cuts[i])
            };
            if span < min_span { min_span = span; }
            if span > max_span { max_span = span; }
        }

        let bookmark_word = if count_at_d == 1 { "bookmark" } else { "bookmarks" };
        let segment_word = if segments == 1 { "segment" } else { "segments" };
        let span_text = format_span(min_span, max_span);
        writeln!(
            out,
            "  at depth {d}: {count_at_d:>count_width$} {bookmark_word}  → {segments} {segment_word}, {span_text}",
        )?;
    }

    Ok(())
}

fn format_span(min: u32, max: u32) -> String {
    if min == max {
        let unit = if min == 1 { "page" } else { "pages" };
        format!("{min} {unit} long")
    } else {
        format!("{min}-{max} pages long")
    }
}
```

- [ ] **Step 7: Run both inspect tests, expect pass**

Run: `cargo test -p tchunk-pdf --lib inspect`
Expected: both `print_histogram_no_outline_emits_single_line` and `print_histogram_basic` PASS.

If `print_histogram_basic` fails, the most likely culprit is segment-span math — recompute the expected output by hand against the code logic and reconcile.

- [ ] **Step 8: Commit**

```bash
git add tchunk-pdf/src/inspect.rs tchunk-pdf/src/lib.rs
git commit -m "[feat] inspect.rs: print_histogram for empty and basic outline"
```

---

## Task 5: `print_histogram` edge cases (singular grammar, min==max collapse, zero-count rows)

Add three regression tests. The implementation from Task 4 should already cover all three (via `format_span` collapse + the bookmark/segment word helpers + iterating `1..=max_depth`), so this task is primarily verification. If any test fails, fix the implementation.

**Files:**
- Modify: `tchunk-pdf/src/inspect.rs`

- [ ] **Step 1: Add singular-grammar test**

Add to the `tests` module in `inspect.rs`:

```rust
    #[test]
    fn print_histogram_singular_grammar() {
        // Single bookmark at depth 1 in a 1-page doc → "1 bookmark", "1 segment", "1 page long".
        let entries = vec![OutlineEntry { depth: 1, page: 1, title: "Only".into() }];
        let mut buf = Vec::new();
        print_histogram(&mut buf, &entries, 1).unwrap();
        let got = String::from_utf8(buf).unwrap();
        let expected = "\
1 pages, 1 bookmark, max depth 1
  at depth 1: 1 bookmark  → 1 segment, 1 page long
";
        assert_eq!(got, expected, "got:\n{got}\nexpected:\n{expected}");
    }
```

- [ ] **Step 2: Add min==max collapse test**

Add:

```rust
    #[test]
    fn print_histogram_min_eq_max_collapse() {
        // Three depth-1 entries at pages 1, 4, 7 in a 9-page doc.
        // Cut points: [1, 4, 7]. Segments: [1-3, 4-6, 7-9] = 3 pages each.
        let entries = vec![
            OutlineEntry { depth: 1, page: 1, title: "A".into() },
            OutlineEntry { depth: 1, page: 4, title: "B".into() },
            OutlineEntry { depth: 1, page: 7, title: "C".into() },
        ];
        let mut buf = Vec::new();
        print_histogram(&mut buf, &entries, 9).unwrap();
        let got = String::from_utf8(buf).unwrap();
        let expected = "\
9 pages, 3 bookmarks, max depth 1
  at depth 1: 3 bookmarks  → 3 segments, 3 pages long
";
        assert_eq!(got, expected, "got:\n{got}\nexpected:\n{expected}");
    }
```

- [ ] **Step 3: Add zero-count rows test**

Add:

```rust
    #[test]
    fn print_histogram_includes_zero_count_rows() {
        // Outline has depth-1 and depth-3 entries but no depth-2 entries.
        // Expect a depth-2 row with "0 bookmarks" and segment count = cumulative through depth-2,
        // which equals depth-1 count since no new cuts are added at depth-2.
        let entries = vec![
            OutlineEntry { depth: 1, page: 1, title: "A".into() },
            OutlineEntry { depth: 3, page: 5, title: "A.1.1".into() },
            OutlineEntry { depth: 1, page: 10, title: "B".into() },
        ];
        let mut buf = Vec::new();
        print_histogram(&mut buf, &entries, 20).unwrap();
        let got = String::from_utf8(buf).unwrap();
        // Depth 1: 2 entries → cuts [1, 10] → segments of 9 and 11 pages
        // Depth 2: 0 entries → cumulative still 2 segments, same spans 9-11
        // Depth 3: 1 entry → cuts [1, 5, 10] → spans [4, 5, 11]
        let expected = "\
20 pages, 3 bookmarks, max depth 3
  at depth 1: 2 bookmarks  → 2 segments, 9-11 pages long
  at depth 2: 0 bookmarks  → 2 segments, 9-11 pages long
  at depth 3: 1 bookmark  → 3 segments, 4-11 pages long
";
        assert_eq!(got, expected, "got:\n{got}\nexpected:\n{expected}");
    }
```

- [ ] **Step 4: Run all three new tests, expect pass**

Run: `cargo test -p tchunk-pdf --lib inspect::tests`
Expected: all five tests (the two from Task 4 plus three new) PASS.

If a test fails, debug the implementation. Likely culprits: forgot the `count_width` alignment when count is 0 (verify it still right-aligns), forgot to special-case `1 segment` vs `1 segments`, math off-by-one in span computation.

- [ ] **Step 5: Commit**

```bash
git add tchunk-pdf/src/inspect.rs
git commit -m "[test] inspect.rs: pin histogram singular, collapse, and zero-count behaviors"
```

---

## Task 6: `inspect.rs` `print_tree` — empty + basic + edge cases

Add the second public function. Includes empty-outline case, basic indented output, page-prefix width sizing, and `(untitled)` rendering for empty titles.

**Files:**
- Modify: `tchunk-pdf/src/inspect.rs`

- [ ] **Step 1: Add `print_tree` skeleton with the empty-outline case**

Add to `inspect.rs` (above the `#[cfg(test)] mod tests`):

```rust
/// Print an indented outline tree to `out`. Each line is `[pN] <indent><title>`, with
/// `[pN]` left-aligned and width-padded to the document's max page count, and `<indent>`
/// = 2 spaces per depth level (depth 1 = 0 indent, depth 2 = 2 spaces, etc.). Empty
/// `entries` prints "<page_count> pages, no outline present".
pub fn print_tree<W: Write>(
    out: &mut W,
    entries: &[OutlineEntry],
    page_count: usize,
) -> io::Result<()> {
    if entries.is_empty() {
        writeln!(out, "{page_count} pages, no outline present")?;
        return Ok(());
    }
    let prefix_width = page_prefix_width(page_count);
    for e in entries {
        let prefix = format!("[p{}]", e.page);
        let pad = prefix_width.saturating_sub(prefix.len());
        let indent = "  ".repeat((e.depth.saturating_sub(1)) as usize);
        let title = if e.title.is_empty() { "(untitled)" } else { e.title.as_str() };
        writeln!(out, "{prefix}{} {indent}{title}", " ".repeat(pad))?;
    }
    Ok(())
}

/// Width in columns of "[pN]" sized for the largest page number. e.g. page_count = 423
/// → "[p423]" → 6.
fn page_prefix_width(page_count: usize) -> usize {
    let digits = if page_count == 0 {
        1
    } else {
        (page_count as f64).log10() as usize + 1
    };
    "[p".len() + digits + "]".len()
}
```

- [ ] **Step 2: Add tests for `print_tree`**

Append to the existing `mod tests` in `inspect.rs`:

```rust
    #[test]
    fn print_tree_no_outline() {
        let mut buf = Vec::new();
        print_tree(&mut buf, &[], 100).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "100 pages, no outline present\n");
    }

    #[test]
    fn print_tree_basic() {
        let entries = vec![
            OutlineEntry { depth: 1, page: 1, title: "Front Matter".into() },
            OutlineEntry { depth: 2, page: 3, title: "Acknowledgements".into() },
            OutlineEntry { depth: 1, page: 9, title: "Chapter 1".into() },
            OutlineEntry { depth: 2, page: 11, title: "1.1 Background".into() },
        ];
        let mut buf = Vec::new();
        print_tree(&mut buf, &entries, 423).unwrap();
        let got = String::from_utf8(buf).unwrap();
        // page_prefix_width("423") = 6 columns. "[p1]  " (4 chars + 2 spaces of pad)
        // for the first entry, "[p3]  " for the second, "[p11] " for the fourth, etc.
        let expected = "\
[p1]   Front Matter
[p3]     Acknowledgements
[p9]   Chapter 1
[p11]   1.1 Background
";
        assert_eq!(got, expected, "got:\n{got:?}\nexpected:\n{expected:?}");
    }

    #[test]
    fn print_tree_empty_title_renders_untitled() {
        let entries = vec![
            OutlineEntry { depth: 1, page: 1, title: "".into() },
        ];
        let mut buf = Vec::new();
        print_tree(&mut buf, &entries, 9).unwrap();
        let got = String::from_utf8(buf).unwrap();
        // page_prefix_width(9) = 4. "[p1] " = 4 chars + 1 pad space.
        let expected = "[p1]  (untitled)\n";
        assert_eq!(got, expected, "got:\n{got:?}\nexpected:\n{expected:?}");
    }
```

- [ ] **Step 3: Run all `inspect` tests, expect all pass**

Run: `cargo test -p tchunk-pdf --lib inspect::tests`
Expected: all 8 tests (5 histogram + 3 tree) PASS.

The most likely failure mode for `print_tree_basic` is column alignment. Re-derive expected output by hand: `[p1]` is 4 chars; `prefix_width` for page_count=423 is 6 (`"[p"=2 + log10(423).floor()+1=3 + "]"=1`). Pad = 6 - 4 = 2 spaces after `[p1]`. Then 1 space separator before indent. Then `indent = ""` for depth 1. Total: `"[p1]" + "  " + " " + "" + "Front Matter"` = `"[p1]   Front Matter"`. Three spaces between `]` and `F`. Match what's in the expected string.

- [ ] **Step 4: Commit**

```bash
git add tchunk-pdf/src/inspect.rs
git commit -m "[feat] inspect.rs: print_tree with indentation and untitled fallback"
```

---

## Task 7: CLI flags `--bookmarks-hist` and `--bookmarks-tree`

Add the two boolean flags to `Cli`. Update `Cli::validate()` so the HuggingFace tokenizer source check is skipped in inspection mode (no tokenizer is constructed). Add unit tests covering parse, mutual independence, and validate behavior.

**Files:**
- Modify: `tchunk-pdf/src/cli.rs`

- [ ] **Step 1: Add the two fields to `Cli`**

In `tchunk-pdf/src/cli.rs`, add these two fields to the `Cli` struct, after the existing `pub jobs: usize` field (after line 130):

```rust
    /// Print a depth histogram of the input's outline and exit. Combinable with
    /// --bookmarks-tree. Inspection mode skips chunking entirely; --max-tokens,
    /// --split-at, --tokenizer, --output-dir, and --prefix are not used.
    #[arg(long = "bookmarks-hist")]
    pub bookmarks_hist: bool,

    /// Print the full indented outline tree with page numbers and exit. Combinable
    /// with --bookmarks-hist. Inspection mode skips chunking entirely.
    #[arg(long = "bookmarks-tree")]
    pub bookmarks_tree: bool,
```

- [ ] **Step 2: Add an `inspection_mode` helper and update `validate()`**

In the `impl Cli { ... }` block, add this helper near the top (before `resolved_split_at`):

```rust
    /// Whether either inspection flag is set. When true, `main.rs` takes the inspection
    /// path and bypasses tokenizer construction, planning, chunk writing, and the index
    /// sidecar.
    pub fn inspection_mode(&self) -> bool {
        self.bookmarks_hist || self.bookmarks_tree
    }
```

Then update `validate()` to skip the HuggingFace source check in inspection mode. Replace the existing tokenizer match (around lines 160-170):

```rust
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
```

with:

```rust
        // Inspection mode never constructs a tokenizer, so the tokenizer/HF-source
        // consistency check would only produce confusing errors for users who set
        // --tokenizer alongside --bookmarks-hist. Skip it in that mode.
        if !self.inspection_mode() {
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
        }
```

- [ ] **Step 3: Add unit tests in `cli.rs`'s `tests` module**

Append these tests to the existing `#[cfg(test)] mod tests` block in `cli.rs` (after line 326, before the closing `}`):

```rust
    #[test]
    fn bookmarks_hist_flag_parses() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["tchunk-pdf", "input.pdf", "--bookmarks-hist"])
            .expect("parse");
        assert!(cli.bookmarks_hist);
        assert!(!cli.bookmarks_tree);
        assert!(cli.inspection_mode());
    }

    #[test]
    fn bookmarks_tree_flag_parses() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["tchunk-pdf", "input.pdf", "--bookmarks-tree"])
            .expect("parse");
        assert!(!cli.bookmarks_hist);
        assert!(cli.bookmarks_tree);
        assert!(cli.inspection_mode());
    }

    #[test]
    fn bookmarks_flags_are_combinable() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "tchunk-pdf",
            "input.pdf",
            "--bookmarks-hist",
            "--bookmarks-tree",
        ])
        .expect("parse");
        assert!(cli.bookmarks_hist);
        assert!(cli.bookmarks_tree);
    }

    #[test]
    fn inspection_mode_skips_hf_source_validation() {
        use clap::Parser;
        // -t huggingface without a source would normally fail validation, but in
        // inspection mode the tokenizer is never constructed so the check is skipped.
        // We need a real input path for validate() to pass its other checks; use a
        // path that exists. Cargo.toml is always present at the workspace root.
        let mut cli = Cli::try_parse_from([
            "tchunk-pdf",
            "Cargo.toml",
            "-t",
            "huggingface",
            "--bookmarks-hist",
        ])
        .expect("parse");
        assert!(cli.validate().is_ok(), "expected validate to pass in inspection mode");
    }

    #[test]
    fn non_inspection_mode_still_enforces_hf_source() {
        use clap::Parser;
        let mut cli = Cli::try_parse_from(["tchunk-pdf", "Cargo.toml", "-t", "huggingface"])
            .expect("parse");
        let err = cli.validate().expect_err("expected error without HF source in chunking mode");
        assert!(err.to_string().contains("requires --tokenizer-file"));
    }

    #[test]
    fn no_inspection_flags_means_chunking_mode() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["tchunk-pdf", "input.pdf"]).expect("parse");
        assert!(!cli.inspection_mode());
    }
```

Note: the inspection-mode-validate test uses `Cargo.toml` as a stand-in for an existing path — it doesn't matter that it's not a PDF, because validation only checks path existence/glob expansion, not PDF parseability. (Actual PDF loading happens later in `run()`.)

- [ ] **Step 4: Run the new CLI tests, expect pass**

Run: `cargo test -p tchunk-pdf --lib cli::tests`
Expected: all CLI tests (existing + 6 new) PASS.

- [ ] **Step 5: Commit**

```bash
git add tchunk-pdf/src/cli.rs
git commit -m "[feat] cli: add --bookmarks-hist and --bookmarks-tree flags"
```

---

## Task 8: `run_inspect()` and branching in `run()`

Wire the inspection path into `main.rs`. After `cli.validate()`, branch on `cli.inspection_mode()` to call a new `run_inspect()` that loads each input, fetches outline entries, and prints to stdout via the inspect module.

**Files:**
- Modify: `tchunk-pdf/src/main.rs`

- [ ] **Step 1: Add the imports and `run_inspect` function to `main.rs`**

In the top imports of `main.rs`, add:

```rust
use std::io::{self, Write};
use tchunk_pdf::inspect;
```

Place `std::io::{self, Write}` with the other `std::*` imports (after the existing `std::path::*` line). Place `tchunk_pdf::inspect` with the other `tchunk_pdf::*` imports.

The existing `use tchunk_pdf::pdf::Pdf;` does not need to change — `OutlineEntry` is consumed inside the `inspect` module, not in `main.rs`.

Now add `run_inspect` immediately after the existing `run()` function (insert before `fn process_input`):

```rust
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
```

- [ ] **Step 2: Add the inspection branch to `run()`**

In the existing `run()` function (starts around line 43), insert this branch immediately after `cli.validate().map_err(RunError::Input)?;` (after line 44):

```rust
    if cli.inspection_mode() {
        return run_inspect(&cli);
    }
```

- [ ] **Step 3: Verify the chunking path still compiles**

Run: `cargo check -p tchunk-pdf`
Expected: clean compile, possibly with one new dead-code warning if `OutlineEntry` was added incorrectly. Resolve any warnings/errors before continuing.

- [ ] **Step 4: Smoke-test inspection mode against a known fixture**

Build the binary:

Run: `cargo build -p tchunk-pdf --release`

Run a smoke test with the existing test infrastructure — the simplest path is to add a temporary inline test, but for a quick manual check, use any PDF you have on the system. If you don't have one, skip to Step 5 (Task 9 covers this with formal integration tests).

If you do have a PDF available, try:

Run: `./target/release/tchunk-pdf <some.pdf> --bookmarks-hist`
Expected: prints the histogram to stdout, no chunk PDFs created in cwd, exit 0.

Run: `./target/release/tchunk-pdf <some.pdf> --bookmarks-tree | head -20`
Expected: prints up to 20 lines of indented outline.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test -p tchunk-pdf`
Expected: all tests PASS.

- [ ] **Step 6: Commit**

```bash
git add tchunk-pdf/src/main.rs
git commit -m "[feat] main: branch into run_inspect() when --bookmarks-* flag is set"
```

---

## Task 9: Integration tests for inspection mode

Add three integration tests covering: no chunks/sidecar in inspection mode, multi-file framing, and combined-flag ordering.

**Files:**
- Modify: `tchunk-pdf/tests/end_to_end.rs`

- [ ] **Step 1: Add test for "no chunks or sidecar produced in inspection mode"**

Append to `tests/end_to_end.rs`:

```rust
#[test]
fn inspection_mode_writes_no_chunks_or_sidecar() {
    let outline: Vec<(u32, u32, &str)> = vec![
        (1, 1, "Chapter 1"),
        (2, 2, "Section 1.1"),
        (1, 3, "Chapter 2"),
    ];
    let bytes = synthesize_pdf_with_outline(3, &outline);
    let dir = std::env::temp_dir().join(format!(
        "tchunk-pdf-test-inspect-no-chunks-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let input_path = dir.join("input.pdf");
    std::fs::write(&input_path, &bytes).unwrap();

    // Run the binary with --bookmarks-hist. Use cargo's compiled binary path.
    let bin_path = env!("CARGO_BIN_EXE_tchunk-pdf");
    let output = Command::new(bin_path)
        .arg(&input_path)
        .arg("--bookmarks-hist")
        .arg("--output-dir")
        .arg(&dir)
        .output()
        .expect("run tchunk-pdf");
    assert!(output.status.success(), "non-zero exit: stderr={}",
        String::from_utf8_lossy(&output.stderr));

    // No PDF chunks should have been created in the output dir (other than the input).
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    assert!(
        !entries.iter().any(|n| n != "input.pdf" && n.ends_with(".pdf")),
        "unexpected chunk PDF created: {entries:?}",
    );
    assert!(
        !entries.iter().any(|n| n.ends_with(".index.json")),
        "unexpected sidecar created: {entries:?}",
    );

    // Stdout should contain the histogram body.
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("3 pages, 3 bookmarks, max depth 2"),
        "missing histogram header in stdout: {stdout}");
    assert!(stdout.contains("at depth 1: 2 bookmarks"),
        "missing depth-1 row in stdout: {stdout}");
    assert!(stdout.contains("at depth 2: 1 bookmark"),
        "missing depth-2 row in stdout: {stdout}");
}
```

- [ ] **Step 2: Add multi-file framing test**

Append:

```rust
#[test]
fn inspection_mode_multi_file_framing() {
    let dir = std::env::temp_dir().join(format!(
        "tchunk-pdf-test-inspect-multi-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let outline_a: Vec<(u32, u32, &str)> = vec![(1, 1, "A.Ch1")];
    let outline_b: Vec<(u32, u32, &str)> = vec![(1, 1, "B.Ch1")];
    let path_a = dir.join("a.pdf");
    let path_b = dir.join("b.pdf");
    std::fs::write(&path_a, synthesize_pdf_with_outline(2, &outline_a)).unwrap();
    std::fs::write(&path_b, synthesize_pdf_with_outline(2, &outline_b)).unwrap();

    let bin_path = env!("CARGO_BIN_EXE_tchunk-pdf");
    let output = Command::new(bin_path)
        .arg(&path_a)
        .arg(&path_b)
        .arg("--bookmarks-hist")
        .output()
        .expect("run tchunk-pdf");
    assert!(output.status.success(), "non-zero exit: stderr={}",
        String::from_utf8_lossy(&output.stderr));

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("=== ") && stdout.contains("(1/2) ==="),
        "missing first-file frame: {stdout}");
    assert!(stdout.contains("(2/2) ==="),
        "missing second-file frame: {stdout}");
    // Per-file blocks should be separated by a blank line.
    assert!(stdout.contains("\n\n==="),
        "expected blank line between per-file blocks: {stdout:?}");
}
```

- [ ] **Step 3: Add combined-flag ordering test**

Append:

```rust
#[test]
fn inspection_mode_combined_flags_emit_histogram_then_tree() {
    let outline: Vec<(u32, u32, &str)> = vec![
        (1, 1, "Chapter 1"),
        (2, 2, "Section 1.1"),
    ];
    let bytes = synthesize_pdf_with_outline(2, &outline);
    let dir = std::env::temp_dir().join(format!(
        "tchunk-pdf-test-inspect-combined-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let input_path = dir.join("input.pdf");
    std::fs::write(&input_path, &bytes).unwrap();

    let bin_path = env!("CARGO_BIN_EXE_tchunk-pdf");
    let output = Command::new(bin_path)
        .arg(&input_path)
        .arg("--bookmarks-hist")
        .arg("--bookmarks-tree")
        .output()
        .expect("run tchunk-pdf");
    assert!(output.status.success(), "non-zero exit: stderr={}",
        String::from_utf8_lossy(&output.stderr));

    let stdout = String::from_utf8(output.stdout).unwrap();
    let hist_idx = stdout.find("at depth 1:").expect("histogram missing from stdout");
    let tree_idx = stdout.find("[p1]").expect("tree missing from stdout");
    assert!(hist_idx < tree_idx,
        "expected histogram block before tree block in stdout:\n{stdout}");
}
```

- [ ] **Step 4: Run all integration tests, expect pass**

Run: `cargo test -p tchunk-pdf --test end_to_end`
Expected: all tests PASS.

- [ ] **Step 5: Run the full test suite as a final regression check**

Run: `cargo test -p tchunk-pdf`
Expected: all tests PASS.

- [ ] **Step 6: Commit**

```bash
git add tchunk-pdf/tests/end_to_end.rs
git commit -m "[test] e2e: inspection mode produces no chunks/sidecar, multi-file framing, combined flags"
```

---

## Task 10: README updates

Document the two new flags and the synthetic-marker caveat in the README.

**Files:**
- Modify: `tchunk-pdf/README.md`

- [ ] **Step 1: Locate the existing flag documentation in the README**

Run: `grep -n '\-\-split-at' tchunk-pdf/README.md` to find the section that documents flags.

(If the README structure has changed, adapt the placement of the new section to match the existing organization.)

- [ ] **Step 2: Add a new section to the README**

Add a new section (typically before or after the `--split-at-depth` documentation) titled "Inspecting a PDF's outline" with content like:

```markdown
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
```

- [ ] **Step 3: Verify the README still renders**

Run: `cargo doc -p tchunk-pdf --no-deps` (this isn't strictly necessary for README changes, but it confirms the crate still builds clean with all changes).

Or just visually check the rendered Markdown if your editor supports it.

- [ ] **Step 4: Run all tests one final time**

Run: `cargo test -p tchunk-pdf`
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add tchunk-pdf/README.md
git commit -m "[docs] document --bookmarks-hist and --bookmarks-tree"
```

---

## Self-review checklist (run after all tasks)

After completing all tasks, verify against the spec:

- [ ] `OutlineEntry` struct fields match spec: `depth: u32`, `page: u32`, `title: String`.
- [ ] `Pdf::outline_entries()` returns empty Vec when no outline; entries in document order otherwise.
- [ ] `inspect::print_histogram` and `inspect::print_tree` both take `&mut impl Write` and return `io::Result<()>`.
- [ ] Histogram includes a row for every depth in `1..=max_depth` (zero-count rows present).
- [ ] Histogram singular forms: `1 bookmark`, `1 segment`, `1 page long`.
- [ ] Histogram min == max collapses to `N pages long`.
- [ ] Tree page-prefix is left-aligned and width-padded to `[p<max_page_num>]`.
- [ ] Tree indent is 2 spaces per depth level (depth 1 = 0 indent).
- [ ] Tree empty title renders as `(untitled)`.
- [ ] CLI flags `--bookmarks-hist` and `--bookmarks-tree` are independent and combinable.
- [ ] `Cli::validate` skips the HF source check in inspection mode.
- [ ] `main::run()` branches into `run_inspect()` when either inspection flag is set.
- [ ] Inspection mode produces no chunk PDFs and no `.index.json` sidecar.
- [ ] Multi-file inspection uses the `=== file.pdf (i/N) ===` framing, separated by blank lines.
- [ ] Output goes to stdout; only genuine errors go to stderr.
- [ ] README documents both flags and the synthetic-marker caveat.
