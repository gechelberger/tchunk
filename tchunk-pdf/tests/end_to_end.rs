use std::path::PathBuf;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};

use tchunk_pdf::pdf::Pdf;
use tchunk_pdf::plan::{plan_chunks, BoundaryLevel};
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
    let texts = pdf.page_texts();
    let tokens: Vec<usize> = texts.iter().map(|t| tok.count(t)).collect();
    let boundaries = vec![BoundaryLevel::Page; 6];

    // Force multi-chunk: budget = roughly 2 pages worth.
    let budget = tokens.iter().sum::<usize>().div_ceil(3).max(1);
    let plan = plan_chunks(&tokens, &boundaries, BoundaryLevel::Page, budget);

    // Sum of chunk pages == input pages, each page appears exactly once, ordered.
    let flat: Vec<u32> = plan.chunks.iter().flatten().copied().collect();
    assert_eq!(flat, (1..=6).collect::<Vec<_>>(), "pages reordered or lost");
    assert!(plan.chunks.len() >= 2, "expected multiple chunks, got {}", plan.chunks.len());

    // Write each chunk and verify its page count matches the plan.
    for (i, page_nums) in plan.chunks.iter().enumerate() {
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
        let out_texts = out_pdf.page_texts();
        for (k, &orig_page) in page_nums.iter().enumerate() {
            let want = format!("Page {orig_page}");
            assert!(
                out_texts[k].contains(&want),
                "chunk {} position {} expected text containing '{want}', got '{}'",
                i + 1,
                k,
                out_texts[k]
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
    let texts = pdf.page_texts();
    let tokens: Vec<usize> = texts.iter().map(|t| tok.count(t)).collect();
    let boundaries = vec![BoundaryLevel::Page; 3];

    let plan = plan_chunks(&tokens, &boundaries, BoundaryLevel::Page, 10_000);
    assert_eq!(plan.chunks.len(), 1);
    assert_eq!(plan.chunks[0], vec![1, 2, 3]);

    let out_path = dir.join("out.pdf");
    pdf.write_chunk(&plan.chunks[0], &out_path).unwrap();
    let reloaded = Pdf::load(&out_path).unwrap();
    assert_eq!(reloaded.page_count(), 3);
}
