use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use indexmap::IndexMap;
use lopdf::{Destination, Document, Object, ObjectId, Outline};

use crate::plan::BoundaryLevel;

pub struct Pdf {
    doc: Document,
    pages: BTreeMap<u32, ObjectId>,
}

impl Pdf {
    pub fn load(path: &Path) -> Result<Self> {
        let doc = Document::load(path)
            .with_context(|| format!("failed to parse PDF: {}", path.display()))?;
        let pages = doc.get_pages();
        Ok(Self { doc, pages })
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// 1-based page numbers in document order.
    pub fn page_nums(&self) -> Vec<u32> {
        self.pages.keys().copied().collect()
    }

    /// Extracted text for a single 1-based page number. Missing or errored pages return `""`.
    ///
    /// Uses `lopdf::Document::extract_text` on the already-parsed Document (no second PDF parse),
    /// which is much faster than `pdf-extract` on large files. Quality is lower than pdf-extract,
    /// but we only use this for token *counting* — approximate is fine.
    pub fn page_text(&self, page_num: u32) -> String {
        self.doc.extract_text(&[page_num]).unwrap_or_default()
    }

    /// Number of `/Subtype /Image` XObjects referenced by a single 1-based page number.
    pub fn image_count(&self, page_num: u32) -> usize {
        match self.pages.get(&page_num) {
            Some(&page_id) => self.count_images_on_page(page_id),
            None => 0,
        }
    }

    fn count_images_on_page(&self, page_id: ObjectId) -> usize {
        let mut xobject_dicts: Vec<&lopdf::Dictionary> = Vec::new();
        if let Ok((inline, referenced)) = self.doc.get_page_resources(page_id) {
            if let Some(inline) = inline {
                if let Ok(xo) = inline
                    .get(b"XObject")
                    .and_then(Object::as_dict)
                {
                    xobject_dicts.push(xo);
                }
            }
            for rid in referenced {
                if let Ok(rdict) = self.doc.get_dictionary(rid) {
                    if let Ok(xo) = rdict.get(b"XObject").and_then(Object::as_dict) {
                        xobject_dicts.push(xo);
                    }
                }
            }
        }

        let mut count = 0usize;
        let mut seen: HashSet<ObjectId> = HashSet::new();
        for xo in xobject_dicts {
            for (_name, obj) in xo.iter() {
                if let Ok(id) = obj.as_reference() {
                    if !seen.insert(id) {
                        continue;
                    }
                    if let Ok(stream) = self.doc.get_object(id).and_then(Object::as_stream) {
                        if stream
                            .dict
                            .get(b"Subtype")
                            .and_then(Object::as_name)
                            .map(|n| n == b"Image")
                            .unwrap_or(false)
                        {
                            count += 1;
                        }
                    }
                }
            }
        }
        count
    }

    /// Boundary level that starts at each page (1-based indexed). Defaults to `Page` when no
    /// outline entry targets that page. If the document has no `/Outlines` at all, every entry
    /// is `Page`.
    pub fn boundaries(&self) -> Vec<BoundaryLevel> {
        let mut levels = vec![BoundaryLevel::Page; self.pages.len()];
        if levels.is_empty() {
            return levels;
        }
        // First page always starts the document.
        levels[0] = BoundaryLevel::Chapter;

        let page_id_to_num: std::collections::HashMap<ObjectId, u32> =
            self.pages.iter().map(|(n, id)| (*id, *n)).collect();

        let mut named: IndexMap<Vec<u8>, Destination> = IndexMap::new();
        let outlines = match self.doc.get_outlines(None, None, &mut named) {
            Ok(Some(o)) => o,
            _ => return levels,
        };

        fn walk(
            nodes: &[Outline],
            depth: u32,
            page_id_to_num: &std::collections::HashMap<ObjectId, u32>,
            levels: &mut [BoundaryLevel],
        ) {
            for node in nodes {
                match node {
                    Outline::Destination(dest) => {
                        if let Some(page) = resolve_page(dest, page_id_to_num) {
                            let lvl = BoundaryLevel::from_outline_depth(depth);
                            let idx = (page - 1) as usize;
                            if idx < levels.len() && lvl > levels[idx] {
                                levels[idx] = lvl;
                            }
                        }
                    }
                    Outline::SubOutlines(children) => {
                        walk(children, depth + 1, page_id_to_num, levels);
                    }
                }
            }
        }

        walk(&outlines, 1, &page_id_to_num, &mut levels);
        levels
    }

    /// True if the PDF has any `/Outlines` tree.
    pub fn has_outline(&self) -> bool {
        let mut named: IndexMap<Vec<u8>, Destination> = IndexMap::new();
        match self.doc.get_outlines(None, None, &mut named) {
            Ok(Some(o)) => !o.is_empty(),
            _ => false,
        }
    }

    /// Write a new PDF containing only the given 1-based page numbers, preserving original page
    /// content. Avoids `delete_pages` (which is O(deleted × all_objects) due to per-page graph
    /// traversal); instead, rebuilds the page tree directly to reference only the kept pages and
    /// uses `prune_objects` to GC the orphans in a single pass.
    pub fn write_chunk(&self, keep: &[u32], out_path: &Path) -> Result<()> {
        let mut doc = self.doc.clone();
        subset_to_pages(&mut doc, &self.pages, keep)?;
        doc.save(out_path)
            .with_context(|| format!("failed to write {}", out_path.display()))?;
        Ok(())
    }
}

fn subset_to_pages(
    doc: &mut Document,
    pages: &BTreeMap<u32, ObjectId>,
    keep: &[u32],
) -> Result<()> {
    let keep_set: BTreeSet<u32> = keep.iter().copied().collect();
    let kept_ids: Vec<ObjectId> = pages
        .iter()
        .filter(|(n, _)| keep_set.contains(n))
        .map(|(_, id)| *id)
        .collect();
    if kept_ids.is_empty() {
        return Err(anyhow!("subset_to_pages called with empty keep list"));
    }

    let catalog_id = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(|e| anyhow!("missing /Root in trailer: {e}"))?;
    let pages_root_id = doc
        .get_dictionary(catalog_id)
        .and_then(|d| d.get(b"Pages"))
        .and_then(Object::as_reference)
        .map_err(|e| anyhow!("missing /Pages in catalog: {e}"))?;

    // Repoint each kept page directly at the pages root, and strip per-page entries that may
    // reach pages outside the kept set: /Annots can carry GoTo links to dropped pages,
    // /B references article-thread beads, /AA carries open/close actions. Without removal,
    // prune_objects keeps those targets alive (bloating the chunk) and the chunk PDF retains
    // dangling refs. We accept losing intra-chunk hyperlinks and URL annotations here —
    // chunk PDFs are for downstream tokenized processing, not reading.
    for &page_id in &kept_ids {
        if let Ok(page_dict) = doc.get_object_mut(page_id).and_then(Object::as_dict_mut) {
            page_dict.set("Parent", Object::Reference(pages_root_id));
            page_dict.remove(b"Annots");
            page_dict.remove(b"B");
            page_dict.remove(b"AA");
        }
    }

    // Replace the page tree's Kids/Count with our subset, flattened.
    let new_kids: Vec<Object> = kept_ids.iter().map(|id| Object::Reference(*id)).collect();
    let new_count = kept_ids.len() as i64;
    if let Ok(pages_dict) = doc.get_object_mut(pages_root_id).and_then(Object::as_dict_mut) {
        pages_dict.set("Kids", Object::Array(new_kids));
        pages_dict.set("Count", new_count);
    } else {
        return Err(anyhow!("/Pages object is not a dictionary"));
    }

    // Drop catalog entries that reference pages or document-wide interactive structure.
    // Anything reachable through these would survive prune_objects, keeping dropped pages
    // alive in the chunk and leaving stale refs (e.g. an OpenAction targeting a missing page).
    if let Ok(catalog_dict) = doc.get_object_mut(catalog_id).and_then(Object::as_dict_mut) {
        catalog_dict.remove(b"Outlines");
        catalog_dict.remove(b"Names");
        catalog_dict.remove(b"Dests");
        catalog_dict.remove(b"PageLabels");
        catalog_dict.remove(b"OpenAction");
        catalog_dict.remove(b"AA");
        catalog_dict.remove(b"AcroForm");
        catalog_dict.remove(b"StructTreeRoot");
        catalog_dict.remove(b"MarkInfo");
        catalog_dict.remove(b"Threads");
    }

    // Single-pass GC of everything no longer reachable from the trailer.
    doc.prune_objects();

    Ok(())
}

fn resolve_page(
    dest: &Destination,
    page_id_to_num: &std::collections::HashMap<ObjectId, u32>,
) -> Option<u32> {
    let page_obj = dest.page().ok()?;
    match page_obj {
        Object::Reference(id) => page_id_to_num.get(id).copied(),
        Object::Integer(i) => {
            if *i >= 0 {
                let n = (*i as u32) + 1;
                if (n as usize) <= page_id_to_num.len() {
                    Some(n)
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}
