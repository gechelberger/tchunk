use std::path::PathBuf;
use std::process::Command;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};

use tchunk_pdf::pdf::{OutlineEntry, Pdf};
use tchunk_pdf::plan::{plan_chunks, Boundary, SplitAt};
use tchunk_pdf::tokenize::{TiktokenTokenizer, Tokenizer};

/// Build a synthetic N-page PDF where each page contains the text "Page <n>". Returns the bytes.
fn synthesize_pdf(page_count: usize) -> Vec<u8> {
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
        "Kids" => page_ids,
        "Count" => page_count as i64,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc.compress();

    let mut out = Vec::new();
    doc.save_to(&mut out).expect("save_to memory buffer");
    out
}

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

    // A dummy object used as the page reference for out-of-range entries. lopdf's get_toc
    // requires all page destinations to be Object::Reference (it calls as_reference()?), so we
    // use a valid reference to a non-page object rather than an Object::Integer. The dummy ID
    // won't be in page_id_to_page_numbers, so get_toc silently skips such entries.
    let dummy_id = doc.add_object(Object::Null);

    // Build parent/child links by walking entries with a depth stack.
    // `parent_at_depth[d]` = item id of the most-recent open item at depth d (if any);
    //   read on the `else` branch below to locate the parent of each deeper entry.
    let mut parent_at_depth: Vec<Option<lopdf::ObjectId>> = vec![None; 32];
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
            // Out-of-range: use a reference to the dummy object. lopdf's get_toc calls
            // as_reference()? on the page field, so Object::Integer would cause the entire
            // get_toc call to fail. A reference to a non-page object passes as_reference()
            // but isn't found in page_id_to_page_numbers, so get_toc silently skips it.
            Object::Reference(dummy_id)
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

#[test]
fn split_six_page_pdf_into_three_chunks_preserves_pages() {
    // 6-page synthetic PDF, ~5 tokens per page. Budget 10 -> roughly 3 chunks.
    let bytes = synthesize_pdf(6);
    let dir = std::env::temp_dir().join(format!(
        "tchunk-pdf-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let input_path = dir.join("input.pdf");
    std::fs::write(&input_path, &bytes).unwrap();

    let pdf = Pdf::load(&input_path).expect("load synthetic pdf");
    assert_eq!(pdf.page_count(), 6);

    let tok = TiktokenTokenizer::new("cl100k_base").unwrap();
    let tokens: Vec<usize> = pdf
        .page_nums()
        .iter()
        .map(|&n| tok.count(&pdf.page_text(n)))
        .collect();
    let boundaries = vec![Boundary::Page; 6];

    // Force multi-chunk: budget = roughly 2 pages worth.
    let budget = tokens.iter().sum::<usize>().div_ceil(3).max(1);
    let plan = plan_chunks(&tokens, &boundaries, SplitAt::Page, budget);

    // Sum of chunk pages == input pages, each page appears exactly once, ordered.
    let flat: Vec<u32> = plan.chunks.iter().flat_map(|c| c.pages.clone()).collect();
    assert_eq!(flat, (1..=6).collect::<Vec<_>>(), "pages reordered or lost");
    assert!(plan.chunks.len() >= 2, "expected multiple chunks, got {}", plan.chunks.len());

    // Write each chunk and verify its page count matches the plan.
    for (i, chunk) in plan.chunks.iter().enumerate() {
        let page_nums = &chunk.pages;
        let out_path: PathBuf = dir.join(format!("out_{}.pdf", i + 1));
        pdf.write_chunk(page_nums, &out_path).expect("write_chunk");

        let out_pdf = Pdf::load(&out_path).expect("reload chunk");
        assert_eq!(
            out_pdf.page_count(),
            page_nums.len(),
            "chunk {} page count mismatch",
            i + 1
        );

        // Verify the right pages ended up in the chunk by checking extracted text.
        let out_page_nums = out_pdf.page_nums();
        for (k, &orig_page) in page_nums.iter().enumerate() {
            let want = format!("Page {orig_page}");
            let got = out_pdf.page_text(out_page_nums[k]);
            assert!(
                got.contains(&want),
                "chunk {} position {} expected text containing '{want}', got '{}'",
                i + 1,
                k,
                got
            );
        }
    }
}

#[test]
fn single_chunk_when_budget_exceeds_total() {
    let bytes = synthesize_pdf(3);
    let dir = std::env::temp_dir().join(format!(
        "tchunk-pdf-test-single-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let input_path = dir.join("input.pdf");
    std::fs::write(&input_path, &bytes).unwrap();

    let pdf = Pdf::load(&input_path).expect("load");
    let tok = TiktokenTokenizer::new("cl100k_base").unwrap();
    let tokens: Vec<usize> = pdf
        .page_nums()
        .iter()
        .map(|&n| tok.count(&pdf.page_text(n)))
        .collect();
    let boundaries = vec![Boundary::Page; 3];

    let plan = plan_chunks(&tokens, &boundaries, SplitAt::Page, 10_000);
    assert_eq!(plan.chunks.len(), 1);
    assert_eq!(plan.chunks[0].pages, vec![1, 2, 3]);

    let out_path = dir.join("out.pdf");
    pdf.write_chunk(&plan.chunks[0].pages, &out_path).unwrap();
    let reloaded = Pdf::load(&out_path).unwrap();
    assert_eq!(reloaded.page_count(), 3);
}

/// Build a synthetic 6-page PDF whose catalog has an `/OpenAction` targeting page 5. Used to
/// verify that subsetting drops the catalog action so a dropped page can't survive in the chunk
/// via `prune_objects` reachability.
fn synthesize_pdf_with_open_action(page_count: usize, target_page: usize) -> Vec<u8> {
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
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![100.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal(format!("Page {i}"))]),
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

    let target_page_id = match &page_ids[target_page - 1] {
        Object::Reference(id) => *id,
        _ => unreachable!("page_ids contains References"),
    };

    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => page_ids,
        "Count" => page_count as i64,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "OpenAction" => vec![Object::Reference(target_page_id), "Fit".into()],
    });
    doc.trailer.set("Root", catalog_id);
    doc.compress();

    let mut out = Vec::new();
    doc.save_to(&mut out).expect("save_to memory buffer");
    out
}

#[test]
fn subset_strips_catalog_open_action_referencing_dropped_page() {
    let bytes = synthesize_pdf_with_open_action(6, 5);
    let dir = std::env::temp_dir().join(format!(
        "tchunk-pdf-test-catalog-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let input_path = dir.join("input.pdf");
    std::fs::write(&input_path, &bytes).unwrap();

    let pdf = Pdf::load(&input_path).expect("load");
    assert_eq!(pdf.page_count(), 6);

    let out_path = dir.join("chunk.pdf");
    pdf.write_chunk(&[1, 2], &out_path).expect("write_chunk");

    let out_doc = Document::load(&out_path).expect("reload chunk");
    let catalog_id = out_doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("catalog ref in trailer");
    let catalog = out_doc.get_dictionary(catalog_id).expect("catalog dict");
    assert!(
        catalog.get(b"OpenAction").is_err(),
        "catalog still carries /OpenAction after subset"
    );

    let pages = out_doc.get_pages();
    assert_eq!(pages.len(), 2, "expected 2 pages in chunk, got {}", pages.len());
}

#[test]
fn cli_writes_index_sidecar_with_chunk_entries() {
    let bytes = synthesize_pdf(6);
    let dir = std::env::temp_dir().join(format!(
        "tchunk-pdf-test-index-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let input_path = dir.join("smoke.pdf");
    std::fs::write(&input_path, &bytes).unwrap();

    let out_dir = dir.join("out");
    let bin = env!("CARGO_BIN_EXE_tchunk-pdf");
    let status = Command::new(bin)
        .arg(&input_path)
        .arg("-m")
        .arg("10")
        .arg("-o")
        .arg(&out_dir)
        .arg("-t")
        .arg("word_count")
        .arg("--split-at")
        .arg("page")
        .status()
        .expect("spawn tchunk-pdf");
    assert!(status.success(), "binary exited non-zero: {status:?}");

    let index_path = out_dir.join("smoke.index.json");
    let json_text = std::fs::read_to_string(&index_path)
        .unwrap_or_else(|e| panic!("sidecar missing at {}: {e}", index_path.display()));
    let v: serde_json::Value = serde_json::from_str(&json_text).expect("valid JSON");

    assert_eq!(v["tool"], "tchunk-pdf");
    assert_eq!(v["source"]["page_count"], 6);
    assert_eq!(v["config"]["tokenizer"], "word_count");
    assert_eq!(v["config"]["max_tokens"], 10);
    assert_eq!(v["config"]["split_at_requested"], "page");
    assert_eq!(v["config"]["split_at_effective"], "page");

    let chunks = v["chunks"].as_array().expect("chunks array");
    assert!(!chunks.is_empty());

    // Chunks must cover pages 1..=6 contiguously with no gaps/overlap, and each must carry a
    // per-chunk effective_level in the sidecar.
    let mut expected_next: u64 = 1;
    for c in chunks {
        let start = c["pages"]["start"].as_u64().unwrap();
        let end = c["pages"]["end"].as_u64().unwrap();
        let count = c["pages"]["count"].as_u64().unwrap();
        assert_eq!(start, expected_next, "chunk start/gap mismatch");
        assert_eq!(end - start + 1, count, "pages.count mismatch");
        assert!(c["filename"].as_str().unwrap().ends_with(".pdf"));
        assert_eq!(
            c["effective_level"].as_str(),
            Some("page"),
            "chunk missing or wrong effective_level: {c:?}"
        );
        expected_next = end + 1;
    }
    assert_eq!(expected_next, 7, "chunks did not cover all 6 pages");
}

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
                // Hexadecimal survives lopdf's save/load roundtrip; Literal is the alternative
                // per the design plan if a future lopdf version drops hex support.
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
    assert_eq!(entries[0].page, 1);
    assert_eq!(entries[1].page, 3);
}

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
