use std::fmt;
use std::ops::Range;

/// Per-page outline data. Records the actual outline depth, with no level-name mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    Page,
    Bookmark { depth: u32 },
}

/// What the user (or recursion) is splitting at. Named CLI flags resolve to specific
/// `SplitAt` values in `cli.rs`; everywhere else in the codebase works directly in
/// depth-space.
///
/// Variant order is load-bearing: derived `Ord` ranks coarsest→finest, so
/// `Depth(1) < Depth(2) < ... < AnyBookmark < Page`. `main.rs` uses `.max()` to pick
/// the finest level reached across chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SplitAt {
    Depth(u32),
    AnyBookmark,
    Page,
}

impl SplitAt {
    /// True if `b` is a valid cut point at this split level. At `Page` level every page
    /// boundary qualifies; at `AnyBookmark` any bookmark qualifies; at `Depth(N)` a
    /// bookmark qualifies iff its depth is `<= N`.
    pub fn matches(&self, b: &Boundary) -> bool {
        match (self, b) {
            (SplitAt::Page, _) => true,
            (SplitAt::AnyBookmark, Boundary::Bookmark { .. }) => true,
            (SplitAt::AnyBookmark, Boundary::Page) => false,
            (SplitAt::Depth(n), Boundary::Bookmark { depth }) => depth <= n,
            (SplitAt::Depth(_), Boundary::Page) => false,
        }
    }
}

impl fmt::Display for SplitAt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SplitAt::Page => f.write_str("page"),
            SplitAt::AnyBookmark => f.write_str("any-bookmark"),
            SplitAt::Depth(n) => write!(f, "depth-{n}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    OversizedPage { page: u32, tokens: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedChunk {
    pub pages: Vec<u32>,
    /// The split level at which *this chunk's* adjacent cuts were taken. For a chunk produced at
    /// the requested `split_at`, this equals the requested level. For a chunk produced by
    /// recursing into an over-budget unit, this is the finer level the recursion landed on.
    pub effective_level: SplitAt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanResult {
    pub chunks: Vec<PlannedChunk>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn plan_chunks(
    tokens: &[usize],
    boundaries: &[Boundary],
    split_at: SplitAt,
    budget: usize,
) -> PlanResult {
    assert_eq!(tokens.len(), boundaries.len());
    if tokens.is_empty() {
        return PlanResult { chunks: Vec::new(), diagnostics: Vec::new() };
    }

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut chunks = greedy_pack(tokens, boundaries, 0..tokens.len(), split_at, budget, &mut diagnostics);
    rebalance_last_two(&mut chunks, tokens, boundaries, split_at, budget);
    PlanResult { chunks, diagnostics }
}

/// Budget-greedy pack over the units that `segment_units` produces from `range` at `split_at`.
/// On unit overrun, hand off to `plan_overrun` which re-plans that unit at the next finer level.
///
/// Budget-greedy (rather than equal-target) is important: it keeps the invariant that no two
/// adjacent output chunks can be combined under budget, because a flush only happens when the
/// next unit would overflow. Equal-target packing broke that invariant — the hysteresis check
/// would fire partway through the remainder and leave two small neighbor chunks that obviously
/// should have been one.
fn greedy_pack(
    tokens: &[usize],
    boundaries: &[Boundary],
    range: Range<usize>,
    split_at: SplitAt,
    budget: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<PlannedChunk> {
    let units = segment_units(boundaries, range, split_at);

    let mut chunks: Vec<PlannedChunk> = Vec::new();
    let mut cur: Vec<Range<usize>> = Vec::new();
    let mut cur_tokens: usize = 0;

    for unit in units {
        let unit_tokens = sum_tokens(tokens, &unit);

        if unit_tokens > budget {
            flush_units(&mut chunks, &mut cur, &mut cur_tokens, split_at);
            let finer = next_effective_level(boundaries, unit.clone(), split_at);
            let sub = plan_overrun(tokens, boundaries, unit, finer, budget, diagnostics);
            chunks.extend(sub);
            continue;
        }

        if cur_tokens + unit_tokens > budget {
            flush_units(&mut chunks, &mut cur, &mut cur_tokens, split_at);
        }
        cur_tokens += unit_tokens;
        cur.push(unit);
    }
    flush_units(&mut chunks, &mut cur, &mut cur_tokens, split_at);
    chunks
}

/// Re-plan an over-budget unit at a finer level. Same packing as the top level, but follows up
/// with a pairwise-sweep rebalance so siblings of a recursed unit redistribute toward equal
/// sizes. Page-level bottoms out in `pack_pages_balanced`, which adds the oversized-page
/// diagnostic.
fn plan_overrun(
    tokens: &[usize],
    boundaries: &[Boundary],
    range: Range<usize>,
    split_at: SplitAt,
    budget: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<PlannedChunk> {
    if split_at == SplitAt::Page {
        return pack_pages_balanced(tokens, boundaries, range, budget, diagnostics);
    }
    let mut chunks = greedy_pack(tokens, boundaries, range, split_at, budget, diagnostics);
    pairwise_rebalance(&mut chunks, tokens, boundaries, split_at, budget);
    chunks
}

/// Page-level base case for `plan_overrun`. Budget-greedy page packing (same strategy as the
/// top-level packer, just at page granularity) followed by the pairwise rebalance. Oversized
/// pages (token count > budget) become their own chunk with an `OversizedPage` diagnostic.
fn pack_pages_balanced(
    tokens: &[usize],
    boundaries: &[Boundary],
    range: Range<usize>,
    budget: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<PlannedChunk> {
    let mut chunks: Vec<PlannedChunk> = Vec::new();
    let mut cur: Vec<u32> = Vec::new();
    let mut cur_tokens: usize = 0;

    for i in range {
        let page = (i + 1) as u32;
        let t = tokens[i];

        if t > budget {
            if !cur.is_empty() {
                chunks.push(PlannedChunk {
                    pages: std::mem::take(&mut cur),
                    effective_level: SplitAt::Page,
                });
                cur_tokens = 0;
            }
            chunks.push(PlannedChunk {
                pages: vec![page],
                effective_level: SplitAt::Page,
            });
            diagnostics.push(Diagnostic::OversizedPage { page, tokens: t });
            continue;
        }

        if cur_tokens + t > budget {
            chunks.push(PlannedChunk {
                pages: std::mem::take(&mut cur),
                effective_level: SplitAt::Page,
            });
            cur_tokens = 0;
        }
        cur.push(page);
        cur_tokens += t;
    }
    if !cur.is_empty() {
        chunks.push(PlannedChunk {
            pages: cur,
            effective_level: SplitAt::Page,
        });
    }

    pairwise_rebalance(&mut chunks, tokens, boundaries, SplitAt::Page, budget);
    chunks
}

fn flush_units(
    chunks: &mut Vec<PlannedChunk>,
    cur: &mut Vec<Range<usize>>,
    cur_tokens: &mut usize,
    level: SplitAt,
) {
    if cur.is_empty() {
        return;
    }
    let mut pages: Vec<u32> = Vec::new();
    for r in cur.drain(..) {
        for i in r {
            pages.push((i + 1) as u32);
        }
    }
    *cur_tokens = 0;
    chunks.push(PlannedChunk { pages, effective_level: level });
}

fn sum_tokens(tokens: &[usize], range: &Range<usize>) -> usize {
    tokens[range.clone()].iter().sum()
}

/// Segment `page_range` into unit ranges at `split_at`. A unit starts at `page_range.start` and
/// at every interior page whose boundary qualifies under `split_at`. At `Page` level every page
/// is its own unit.
fn segment_units(
    boundaries: &[Boundary],
    page_range: Range<usize>,
    split_at: SplitAt,
) -> Vec<Range<usize>> {
    if page_range.is_empty() {
        return Vec::new();
    }
    if split_at == SplitAt::Page {
        return page_range.map(|i| i..i + 1).collect();
    }
    let start = page_range.start;
    let end = page_range.end;
    let mut units = Vec::new();
    let mut cur_start = start;
    for (i, b) in boundaries.iter().enumerate().take(end).skip(start + 1) {
        if split_at.matches(b) {
            units.push(cur_start..i);
            cur_start = i;
        }
    }
    units.push(cur_start..end);
    units
}

/// Find the coarsest level strictly finer than `current` that has at least one boundary inside
/// `page_range` (excluding the start page, which is already the unit's own boundary). Falls
/// through to `SplitAt::Page` when nothing finer has an interior split point.
fn next_effective_level(
    boundaries: &[Boundary],
    page_range: Range<usize>,
    current: SplitAt,
) -> SplitAt {
    // From AnyBookmark or Page, the only step finer is Page.
    let start_depth = match current {
        SplitAt::Page | SplitAt::AnyBookmark => return SplitAt::Page,
        SplitAt::Depth(n) => n,
    };

    // The coarsest qualifying level is the smallest interior bookmark depth strictly greater
    // than start_depth. One pass over the interior gives us that directly.
    let min_deeper = (page_range.start + 1..page_range.end)
        .filter_map(|i| match boundaries[i] {
            Boundary::Bookmark { depth } if depth > start_depth => Some(depth),
            _ => None,
        })
        .min();

    match min_deeper {
        Some(d) => SplitAt::Depth(d),
        None => SplitAt::Page,
    }
}

/// Final doc-wide rebalance between the last two chunks. Preserved verbatim in semantics from
/// the pre-recursion implementation so documents with no overrun keep the same chunk layout.
fn rebalance_last_two(
    chunks: &mut [PlannedChunk],
    tokens: &[usize],
    boundaries: &[Boundary],
    split_at: SplitAt,
    budget: usize,
) {
    if chunks.len() < 2 {
        return;
    }
    let i = chunks.len() - 2;
    try_rebalance_pair(chunks, i, tokens, boundaries, split_at, budget);
}

/// Pairwise-sweep rebalance across all adjacent chunk pairs. Stops when a full pass makes no
/// change, or after `MAX_PASSES`. Only shifts cuts at `split_at` boundaries, so deeper-recursed
/// regions (whose interior boundaries are all finer than `split_at`) stay intact.
fn pairwise_rebalance(
    chunks: &mut [PlannedChunk],
    tokens: &[usize],
    boundaries: &[Boundary],
    split_at: SplitAt,
    budget: usize,
) {
    const MAX_PASSES: usize = 3;
    if chunks.len() < 2 {
        return;
    }
    for _ in 0..MAX_PASSES {
        let mut changed = false;
        for i in 0..chunks.len() - 1 {
            if try_rebalance_pair(chunks, i, tokens, boundaries, split_at, budget) {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

fn try_rebalance_pair(
    chunks: &mut [PlannedChunk],
    i: usize,
    tokens: &[usize],
    boundaries: &[Boundary],
    split_at: SplitAt,
    budget: usize,
) -> bool {
    let (left, right) = chunks.split_at_mut(i + 1);
    let left_chunk = &mut left[i];
    let right_chunk = &mut right[0];

    let combined: Vec<u32> = left_chunk
        .pages
        .iter()
        .chain(right_chunk.pages.iter())
        .copied()
        .collect();
    let original_cut_idx = left_chunk.pages.len() - 1;

    let pick = match best_balanced_cut(&combined, tokens, boundaries, split_at, budget) {
        Some(p) if p != original_cut_idx => p,
        _ => return false,
    };

    left_chunk.pages = combined[..=pick].to_vec();
    right_chunk.pages = combined[pick + 1..].to_vec();
    true
}

/// Search all positions in `combined` where a cut is allowed at `split_at` and both halves stay
/// under `budget`; return the index with the smallest |left − right| token difference. Returns
/// `None` if no feasible cut exists (caller keeps the original cut).
fn best_balanced_cut(
    combined: &[u32],
    tokens: &[usize],
    boundaries: &[Boundary],
    split_at: SplitAt,
    budget: usize,
) -> Option<usize> {
    let n = tokens.len();
    let cut_after_allowed =
        |idx0: usize| -> bool { idx0 + 1 == n || split_at.matches(&boundaries[idx0 + 1]) };

    let total: usize = combined.iter().map(|&p| tokens[(p - 1) as usize]).sum();

    let mut best: Option<usize> = None;
    let mut best_diff: usize = usize::MAX;
    let mut left_sum: usize = 0;
    for (k, &p) in combined.iter().enumerate() {
        left_sum += tokens[(p - 1) as usize];
        if k + 1 == combined.len() {
            break;
        }
        let idx0 = (p - 1) as usize;
        if !cut_after_allowed(idx0) {
            continue;
        }
        let right_sum = total - left_sum;
        if left_sum > budget || right_sum > budget {
            continue;
        }
        let diff = left_sum.abs_diff(right_sum);
        if diff < best_diff {
            best_diff = diff;
            best = Some(k);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages_only(n: usize) -> Vec<Boundary> {
        vec![Boundary::Page; n]
    }

    fn chunk_pages(r: &PlanResult) -> Vec<Vec<u32>> {
        r.chunks.iter().map(|c| c.pages.clone()).collect()
    }

    fn chunk_tokens(r: &PlanResult, tokens: &[usize]) -> Vec<usize> {
        r.chunks
            .iter()
            .map(|c| c.pages.iter().map(|&p| tokens[(p - 1) as usize]).sum())
            .collect()
    }

    #[test]
    fn empty_input_yields_empty_plan() {
        let r = plan_chunks(&[], &[], SplitAt::Page, 100);
        assert!(r.chunks.is_empty());
        assert!(r.diagnostics.is_empty());
    }

    #[test]
    fn single_chunk_when_total_under_budget() {
        let tokens = vec![10, 20, 30];
        let r = plan_chunks(&tokens, &pages_only(3), SplitAt::Page, 100);
        assert_eq!(chunk_pages(&r), vec![vec![1, 2, 3]]);
        assert!(r.diagnostics.is_empty());
        assert_eq!(r.chunks[0].effective_level, SplitAt::Page);
    }

    #[test]
    fn two_chunks_get_rebalanced() {
        let tokens = vec![10, 20, 30];
        let r = plan_chunks(&tokens, &pages_only(3), SplitAt::Page, 35);
        assert_eq!(r.chunks.len(), 2);
        let sums = chunk_tokens(&r, &tokens);
        assert!(sums[0].abs_diff(sums[1]) <= 10, "rebalance failed: {:?}", sums);
        assert!(sums.iter().all(|&s| s <= 35));
    }

    #[test]
    fn greedy_then_rebalance_classic() {
        let tokens = vec![30, 30, 30, 5];
        let r = plan_chunks(&tokens, &pages_only(4), SplitAt::Page, 60);
        assert_eq!(chunk_pages(&r), vec![vec![1, 2], vec![3, 4]]);
    }

    #[test]
    fn three_or_more_chunks_only_last_two_rebalance() {
        let tokens = vec![50, 50, 50, 50, 50, 1];
        let r = plan_chunks(&tokens, &pages_only(6), SplitAt::Page, 100);
        assert_eq!(chunk_pages(&r), vec![vec![1, 2], vec![3, 4], vec![5, 6]]);
    }

    #[test]
    fn rebalance_last_two_against_remainder_pattern() {
        let tokens = vec![40, 40, 40, 40, 40, 40, 40, 5];
        let r = plan_chunks(&tokens, &pages_only(8), SplitAt::Page, 100);
        assert_eq!(r.chunks.len(), 4);
        let sums = chunk_tokens(&r, &tokens);
        assert!(sums.iter().all(|&s| s <= 100));
        let last_sum = sums[sums.len() - 1];
        let second_last_sum = sums[sums.len() - 2];
        assert!(second_last_sum + last_sum <= 200);
    }

    #[test]
    fn oversized_page_emits_own_chunk_with_diagnostic() {
        let tokens = vec![10, 200, 10];
        let r = plan_chunks(&tokens, &pages_only(3), SplitAt::Page, 50);
        assert_eq!(chunk_pages(&r), vec![vec![1], vec![2], vec![3]]);
        assert_eq!(
            r.diagnostics,
            vec![Diagnostic::OversizedPage { page: 2, tokens: 200 }]
        );
    }

    #[test]
    fn split_at_section_only_cuts_at_section_boundaries() {
        let tokens = vec![30, 30, 30, 30];
        let boundaries = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Page,
            Boundary::Bookmark { depth: 2 },
            Boundary::Page,
        ];
        let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(2), 70);
        assert_eq!(chunk_pages(&r), vec![vec![1, 2], vec![3, 4]]);
        assert!(r.diagnostics.is_empty());
        assert!(r.chunks.iter().all(|c| c.effective_level == SplitAt::Depth(2)));
    }

    #[test]
    fn split_at_section_no_interior_boundary_recurses_to_page() {
        // 4 pages of 30 tokens; only boundary at page 1 (start). budget 70. split_at=Depth(2).
        // No interior boundary qualifying for Depth(2) anywhere → the whole doc is one unit
        // which overruns, recurses down to Page level, and emits balanced page-level chunks.
        let tokens = vec![30, 30, 30, 30];
        let boundaries = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Page,
            Boundary::Page,
            Boundary::Page,
        ];
        let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(2), 70);
        assert_eq!(chunk_pages(&r), vec![vec![1, 2], vec![3, 4]]);
        assert!(r.diagnostics.is_empty());
        assert!(r.chunks.iter().all(|c| c.effective_level == SplitAt::Page));
    }

    #[test]
    fn split_at_chapter_emits_no_diagnostic_for_page_only_doc_when_total_fits_one_chunk() {
        let tokens = vec![10, 10, 10];
        let boundaries = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Page,
            Boundary::Page,
        ];
        let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(1), 100);
        assert_eq!(chunk_pages(&r), vec![vec![1, 2, 3]]);
        assert!(r.diagnostics.is_empty());
    }

    #[test]
    fn rebalance_respects_allowed_cuts() {
        let tokens = vec![10, 50, 10, 30];
        let boundaries = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Page,
            Boundary::Bookmark { depth: 2 },
            Boundary::Page,
        ];
        let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(2), 70);
        assert_eq!(chunk_pages(&r), vec![vec![1, 2], vec![3, 4]]);
    }

    #[test]
    fn rebalance_does_not_pick_cut_that_exceeds_budget() {
        let tokens = vec![10, 10, 60, 10];
        let r = plan_chunks(&tokens, &pages_only(4), SplitAt::Page, 80);
        assert_eq!(chunk_pages(&r), vec![vec![1, 2], vec![3, 4]]);
    }

    // ---- recursive-balance tests ----

    #[test]
    fn oversized_chapter_recurses_to_section_and_balances() {
        // One chapter containing three equal sections at pages 1, 2, 3 (each 100 tokens).
        // Chapter tokens = 300; budget = 120 (so the chapter is ~2.5× budget).
        // split_at = Depth(1) → unit is the whole doc → overruns → recurses to Depth(2).
        let tokens = vec![100, 100, 100];
        let boundaries = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Bookmark { depth: 2 },
            Boundary::Bookmark { depth: 2 },
        ];
        let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(1), 120);
        let sums = chunk_tokens(&r, &tokens);
        assert!(sums.iter().all(|&s| s <= 120), "budget violated: {:?}", sums);
        // Flat coverage: all three pages accounted for once, in order.
        let flat: Vec<u32> = r.chunks.iter().flat_map(|c| c.pages.clone()).collect();
        assert_eq!(flat, vec![1, 2, 3]);
        assert!(
            r.chunks.iter().all(|c| c.effective_level == SplitAt::Depth(2)),
            "expected all chunks at Depth(2), got {:?}",
            r.chunks.iter().map(|c| c.effective_level).collect::<Vec<_>>()
        );
        assert!(r.diagnostics.is_empty());
    }

    #[test]
    fn oversized_chapter_falls_through_to_page_when_no_finer_boundaries() {
        // One chapter, no finer outline. Chapter overruns → recursion falls through
        // (no deeper bookmarks inside) → Page level.
        let tokens = vec![30, 30, 30, 30, 30];
        let boundaries = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Page,
            Boundary::Page,
            Boundary::Page,
            Boundary::Page,
        ];
        let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(1), 90);
        let sums = chunk_tokens(&r, &tokens);
        assert!(sums.iter().all(|&s| s <= 90), "budget violated: {:?}", sums);
        assert!(r.chunks.iter().all(|c| c.effective_level == SplitAt::Page));
        assert!(r.diagnostics.is_empty());
        let flat: Vec<u32> = r.chunks.iter().flat_map(|c| c.pages.clone()).collect();
        assert_eq!(flat, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn mixed_top_level_some_fit_some_recurse() {
        // Two chapters. Chapter A (pages 1-2) fits in its own chunk. Chapter B (pages 3-6)
        // contains four sections of 40 tokens each and overruns budget 80 → recurses to Depth(2).
        // Expect chunk 0 at Depth(1), subsequent chunks at Depth(2).
        let tokens = vec![40, 40, 40, 40, 40, 40];
        let boundaries = vec![
            Boundary::Bookmark { depth: 1 }, // page 1 starts chapter A
            Boundary::Bookmark { depth: 2 }, // page 2 is a section within A
            Boundary::Bookmark { depth: 1 }, // page 3 starts chapter B
            Boundary::Bookmark { depth: 2 },
            Boundary::Bookmark { depth: 2 },
            Boundary::Bookmark { depth: 2 },
        ];
        let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(1), 80);
        let sums = chunk_tokens(&r, &tokens);
        assert!(sums.iter().all(|&s| s <= 80), "budget violated: {:?}", sums);

        // First chunk is chapter A (pages 1-2) at Depth(1).
        assert_eq!(r.chunks[0].pages, vec![1, 2]);
        assert_eq!(r.chunks[0].effective_level, SplitAt::Depth(1));
        // Later chunks are sub-chunks of chapter B at Depth(2).
        assert!(
            r.chunks[1..]
                .iter()
                .all(|c| c.effective_level == SplitAt::Depth(2)),
            "expected later chunks at Depth(2)"
        );
        let flat: Vec<u32> = r.chunks.iter().flat_map(|c| c.pages.clone()).collect();
        assert_eq!(flat, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn oversized_page_inside_recursed_unit_still_fires_diagnostic() {
        // Chapter = 1 page that itself is oversized. No finer structure. Recursion falls
        // through to Page, where the single-page base case still emits OversizedPage.
        let tokens = vec![10, 200, 10];
        let boundaries = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Bookmark { depth: 1 }, // page 2 starts a new chapter (which overruns)
            Boundary::Bookmark { depth: 1 },
        ];
        let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(1), 50);
        assert_eq!(chunk_pages(&r), vec![vec![1], vec![2], vec![3]]);
        assert_eq!(
            r.diagnostics,
            vec![Diagnostic::OversizedPage { page: 2, tokens: 200 }]
        );
    }

    #[test]
    fn oversized_page_inside_multi_page_recursed_chapter_warns() {
        // One chapter, five pages, page 3 is oversized (tokens > budget). The chapter overruns
        // so we recurse to Page level. The oversized page must still trigger the
        // OversizedPage diagnostic — this is the "recursion bottomed out but a single atomic
        // unit still exceeds budget" signal the user needs to see.
        let tokens = vec![100, 100, 8000, 100, 100];
        let boundaries = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Page,
            Boundary::Page,
            Boundary::Page,
            Boundary::Page,
        ];
        let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(1), 1000);
        assert_eq!(
            r.diagnostics,
            vec![Diagnostic::OversizedPage { page: 3, tokens: 8000 }]
        );
        // The oversized page is its own chunk; neighbors pack around it.
        assert!(
            r.chunks.iter().any(|c| c.pages == vec![3]),
            "expected page 3 alone in its own chunk, got {:?}",
            r.chunks.iter().map(|c| c.pages.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn recursed_chunks_never_mergeable_with_neighbors() {
        // Regression: the Draco_Malfoy book at budget 7000 produced pairs like [3145, 3218] and
        // [2286, 2374] within the same recursed chapter — both pairs sum under budget. The
        // equal-target packing bug was causing that; budget-greedy should make adjacent pairs
        // always sum strictly *over* budget within a recursed unit.
        //
        // 14 pages of 500 tokens each = 7000 total. Chapter covers all. Budget 6000 forces
        // recursion to Page. Expect the produced chunks to have the invariant that no two
        // adjacent chunks can be combined under budget.
        let tokens = vec![500usize; 14];
        let mut boundaries = vec![Boundary::Page; 14];
        boundaries[0] = Boundary::Bookmark { depth: 1 };
        let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(1), 6000);
        let sums = chunk_tokens(&r, &tokens);
        assert!(sums.iter().all(|&s| s <= 6000), "budget violated: {:?}", sums);
        for w in sums.windows(2) {
            assert!(
                w[0] + w[1] > 6000,
                "adjacent chunks could be merged under budget: {} + {} = {} ≤ 6000",
                w[0],
                w[1],
                w[0] + w[1]
            );
        }
    }

    #[test]
    fn recursed_chapter_does_not_oversplit() {
        // Another regression: pack a chapter of 8 pages @ 400 tokens each = 3200 total, budget
        // 2000. Budget-greedy should give 2 chunks (each 1600-2000 tokens), not 3+ tiny ones.
        let tokens = vec![400usize; 8];
        let mut boundaries = vec![Boundary::Page; 8];
        boundaries[0] = Boundary::Bookmark { depth: 1 };
        let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(1), 2000);
        assert_eq!(r.chunks.len(), 2, "expected 2 chunks, got {}: {:?}", r.chunks.len(), chunk_tokens(&r, &tokens));
        let sums = chunk_tokens(&r, &tokens);
        assert!(sums.iter().all(|&s| s <= 2000));
        // Balanced within 1 page (400 tokens).
        assert!(sums[0].abs_diff(sums[1]) <= 400, "unbalanced: {:?}", sums);
    }

    #[test]
    fn balance_improves_with_pairwise_sweep() {
        // Without pairwise sweep, equal-target greedy could leave the final chunk small.
        // Five sections of [60, 60, 60, 60, 60] (total 300), budget 120. Verify budget and
        // ordering are preserved.
        let tokens = vec![60, 60, 60, 60, 60];
        let boundaries = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Bookmark { depth: 2 },
            Boundary::Bookmark { depth: 2 },
            Boundary::Bookmark { depth: 2 },
            Boundary::Bookmark { depth: 2 },
        ];
        let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(1), 120);
        let sums = chunk_tokens(&r, &tokens);
        assert!(sums.iter().all(|&s| s <= 120));
        let flat: Vec<u32> = r.chunks.iter().flat_map(|c| c.pages.clone()).collect();
        assert_eq!(flat, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn next_effective_level_walks_depth_then_page() {
        // From Depth(1), with an interior Bookmark{depth:2} → next is Depth(2).
        let b = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Bookmark { depth: 2 },
            Boundary::Page,
        ];
        assert_eq!(
            next_effective_level(&b, 0..3, SplitAt::Depth(1)),
            SplitAt::Depth(2)
        );
        // From Depth(2), no deeper bookmarks → falls through to Page.
        let b = vec![
            Boundary::Bookmark { depth: 2 },
            Boundary::Page,
            Boundary::Page,
        ];
        assert_eq!(
            next_effective_level(&b, 0..3, SplitAt::Depth(2)),
            SplitAt::Page
        );
        // From AnyBookmark → Page directly.
        let b = vec![
            Boundary::Bookmark { depth: 5 },
            Boundary::Bookmark { depth: 5 },
        ];
        assert_eq!(
            next_effective_level(&b, 0..2, SplitAt::AnyBookmark),
            SplitAt::Page
        );
    }

    #[test]
    fn segment_units_at_page_returns_singletons() {
        let b = pages_only(4);
        let units = segment_units(&b, 0..4, SplitAt::Page);
        assert_eq!(units, vec![0..1, 1..2, 2..3, 3..4]);
    }

    #[test]
    fn segment_units_at_section_splits_on_interior_boundaries() {
        let b = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Page,
            Boundary::Bookmark { depth: 2 },
            Boundary::Page,
            Boundary::Bookmark { depth: 2 },
        ];
        let units = segment_units(&b, 0..5, SplitAt::Depth(2));
        assert_eq!(units, vec![0..2, 2..4, 4..5]);
    }

    #[test]
    fn next_effective_level_skips_empty_levels() {
        // Depth(1) requested; interior has only Page boundaries → fall through to Page.
        let b = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Page,
            Boundary::Page,
        ];
        assert_eq!(
            next_effective_level(&b, 0..3, SplitAt::Depth(1)),
            SplitAt::Page
        );
        // Depth(1) requested; interior has a depth-2 bookmark → land on Depth(2).
        let b = vec![
            Boundary::Bookmark { depth: 1 },
            Boundary::Bookmark { depth: 2 },
            Boundary::Page,
        ];
        assert_eq!(
            next_effective_level(&b, 0..3, SplitAt::Depth(1)),
            SplitAt::Depth(2)
        );
    }

    #[test]
    fn splitat_display_covers_all_variants() {
        assert_eq!(SplitAt::Page.to_string(), "page");
        assert_eq!(SplitAt::AnyBookmark.to_string(), "any-bookmark");
        assert_eq!(SplitAt::Depth(1).to_string(), "depth-1");
        assert_eq!(SplitAt::Depth(42).to_string(), "depth-42");
    }

    #[test]
    fn splitat_matches_depth_threshold() {
        let page_split = SplitAt::Page;
        let any_b = SplitAt::AnyBookmark;
        let d1 = SplitAt::Depth(1);
        let d2 = SplitAt::Depth(2);
        let page = Boundary::Page;
        let b1 = Boundary::Bookmark { depth: 1 };
        let b2 = Boundary::Bookmark { depth: 2 };
        let b3 = Boundary::Bookmark { depth: 3 };

        // Page matches everything (every page boundary qualifies at Page level).
        assert!(page_split.matches(&page));
        assert!(page_split.matches(&b1));

        // AnyBookmark matches every Bookmark, never Page.
        assert!(any_b.matches(&b1));
        assert!(any_b.matches(&b3));
        assert!(!any_b.matches(&page));

        // Depth(N) matches Bookmark{depth} where depth <= N.
        assert!(d1.matches(&b1));
        assert!(!d1.matches(&b2));
        assert!(d2.matches(&b1));
        assert!(d2.matches(&b2));
        assert!(!d2.matches(&b3));
        assert!(!d1.matches(&page));
    }

    #[test]
    fn splitat_ord_runs_coarse_to_fine() {
        // Variant order in the enum declaration drives derive(Ord), so:
        //   Depth(small N) < Depth(large N) < AnyBookmark < Page
        assert!(SplitAt::Depth(1) < SplitAt::Depth(2));
        assert!(SplitAt::Depth(99) < SplitAt::AnyBookmark);
        assert!(SplitAt::AnyBookmark < SplitAt::Page);
        assert!(SplitAt::Depth(1) < SplitAt::Page);
    }
}
