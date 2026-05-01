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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_histogram_no_outline_emits_single_line() {
        let mut buf = Vec::new();
        print_histogram(&mut buf, &[], 423).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "423 pages, no outline present\n");
    }

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
[p11]    1.1 Background
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
        let expected = "[p1] (untitled)\n";
        assert_eq!(got, expected, "got:\n{got:?}\nexpected:\n{expected:?}");
    }
}
