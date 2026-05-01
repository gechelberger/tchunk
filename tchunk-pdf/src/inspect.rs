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
}
