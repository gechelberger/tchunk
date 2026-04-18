use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use indexmap::IndexMap;
use lopdf::{Destination, Document, Object, ObjectId, Outline};

use crate::plan::BoundaryLevel;

pub struct Pdf {
    bytes: Vec<u8>,
    doc: Document,
    pages: BTreeMap<u32, ObjectId>,
}

impl Pdf {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read input file: {}", path.display()))?;
        let doc = Document::load_mem(&bytes)
            .with_context(|| format!("failed to parse PDF: {}", path.display()))?;
        let pages = doc.get_pages();
        Ok(Self { bytes, doc, pages })
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Per-page text via `pdf-extract`. Returns one String per page (length == page_count).
    /// On failure, returns empty strings for all pages so token counting still proceeds (with
    /// zero-token pages, which will trigger the scan-like warning naturally).
    pub fn page_texts(&self) -> Vec<String> {
        match pdf_extract::extract_text_from_mem_by_pages(&self.bytes) {
            Ok(v) if v.len() == self.pages.len() => v,
            Ok(v) => {
                let mut out = v;
                out.resize(self.pages.len(), String::new());
                out
            }
            Err(_) => vec![String::new(); self.pages.len()],
        }
    }

    /// Number of `/Subtype /Image` XObjects referenced by each page (1-based indexed).
    pub fn image_counts(&self) -> Vec<usize> {
        let mut counts = Vec::with_capacity(self.pages.len());
        for (_pn, page_id) in &self.pages {
            counts.push(self.count_images_on_page(*page_id));
        }
        counts
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
    /// content. Achieved by loading a fresh copy of the source bytes, deleting all other pages,
    /// pruning unreferenced objects, then saving.
    pub fn write_chunk(&self, keep: &[u32], out_path: &Path) -> Result<()> {
        let mut doc = Document::load_mem(&self.bytes)
            .context("failed to reload source PDF for chunk write")?;
        let total_pages = self.pages.len() as u32;
        let keep_set: std::collections::BTreeSet<u32> = keep.iter().copied().collect();
        let to_delete: Vec<u32> = (1..=total_pages).filter(|p| !keep_set.contains(p)).collect();
        if !to_delete.is_empty() {
            doc.delete_pages(&to_delete);
        }
        doc.renumber_objects();
        doc.compress();
        doc.save(out_path)
            .with_context(|| format!("failed to write {}", out_path.display()))?;
        Ok(())
    }
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
