# Depth as the splitting primitive — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor `tchunk-pdf`'s split-boundary system so raw outline depth is the native primitive, with named CLI flags (`chapter`, `section`, `subsection`, `any-bookmark`) demoted to syntactic sugar over specific depths.

**Architecture:** Replace the single `BoundaryLevel` enum with two distinct types: `Boundary` for per-page outline data (`Page` or `Bookmark { depth: u32 }`) and `SplitAt` for the user/recursion target (`Page`, `AnyBookmark`, `Depth(u32)`). Comparison semantics flip: smaller depth = coarser. Named CLI flags resolve to specific `SplitAt` values in `cli.rs`; everywhere else operates in depth-space. The `0|1 → Chapter` heuristic in `pdf.rs` is dropped — outline depth is recorded honestly. A new `--split-at-depth N` peer flag (mutually exclusive with `--split-at` via clap `ArgGroup`) provides direct depth control.

**Tech Stack:** Rust, clap (CLI), lopdf (PDF parsing), serde (sidecar JSON).

**Source-of-truth design doc:** `docs/superpowers/specs/2026-05-01-depth-as-splitting-primitive-design.md`. Read it before starting.

**Caveat about cross-task compile state:** the type swap touches 5 files. The repo will not compile cleanly until Tasks 2 through 7 are all done. That's expected — verify with `cargo check` after each task, but don't expect zero errors until Task 8.

---

## File map

| File                       | Change                                                                        |
|----------------------------|-------------------------------------------------------------------------------|
| `tchunk-pdf/src/plan.rs`   | Add `Boundary` + `SplitAt`. Drop `BoundaryLevel`. Rewrite all internals.      |
| `tchunk-pdf/src/pdf.rs`    | `boundaries()` returns `Vec<Boundary>`. Drop `from_outline_depth` heuristic.  |
| `tchunk-pdf/src/cli.rs`    | Rename clap enum to `SplitAtArg`. Add `--split-at-depth`. ArgGroup. Mapping.  |
| `tchunk-pdf/src/main.rs`   | Wire new types. Replace `>= Page` outline-fallback check. Finest comparison.  |
| `tchunk-pdf/src/index.rs`  | `&'static str` → `String` for `split_at_*` and `effective_level` fields.       |
| `tchunk-pdf/tests/end_to_end.rs` | `BoundaryLevel::Page` → `Boundary::Page`.                               |
| `tchunk-pdf/README.md`     | Document `--split-at-depth`. Update sidecar example to depth-strings.          |

---

## Task 1: Define `Boundary` and `SplitAt` types alongside `BoundaryLevel`

This task is additive — it introduces the new types and their methods *next to* the existing `BoundaryLevel`, with isolated unit tests. The build still compiles after this task because nothing else changes yet.

**Files:**
- Modify: `tchunk-pdf/src/plan.rs`

- [ ] **Step 1: Open `tchunk-pdf/src/plan.rs` and add the new enums + methods immediately after the existing `BoundaryLevel` block (after the existing `impl BoundaryLevel { ... }` ending at line 42)**

Insert this block after line 42 of `plan.rs`:

```rust
/// Per-page outline data. Records the actual outline depth, with no level-name mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    Page,
    Bookmark { depth: u32 },
}

impl Boundary {
    pub fn as_str(self) -> String {
        match self {
            Boundary::Page => "page".to_string(),
            Boundary::Bookmark { depth } => format!("depth-{depth}"),
        }
    }
}

/// What the user (or recursion) is splitting at. Named CLI flags resolve to specific
/// `SplitAt` values in `cli.rs`; everywhere else in the codebase works directly in
/// depth-space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAt {
    Page,
    AnyBookmark,
    Depth(u32),
}

impl SplitAt {
    /// True if `b` is a valid cut point at this split level.
    /// `Page` matches no `Boundary` directly — it is special-cased upstream
    /// (every page is its own unit), so this method panics on `SplitAt::Page` to surface
    /// any caller that forgot to special-case it.
    pub fn matches(&self, b: &Boundary) -> bool {
        match (self, b) {
            (SplitAt::Page, _) => unreachable!("SplitAt::Page is special-cased upstream"),
            (SplitAt::AnyBookmark, Boundary::Bookmark { .. }) => true,
            (SplitAt::AnyBookmark, Boundary::Page) => false,
            (SplitAt::Depth(n), Boundary::Bookmark { depth }) => depth <= n,
            (SplitAt::Depth(_), Boundary::Page) => false,
        }
    }

    pub fn as_str(self) -> String {
        match self {
            SplitAt::Page => "page".to_string(),
            SplitAt::AnyBookmark => "any-bookmark".to_string(),
            SplitAt::Depth(n) => format!("depth-{n}"),
        }
    }

    /// Rank this split-level on a coarsest→finest axis. Used by `main.rs` to pick the
    /// finest level used across chunks for the `split_at_effective` sidecar field.
    /// Larger return value = finer.
    ///   `Depth(N)` → `(0, N)` — coarsest, ordered by depth.
    ///   `AnyBookmark` → `(1, 0)` — finer than any specific `Depth`.
    ///   `Page` → `(2, 0)` — finest.
    pub fn finest_rank(self) -> (u8, u32) {
        match self {
            SplitAt::Depth(n) => (0, n),
            SplitAt::AnyBookmark => (1, 0),
            SplitAt::Page => (2, 0),
        }
    }
}
```

- [ ] **Step 2: Add unit tests for the new types**

Add these tests to the existing `mod tests` block in `plan.rs` (just before the closing `}` of the module):

```rust
#[test]
fn boundary_as_str_renders_depth() {
    assert_eq!(Boundary::Page.as_str(), "page");
    assert_eq!(Boundary::Bookmark { depth: 1 }.as_str(), "depth-1");
    assert_eq!(Boundary::Bookmark { depth: 7 }.as_str(), "depth-7");
}

#[test]
fn splitat_as_str_covers_all_variants() {
    assert_eq!(SplitAt::Page.as_str(), "page");
    assert_eq!(SplitAt::AnyBookmark.as_str(), "any-bookmark");
    assert_eq!(SplitAt::Depth(1).as_str(), "depth-1");
    assert_eq!(SplitAt::Depth(42).as_str(), "depth-42");
}

#[test]
fn splitat_matches_depth_threshold() {
    let any_b = SplitAt::AnyBookmark;
    let d1 = SplitAt::Depth(1);
    let d2 = SplitAt::Depth(2);
    let page = Boundary::Page;
    let b1 = Boundary::Bookmark { depth: 1 };
    let b2 = Boundary::Bookmark { depth: 2 };
    let b3 = Boundary::Bookmark { depth: 3 };

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
#[should_panic]
fn splitat_page_matches_panics_to_catch_misuse() {
    // Page is special-cased upstream; callers must not invoke matches() on it.
    let _ = SplitAt::Page.matches(&Boundary::Page);
}

#[test]
fn finest_rank_orders_coarse_to_fine() {
    use std::cmp::Ordering;
    let pairs = [
        (SplitAt::Depth(1), SplitAt::Depth(2), Ordering::Less),
        (SplitAt::Depth(2), SplitAt::Depth(1), Ordering::Greater),
        (SplitAt::Depth(5), SplitAt::AnyBookmark, Ordering::Less),
        (SplitAt::AnyBookmark, SplitAt::Depth(99), Ordering::Greater),
        (SplitAt::AnyBookmark, SplitAt::Page, Ordering::Less),
        (SplitAt::Page, SplitAt::Depth(1), Ordering::Greater),
    ];
    for (a, b, want) in pairs {
        assert_eq!(
            a.finest_rank().cmp(&b.finest_rank()),
            want,
            "{:?} vs {:?}",
            a,
            b
        );
    }
}
```

- [ ] **Step 3: Run the new tests**

```sh
cargo test -p tchunk-pdf --lib plan::tests::boundary_as_str_renders_depth plan::tests::splitat_as_str_covers_all_variants plan::tests::splitat_matches_depth_threshold plan::tests::splitat_page_matches_panics_to_catch_misuse plan::tests::finest_rank_orders_coarse_to_fine
```

Expected: all 5 tests PASS. (The other tests still pass too because we haven't touched `BoundaryLevel`.)

- [ ] **Step 4: Verify the project still compiles cleanly**

```sh
cargo check -p tchunk-pdf
```

Expected: no errors, no new warnings (other than unused-method warnings on the new types, which are fine — they get used in Task 2).

- [ ] **Step 5: Commit**

```sh
git add tchunk-pdf/src/plan.rs
git commit -m "[refactor] add Boundary and SplitAt types alongside BoundaryLevel"
```

---

## Task 2: Migrate `plan.rs` internals from `BoundaryLevel` to `Boundary` + `SplitAt`

This is the largest task. It rewrites every function in `plan.rs` to take `&[Boundary]` and `SplitAt` instead of `&[BoundaryLevel]`, updates `PlannedChunk::effective_level`, and converts every test fixture in the file. After this task, `plan.rs` no longer mentions `BoundaryLevel`. The other files (`pdf.rs`, `cli.rs`, `main.rs`) still reference `BoundaryLevel` and will fail to compile until their tasks land — that is expected.

Take this task carefully and run the test suite at the end of every step. Step 9's `cargo test -p tchunk-pdf --lib` should pass after the substep is complete; the workspace as a whole won't build until Task 6.

**Files:**
- Modify: `tchunk-pdf/src/plan.rs`

- [ ] **Step 1: Update `Diagnostic` and `PlannedChunk` field types**

In `plan.rs`, change `PlannedChunk::effective_level` from `BoundaryLevel` to `SplitAt`. `Diagnostic` is unchanged. The struct now reads:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedChunk {
    pub pages: Vec<u32>,
    /// The split level at which *this chunk's* adjacent cuts were taken. For a chunk produced at
    /// the requested `split_at`, this equals the requested level. For a chunk produced by
    /// recursing into an over-budget unit, this is the finer level the recursion landed on.
    pub effective_level: SplitAt,
}
```

- [ ] **Step 2: Rewrite `segment_units` for the new types**

Replace the existing `segment_units` (currently around lines 225-248) with:

```rust
/// Segment `page_range` into unit ranges at `split_at`. A unit starts at `page_range.start` and
/// at every interior page whose boundary qualifies under `split_at`. At `Page` level every page is
/// its own unit.
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
```

- [ ] **Step 3: Rewrite `next_effective_level` for the new types**

Replace the existing `next_effective_level` (currently around lines 252-277) with:

```rust
/// Find the coarsest level strictly finer than `current` that has at least one boundary inside
/// `page_range` (excluding the start page, which is already the unit's own boundary). Falls
/// through to `SplitAt::Page` when nothing finer has an interior split point.
fn next_effective_level(
    boundaries: &[Boundary],
    page_range: Range<usize>,
    current: SplitAt,
) -> SplitAt {
    let start = page_range.start;
    let end = page_range.end;

    // From AnyBookmark, the only step finer is Page. From Page, no step finer.
    let start_depth = match current {
        SplitAt::Page => return SplitAt::Page,
        SplitAt::AnyBookmark => return SplitAt::Page,
        SplitAt::Depth(n) => n,
    };

    // Probe Depth(start_depth + 1), Depth(start_depth + 2), ... looking for the smallest depth
    // that is *deeper than current* and has an interior boundary qualifying for it. Cap probing
    // at the maximum depth actually present in the range (no point probing beyond that).
    let max_interior_depth = (start + 1..end)
        .filter_map(|i| match boundaries[i] {
            Boundary::Bookmark { depth } => Some(depth),
            Boundary::Page => None,
        })
        .max();

    let max_d = match max_interior_depth {
        Some(d) if d > start_depth => d,
        _ => return SplitAt::Page,
    };

    for candidate in (start_depth + 1)..=max_d {
        let cand = SplitAt::Depth(candidate);
        let has_boundary = (start + 1..end).any(|i| cand.matches(&boundaries[i]));
        if has_boundary {
            return cand;
        }
    }
    SplitAt::Page
}
```

- [ ] **Step 4: Rewrite `flush_units`, `greedy_pack`, `plan_overrun`, `pack_pages_balanced` for the new types**

Replace `flush_units` (currently lines 199-216):

```rust
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
```

Replace `greedy_pack` (currently lines 89-122) — only the type signatures and parameter passing change; the body logic is identical:

```rust
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
```

Replace `plan_overrun` (currently lines 128-142):

```rust
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
```

Replace `pack_pages_balanced` (currently lines 147-197) — note `BoundaryLevel::Page` becomes `SplitAt::Page` and `&[BoundaryLevel]` becomes `&[Boundary]`:

```rust
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
```

- [ ] **Step 5: Rewrite `rebalance_last_two`, `pairwise_rebalance`, `try_rebalance_pair`, `best_balanced_cut` for the new types**

Replace `rebalance_last_two` (currently lines 281-293):

```rust
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
```

Replace `pairwise_rebalance` (currently lines 298-320):

```rust
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
```

Replace `try_rebalance_pair` (currently lines 322-350):

```rust
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
```

Replace `best_balanced_cut` (currently lines 355-391). The `cut_after_allowed` closure now uses `split_at.matches(&boundaries[idx0 + 1])` instead of `boundaries[idx0 + 1] >= split_at`. **Important:** the `idx0 + 1 == n` boundary-of-document branch must remain — `split_at.matches` would panic on `SplitAt::Page` and that case is unreachable when `split_at == SplitAt::Page` because `pack_pages_balanced` calls `pairwise_rebalance(..., SplitAt::Page, ...)` and the closure's first early-return covers the past-end case. To be safe and explicit, special-case `SplitAt::Page`:

```rust
fn best_balanced_cut(
    combined: &[u32],
    tokens: &[usize],
    boundaries: &[Boundary],
    split_at: SplitAt,
    budget: usize,
) -> Option<usize> {
    let n = tokens.len();
    let cut_after_allowed = |idx0: usize| -> bool {
        if idx0 + 1 == n {
            return true;
        }
        match split_at {
            SplitAt::Page => true, // every page boundary is a valid cut at Page level
            other => other.matches(&boundaries[idx0 + 1]),
        }
    };

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
```

- [ ] **Step 6: Rewrite `plan_chunks` public API**

Replace `plan_chunks` (currently lines 64-79):

```rust
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
```

- [ ] **Step 7: Delete the old `BoundaryLevel` enum and its `impl` block**

Remove lines 3-42 of `plan.rs` (the entire `BoundaryLevel` enum definition through the closing `}` of `impl BoundaryLevel`). The file's top-of-module section should now begin with `use std::ops::Range;` followed directly by the `Boundary` definition.

- [ ] **Step 8: Convert all unit tests in `plan.rs` to the new types**

In the existing `mod tests` block, apply these mechanical conversions throughout every test (and helper):

- `BoundaryLevel::Page` → `Boundary::Page` (when used as a `Vec<_>` element)
- `BoundaryLevel::Chapter` → `Boundary::Bookmark { depth: 1 }`
- `BoundaryLevel::Section` → `Boundary::Bookmark { depth: 2 }`
- `BoundaryLevel::Subsection` → `Boundary::Bookmark { depth: 3 }`
- `BoundaryLevel::AnyBookmark` → `Boundary::Bookmark { depth: 4 }`

For `split_at` arguments to `plan_chunks` and direct calls to `segment_units` / `next_effective_level`:

- `BoundaryLevel::Page` → `SplitAt::Page`
- `BoundaryLevel::Chapter` → `SplitAt::Depth(1)`
- `BoundaryLevel::Section` → `SplitAt::Depth(2)`
- `BoundaryLevel::Subsection` → `SplitAt::Depth(3)`
- `BoundaryLevel::AnyBookmark` → `SplitAt::AnyBookmark`

For `effective_level` assertions on chunks:

- `c.effective_level == BoundaryLevel::Page` → `c.effective_level == SplitAt::Page`
- `c.effective_level == BoundaryLevel::Section` → `c.effective_level == SplitAt::Depth(2)`
- `c.effective_level == BoundaryLevel::Chapter` → `c.effective_level == SplitAt::Depth(1)`

Update the `pages_only` helper:

```rust
fn pages_only(n: usize) -> Vec<Boundary> {
    vec![Boundary::Page; n]
}
```

Update the `next_finer_chain` test (it tested the now-deleted `next_finer` method on `BoundaryLevel`). **Replace it entirely** with a test that exercises `next_effective_level`'s probe order:

```rust
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
```

Update `next_effective_level_skips_empty_levels` to the new types:

```rust
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
```

Update `segment_units_at_page_returns_singletons`:

```rust
#[test]
fn segment_units_at_page_returns_singletons() {
    let b = pages_only(4);
    let units = segment_units(&b, 0..4, SplitAt::Page);
    assert_eq!(units, vec![0..1, 1..2, 2..3, 3..4]);
}
```

Update `segment_units_at_section_splits_on_interior_boundaries`:

```rust
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
```

For the recursive-balance tests (e.g. `oversized_chapter_recurses_to_section_and_balances`), apply the substitution table above. The test assertions about chunk page ranges and token sums stay the same. Example transformation for `oversized_chapter_recurses_to_section_and_balances`:

```rust
#[test]
fn oversized_chapter_recurses_to_section_and_balances() {
    let tokens = vec![100, 100, 100];
    let boundaries = vec![
        Boundary::Bookmark { depth: 1 },
        Boundary::Bookmark { depth: 2 },
        Boundary::Bookmark { depth: 2 },
    ];
    let r = plan_chunks(&tokens, &boundaries, SplitAt::Depth(1), 120);
    let sums = chunk_tokens(&r, &tokens);
    assert!(sums.iter().all(|&s| s <= 120), "budget violated: {:?}", sums);
    let flat: Vec<u32> = r.chunks.iter().flat_map(|c| c.pages.clone()).collect();
    assert_eq!(flat, vec![1, 2, 3]);
    assert!(
        r.chunks.iter().all(|c| c.effective_level == SplitAt::Depth(2)),
        "expected all chunks at Depth(2), got {:?}",
        r.chunks.iter().map(|c| c.effective_level).collect::<Vec<_>>()
    );
    assert!(r.diagnostics.is_empty());
}
```

Apply the same substitution to `oversized_chapter_falls_through_to_page_when_no_finer_boundaries`, `mixed_top_level_some_fit_some_recurse`, `oversized_page_inside_recursed_unit_still_fires_diagnostic`, `oversized_page_inside_multi_page_recursed_chapter_warns`, `recursed_chunks_never_mergeable_with_neighbors`, `recursed_chapter_does_not_oversplit`, `balance_improves_with_pairwise_sweep`, `split_at_section_only_cuts_at_section_boundaries`, `split_at_section_no_interior_boundary_recurses_to_page`, `split_at_chapter_emits_no_diagnostic_for_page_only_doc_when_total_fits_one_chunk`, `rebalance_respects_allowed_cuts`. Keep all assertion shapes (page lists, token sums, diagnostics) identical.

- [ ] **Step 9: Run the lib tests for `plan.rs` and verify all pass**

```sh
cargo test -p tchunk-pdf --lib plan
```

Expected: ALL `plan::tests::*` tests PASS. (`pdf.rs`, `cli.rs`, `main.rs` tests still won't compile yet — that's expected; we'll fix them in Tasks 3-6. The `--lib plan` filter sidesteps the broken modules.)

If any `plan::tests::*` test fails, fix it before proceeding. Common pitfalls:
- Off-by-one in `next_effective_level` probe range (start_depth + 1, not start_depth)
- Forgot to use `SplitAt::Depth(N)` for splitter-state assertions on recursed chunks
- Confused `split_at` (request) with `boundaries[i]` (per-page data) in some test

- [ ] **Step 10: Commit (knowing the workspace as a whole still does not build)**

```sh
git add tchunk-pdf/src/plan.rs
git commit -m "[refactor] migrate plan.rs internals to depth-based Boundary/SplitAt types"
```

---

## Task 3: Migrate `pdf.rs` to record `Boundary` from outline depth

**Files:**
- Modify: `tchunk-pdf/src/pdf.rs`

- [ ] **Step 1: Update the `use` statement**

Change line 8 of `pdf.rs` from:

```rust
use crate::plan::BoundaryLevel;
```

to:

```rust
use crate::plan::Boundary;
```

- [ ] **Step 2: Rewrite `boundaries()` to return `Vec<Boundary>` and drop the level-name mapping**

Replace the existing `boundaries()` method (currently lines 97-140) with:

```rust
/// Boundary that starts at each page (1-based indexed). Defaults to `Page` when no outline
/// entry targets that page. If the document has no `/Outlines` at all, every entry is `Page`.
/// When multiple outline entries point at the same page, keep the *coarsest* (smallest depth).
pub fn boundaries(&self) -> Vec<Boundary> {
    let mut levels = vec![Boundary::Page; self.pages.len()];
    if levels.is_empty() {
        return levels;
    }
    // First page always starts the document. depth=1 preserves prior behavior:
    // any non-Page split-at request is honored at the first page without special-casing.
    levels[0] = Boundary::Bookmark { depth: 1 };

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
        levels: &mut [Boundary],
    ) {
        for node in nodes {
            match node {
                Outline::Destination(dest) => {
                    if let Some(page) = resolve_page(dest, page_id_to_num) {
                        let idx = (page - 1) as usize;
                        if idx >= levels.len() {
                            continue;
                        }
                        // Keep the coarsest (smallest depth) entry per page.
                        match levels[idx] {
                            Boundary::Page => {
                                levels[idx] = Boundary::Bookmark { depth };
                            }
                            Boundary::Bookmark { depth: cur } if depth < cur => {
                                levels[idx] = Boundary::Bookmark { depth };
                            }
                            _ => {}
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
```

- [ ] **Step 3: Verify `pdf.rs` compiles in isolation**

```sh
cargo check -p tchunk-pdf --lib
```

The check will likely fail at `cli.rs` and `main.rs` (which still reference `BoundaryLevel`). That's fine. Look at the *errors specific to `pdf.rs`* — there should be none. If `pdf.rs` has its own errors, fix them.

- [ ] **Step 4: Commit**

```sh
git add tchunk-pdf/src/pdf.rs
git commit -m "[refactor] pdf.rs records depth-typed Boundary, drops level-name heuristic"
```

---

## Task 4: Update `cli.rs` (rename clap enum, add `--split-at-depth`, ArgGroup, mapping)

**Files:**
- Modify: `tchunk-pdf/src/cli.rs`

- [ ] **Step 1: Update the `use` statement**

Change line 6 of `cli.rs` from:

```rust
use crate::plan::BoundaryLevel;
```

to:

```rust
use crate::plan::SplitAt;
```

- [ ] **Step 2: Rename clap value enum from `SplitAt` to `SplitAtArg`, update `From` impl**

Replace lines 8-28 of `cli.rs` (the `SplitAt` value enum and its `From<SplitAt> for BoundaryLevel` impl) with:

```rust
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SplitAtArg {
    Page,
    #[value(name = "any-bookmark", alias = "bookmark")]
    AnyBookmark,
    Subsection,
    Section,
    Chapter,
}

impl From<SplitAtArg> for SplitAt {
    fn from(s: SplitAtArg) -> Self {
        match s {
            SplitAtArg::Page => SplitAt::Page,
            SplitAtArg::AnyBookmark => SplitAt::AnyBookmark,
            SplitAtArg::Subsection => SplitAt::Depth(3),
            SplitAtArg::Section => SplitAt::Depth(2),
            SplitAtArg::Chapter => SplitAt::Depth(1),
        }
    }
}
```

- [ ] **Step 3: Add `--split-at-depth` field with ArgGroup mutual exclusion**

Update the `Cli` struct's `#[command(...)]` attribute (currently lines 53-64) to add a second `ArgGroup`. The full attribute now reads:

```rust
#[derive(Debug, Parser)]
#[command(
    name = "tchunk-pdf",
    about = "Split a PDF into smaller PDFs along structural boundaries (chapter cuts by default) under a token budget.",
    version,
    group(
        ArgGroup::new("hf_source")
            .args(["tokenizer_file", "tokenizer_model"])
            .multiple(false)
            .required(false),
    ),
    group(
        ArgGroup::new("split_target")
            .args(["split_at", "split_at_depth"])
            .multiple(false)
            .required(false),
    ),
)]
```

Add the `split_at_depth` field after the existing `split_at` field. Replace the existing `split_at` block (currently lines 76-79) with:

```rust
/// Coarsest level at which a split between chunks is allowed. Outline-based levels
/// require the PDF to have a bookmarks tree;
/// otherwise they fall back to `page` with a warning. Mutually exclusive with
/// `--split-at-depth`.
#[arg(short = 's', long, value_enum, default_value_t = SplitAtArg::Chapter)]
pub split_at: SplitAtArg,

/// Coarsest outline depth at which a split is allowed. Equivalent to `--split-at chapter`
/// at depth 1, `--split-at section` at depth 2, etc., but lets you target depths beyond
/// the named flags (e.g. `--split-at-depth 4` for a deeply-nested outline). Mutually
/// exclusive with `--split-at`.
#[arg(long = "split-at-depth", value_name = "N")]
pub split_at_depth: Option<u32>,
```

- [ ] **Step 4: Add a resolution method that returns the effective `SplitAt`**

Add this method to the `impl Cli { ... }` block (after `validate`, before `expand_inputs`):

```rust
/// Resolve the user's split-at request to a `SplitAt`. `--split-at-depth N` takes
/// precedence over the named `--split-at` flag when both are supplied (clap's
/// ArgGroup ensures they aren't, but be explicit anyway).
pub fn resolved_split_at(&self) -> SplitAt {
    match self.split_at_depth {
        Some(n) => SplitAt::Depth(n),
        None => self.split_at.into(),
    }
}
```

- [ ] **Step 5: Add a clap-rejection test for the mutual exclusion**

Add this test inside the existing `#[cfg(test)] mod tests` block in `cli.rs`:

```rust
#[test]
fn split_at_and_split_at_depth_are_mutually_exclusive() {
    use clap::Parser;
    let result = Cli::try_parse_from([
        "tchunk-pdf",
        "input.pdf",
        "--split-at",
        "chapter",
        "--split-at-depth",
        "2",
    ]);
    assert!(result.is_err(), "expected ArgGroup conflict, got: {:?}", result);
}

#[test]
fn split_at_depth_resolves_to_depth_variant() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["tchunk-pdf", "input.pdf", "--split-at-depth", "5"])
        .expect("parse");
    assert_eq!(cli.resolved_split_at(), SplitAt::Depth(5));
}

#[test]
fn split_at_chapter_resolves_to_depth_1() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["tchunk-pdf", "input.pdf", "--split-at", "chapter"])
        .expect("parse");
    assert_eq!(cli.resolved_split_at(), SplitAt::Depth(1));
}

#[test]
fn split_at_any_bookmark_resolves_to_anybookmark() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["tchunk-pdf", "input.pdf", "--split-at", "any-bookmark"])
        .expect("parse");
    assert_eq!(cli.resolved_split_at(), SplitAt::AnyBookmark);
}

#[test]
fn default_split_at_is_chapter_depth_1() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["tchunk-pdf", "input.pdf"]).expect("parse");
    assert_eq!(cli.resolved_split_at(), SplitAt::Depth(1));
}
```

- [ ] **Step 6: Run `cli.rs` tests**

```sh
cargo test -p tchunk-pdf --lib cli
```

Expected: all `cli::tests::*` tests PASS, including the 5 new ones. (Other modules' tests still don't build; the filter ducks them.)

- [ ] **Step 7: Commit**

```sh
git add tchunk-pdf/src/cli.rs
git commit -m "[feat] cli: add --split-at-depth, rename SplitAt enum to SplitAtArg"
```

---

## Task 5: Update `main.rs` (wire new types, replace `>= Page` outline-fallback check, finest comparison)

**Files:**
- Modify: `tchunk-pdf/src/main.rs`

- [ ] **Step 1: Update the `use` statement**

Change line 13 of `main.rs` from:

```rust
use tchunk_pdf::plan::{plan_chunks, BoundaryLevel, Diagnostic};
```

to:

```rust
use tchunk_pdf::plan::{plan_chunks, Boundary, Diagnostic, SplitAt};
```

- [ ] **Step 2: Update the split-at resolution and outline-fallback logic**

Replace the block currently at lines 131-147 of `main.rs` (the `requested_split_at` computation through the outline-fallback `if`) with:

```rust
let requested_split_at: SplitAt = cli.resolved_split_at();
let mut split_at = requested_split_at;
let mut boundaries = pdf.boundaries();

if split_at != SplitAt::Page && !pdf.has_outline() {
    if !cli.quiet {
        eprintln!(
            "warning: no outline present in PDF; --split-at {} ignored, falling back to page.",
            requested_split_at.as_str(),
        );
    }
    warnings.push(Warning::OutlineMissing {
        requested: requested_split_at.as_str(),
    });
    split_at = SplitAt::Page;
    boundaries = vec![Boundary::Page; page_count];
}
```

(Note: the warning message now interpolates `requested_split_at.as_str()` directly, since `cli.split_at` no longer holds the canonical name once `--split-at-depth` is in play.)

- [ ] **Step 3: Update verbose-mode logging to format `split_at` via `as_str()`**

Around line 182 of `main.rs`, the verbose log currently uses `{:?}` for `split_at`. Replace:

```rust
if cli.verbose {
    eprintln!(
        "tchunk-pdf: {} pages -> {} chunks (budget {} tokens, split-at {:?}, tokenizer {})",
        page_count,
        total,
        cli.max_tokens,
        split_at,
        tokenizer.name(),
    );
}
```

with:

```rust
if cli.verbose {
    eprintln!(
        "tchunk-pdf: {} pages -> {} chunks (budget {} tokens, split-at {}, tokenizer {})",
        page_count,
        total,
        cli.max_tokens,
        split_at.as_str(),
        tokenizer.name(),
    );
}
```

Similarly, around line 213-217 the per-chunk verbose line uses `chunk.effective_level.as_str()` — that already works (the new `SplitAt::as_str()` method exists), but `as_str()` now returns `String` rather than `&'static str`, so the `format!` continues to work without changes.

- [ ] **Step 4: Update the finest-level reduce**

Replace the block currently at lines 241-246 of `main.rs` (the `effective_level = plan.chunks.iter().map(|c| c.effective_level).min().unwrap_or(split_at);`) with:

```rust
// effective_level is the *finest* level used across chunks (worst-case view of how far
// recursion descended). Larger finest_rank() = finer.
let effective_level: SplitAt = plan
    .chunks
    .iter()
    .map(|c| c.effective_level)
    .max_by_key(|s| s.finest_rank())
    .unwrap_or(split_at);
```

- [ ] **Step 5: Update the `Index`/`Config` field assignments to pass owned strings**

The `Config` struct will have `String`-typed fields after Task 6. For now, make `main.rs` produce `String`s from the `as_str()` calls:

```rust
let index = Index {
    tool: "tchunk-pdf",
    version: env!("CARGO_PKG_VERSION"),
    source: Source {
        path: input.display().to_string(),
        page_count,
        total_tokens: tokens.iter().sum(),
    },
    config: Config {
        tokenizer: tokenizer.name().to_string(),
        max_tokens: cli.max_tokens,
        split_at_requested: requested_split_at.as_str(),
        split_at_effective: effective_level.as_str(),
    },
    chunks: chunk_entries,
    warnings,
};
```

(`requested_split_at.as_str()` and `effective_level.as_str()` already return `String`.)

- [ ] **Step 6: Update the per-chunk `effective_level` field push to pass an owned string**

Around line 232 of `main.rs`, replace:

```rust
chunk_entries.push(ChunkEntry {
    filename,
    pages: Pages {
        start: first,
        end: last,
        count: page_nums.len(),
    },
    token_count: tok_sum,
    effective_level: chunk.effective_level.as_str(),
});
```

with the same shape — `chunk.effective_level.as_str()` now returns `String`, which we'll match in Task 6's `index.rs` change.

- [ ] **Step 7: Update the `Warning::OutlineMissing` payload**

`Warning::OutlineMissing { requested: requested_split_at.as_str() }` — the `requested` field's type changes to `String` in Task 6. For now, this line produces `String` from `as_str()`, which is what we want.

- [ ] **Step 8: Verify (workspace still won't link until Task 6, but `main.rs` errors should be type-only on the index/warning fields)**

```sh
cargo check -p tchunk-pdf
```

Expected: errors limited to the `String` vs `&'static str` mismatch in `index.rs` field types and `Warning::OutlineMissing { requested: ... }`. No errors in `main.rs`'s use of `plan` types.

- [ ] **Step 9: Commit**

```sh
git add tchunk-pdf/src/main.rs
git commit -m "[refactor] main.rs wires SplitAt; uses finest_rank for effective level"
```

---

## Task 6: Migrate `index.rs` field types from `&'static str` to `String`

**Files:**
- Modify: `tchunk-pdf/src/index.rs`

- [ ] **Step 1: Change the field types on `Config`, `ChunkEntry`, and `Warning::OutlineMissing`**

In `index.rs`, replace the `Config` struct (currently lines 23-29) with:

```rust
#[derive(Serialize)]
pub struct Config {
    pub tokenizer: String,
    pub max_tokens: usize,
    pub split_at_requested: String,
    pub split_at_effective: String,
}
```

Replace the `ChunkEntry` struct (currently lines 32-37) with:

```rust
#[derive(Serialize)]
pub struct ChunkEntry {
    pub filename: String,
    pub pages: Pages,
    pub token_count: usize,
    pub effective_level: String,
}
```

Replace the `Warning::OutlineMissing` variant (in the enum starting at line 47) by changing the field type:

```rust
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Warning {
    OutlineMissing {
        requested: String,
    },
    OversizedPage {
        page: u32,
        tokens: usize,
    },
    ScanLike {
        near_empty_pages: usize,
        total_pages: usize,
    },
    ImageDominant {
        pages_affected: usize,
        total_pages: usize,
    },
}
```

- [ ] **Step 2: Update the unit tests in `index.rs`**

Update `serializes_minimal_index` (currently lines 81-108) to use owned strings:

```rust
#[test]
fn serializes_minimal_index() {
    let idx = Index {
        tool: "tchunk-pdf",
        version: "0.1.0",
        source: Source {
            path: "book.pdf".to_string(),
            page_count: 10,
            total_tokens: 1234,
        },
        config: Config {
            tokenizer: "cl100k_base".to_string(),
            max_tokens: 500_000,
            split_at_requested: "page".to_string(),
            split_at_effective: "page".to_string(),
        },
        chunks: vec![ChunkEntry {
            filename: "book_001.pdf".to_string(),
            pages: Pages { start: 1, end: 10, count: 10 },
            token_count: 1234,
            effective_level: "page".to_string(),
        }],
        warnings: vec![],
    };
    let json = serde_json::to_string(&idx).unwrap();
    assert!(json.contains("\"tool\":\"tchunk-pdf\""));
    assert!(json.contains("\"pages\":{\"start\":1,\"end\":10,\"count\":10}"));
    assert!(json.contains("\"warnings\":[]"));
}
```

Update `outline_missing_serializes_requested` (currently lines 117-122):

```rust
#[test]
fn outline_missing_serializes_requested() {
    let w = Warning::OutlineMissing { requested: "depth-1".to_string() };
    let json = serde_json::to_string(&w).unwrap();
    assert_eq!(json, r#"{"kind":"outline_missing","requested":"depth-1"}"#);
}
```

(`warnings_serialize_with_kind_tag` is unchanged — it tests `OversizedPage` which has no string fields.)

- [ ] **Step 3: Run the index tests**

```sh
cargo test -p tchunk-pdf --lib index
```

Expected: all 3 tests PASS.

- [ ] **Step 4: Verify the whole library compiles**

```sh
cargo check -p tchunk-pdf
```

Expected: no errors. (The `tests/end_to_end.rs` integration-test target may still have errors — Task 7 fixes that.)

- [ ] **Step 5: Commit**

```sh
git add tchunk-pdf/src/index.rs
git commit -m "[refactor] index.rs sidecar fields become String to carry depth-N strings"
```

---

## Task 7: Update integration tests (`tests/end_to_end.rs`)

**Files:**
- Modify: `tchunk-pdf/tests/end_to_end.rs`

- [ ] **Step 1: Replace `BoundaryLevel` references**

Open `tchunk-pdf/tests/end_to_end.rs`. Update line 8 — the `use` statement — from:

```rust
use tchunk_pdf::plan::{plan_chunks, BoundaryLevel};
```

to:

```rust
use tchunk_pdf::plan::{plan_chunks, Boundary, SplitAt};
```

Replace the two `vec![BoundaryLevel::Page; N]` occurrences (currently around lines 89 and 148):

```rust
let boundaries = vec![Boundary::Page; 6];
```

```rust
let boundaries = vec![Boundary::Page; 3];
```

Replace the two `BoundaryLevel::Page` arguments to `plan_chunks` (currently lines 93 and 150):

```rust
let plan = plan_chunks(&tokens, &boundaries, SplitAt::Page, budget);
```

```rust
let plan = plan_chunks(&tokens, &boundaries, SplitAt::Page, 10_000);
```

- [ ] **Step 2: Run all tests**

```sh
cargo test -p tchunk-pdf
```

Expected: ALL tests pass (lib + integration). If anything fails, fix it in place.

- [ ] **Step 3: Commit**

```sh
git add tchunk-pdf/tests/end_to_end.rs
git commit -m "[refactor] end-to-end tests use depth-based plan types"
```

---

## Task 8: Whole-project verification

**Files:** none modified

- [ ] **Step 1: Full clean build**

```sh
cargo clean -p tchunk-pdf && cargo build -p tchunk-pdf
```

Expected: builds without errors. Any warnings about unused imports / unused methods on the new types should be addressed.

- [ ] **Step 2: Full test run**

```sh
cargo test -p tchunk-pdf
```

Expected: every test passes.

- [ ] **Step 3: Smoke test the binary against a real PDF (skip if no PDF available)**

If you have a PDF with an outline handy:

```sh
cargo run -p tchunk-pdf --release -- <path-to-pdf> -o /tmp/tchunk-smoke -m 100000 -v
```

Inspect `/tmp/tchunk-smoke/<stem>.index.json`. Verify:
- `config.split_at_requested` is `"depth-1"` (since the default is `--split-at chapter` which now resolves to `Depth(1)`).
- `config.split_at_effective` is `"depth-1"` or some `"depth-N"` if recursion descended, or `"page"` in the worst case.
- Per-chunk `effective_level` strings match the same depth-N or `"page"` shape.

Try `--split-at-depth 2`:

```sh
cargo run -p tchunk-pdf --release -- <path-to-pdf> -o /tmp/tchunk-smoke -m 100000 --split-at-depth 2
```

Verify `split_at_requested` becomes `"depth-2"`.

Try the mutual exclusion:

```sh
cargo run -p tchunk-pdf --release -- <path-to-pdf> --split-at chapter --split-at-depth 2
```

Expected: clap rejects with an "argument conflict" error.

- [ ] **Step 4: No commit needed for this task — verification only.**

---

## Task 9: Update the README

**Files:**
- Modify: `tchunk-pdf/README.md`

- [ ] **Step 1: Update the options table**

In `tchunk-pdf/README.md`, update the row for `--split-at` (currently around line 58) to mention the new flag and add a row for `--split-at-depth`. Replace the existing table row:

```
| `-s`  | `--split-at`     | `chapter`     | Coarsest level a split is allowed at: `page`, `any-bookmark`, `subsection`, `section`, `chapter`. Outline-based levels fall back to `page` with a warning if the PDF has no bookmarks. |
```

with these two rows:

```
| `-s`  | `--split-at`     | `chapter`     | Coarsest level a split is allowed at: `page`, `any-bookmark`, `subsection`, `section`, `chapter`. Named flags are sugar for specific outline depths (`chapter`=1, `section`=2, `subsection`=3); `any-bookmark` matches any depth. Outline-based levels fall back to `page` with a warning if the PDF has no bookmarks. Mutually exclusive with `--split-at-depth`. |
|       | `--split-at-depth` | —           | Split at a specific outline depth (e.g. `--split-at-depth 4` for outlines deeper than the named flags reach). Mutually exclusive with `--split-at`. |
```

- [ ] **Step 2: Update the splitting-behavior section**

Find the bullet around line 110 that reads:

```
- **Structural splits** are the default. `--split-at chapter` (the default), `section`, `subsection`, and `any-bookmark` all use the PDF outline (bookmarks). Outline depth maps to level: depth 1 → chapter, depth 2 → section, depth 3 → subsection, deeper → any-bookmark. Use `--split-at page` to ignore the outline and cut on any page boundary.
```

Replace with:

```
- **Structural splits** are the default. `--split-at chapter` (the default), `section`, `subsection`, and `any-bookmark` all use the PDF outline (bookmarks). Named flags map to specific outline depths: `chapter`=1, `section`=2, `subsection`=3. `any-bookmark` matches every outline entry regardless of depth. For outlines whose top level isn't called "chapter" (e.g. Parts/Chapters books, where chapters are at depth 2), use `--split-at-depth N` to target the actual depth. Use `--split-at page` to ignore the outline entirely.
```

- [ ] **Step 3: Update the sidecar example to depth-strings**

Find the JSON sidecar example around line 82-100 and update the `config` and `chunks` blocks:

Replace:

```
  "config": {
    "tokenizer": "o200k_base",
    "max_tokens": 500000,
    "split_at_requested": "chapter",
    "split_at_effective": "section"
  },
  "chunks": [
    { "filename": "my-book_001.pdf", "pages": { "start": 1, "end": 112, "count": 112 }, "token_count": 487234, "effective_level": "chapter" },
    { "filename": "my-book_002.pdf", "pages": { "start": 113, "end": 220, "count": 108 }, "token_count": 441200, "effective_level": "section" },
    { "filename": "my-book_003.pdf", "pages": { "start": 221, "end": 320, "count": 100 }, "token_count": 412118, "effective_level": "section" }
  ],
```

with:

```
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
```

- [ ] **Step 4: Update the explanatory paragraph after the JSON**

Find the paragraph around line 102 starting with `` `split_at_effective` is the *finest* level actually used... ``:

Replace:

```
`split_at_effective` is the *finest* level actually used across chunks (the worst-case view of how far recursion had to descend). `effective_level` on each chunk is the level at which *that chunk's* adjacent cuts were taken — for chunks that fit cleanly at the requested level it matches the request; for chunks produced by recursing into an over-budget unit it shows the finer level the recursion landed on.
```

with:

```
`split_at_effective` is the *finest* level actually used across chunks (the worst-case view of how far recursion had to descend). `effective_level` on each chunk is the level at which *that chunk's* adjacent cuts were taken — for chunks that fit cleanly at the requested level it matches the request; for chunks produced by recursing into an over-budget unit it shows the finer level the recursion landed on. Both fields use the canonical depth strings (`"page"`, `"any-bookmark"`, or `"depth-N"`); the named CLI flags (`chapter`, `section`, `subsection`) are sugar for specific depths on input only and don't round-trip into the sidecar.
```

- [ ] **Step 5: Update the recursion-chain bullet**

Find the bullet around line 112 that mentions the chain:

```
- **Over-budget units recurse.** If a single unit (e.g. one chapter) exceeds `--max-tokens`, tchunk-pdf treats that unit as its own sub-problem and re-plans it at the next finer level (chapter → section → subsection → any-bookmark → page) ...
```

Replace with:

```
- **Over-budget units recurse.** If a single unit (e.g. one chapter) exceeds `--max-tokens`, tchunk-pdf treats that unit as its own sub-problem and re-plans it at the next finer outline depth (depth-1 → depth-2 → depth-3 → ... → page), balancing its sibling sub-chunks against each other rather than packing greedy-first-fit. Recursion falls through any depth with no interior boundaries. Per-chunk `effective_level` in the index sidecar shows which depth each chunk's cuts were actually taken at.
```

- [ ] **Step 6: Commit**

```sh
git add tchunk-pdf/README.md
git commit -m "[docs] document --split-at-depth and depth-string sidecar output"
```

---

## Self-review notes for the implementer

Common failure modes for this kind of refactor:

- **Forgetting that `SplitAt::Page.matches(...)` panics.** It's called from inside the closure in `best_balanced_cut`. Special-case `SplitAt::Page` before calling `matches`.
- **`AnyBookmark` recursion direction.** `AnyBookmark` recurses *to* `Page` (one step finer), not to a specific depth. The `next_effective_level` function handles this with an early return.
- **First-page seed.** `pdf.rs::boundaries()` initializes `levels[0] = Boundary::Bookmark { depth: 1 }` even when the PDF has no outline. `main.rs` then immediately overwrites the whole `boundaries` vec with `Page` if `!pdf.has_outline()`, so the seed only matters when an outline exists but doesn't include page 1. Don't simplify the seed away; some real PDFs don't bookmark page 1.
- **`split_at_requested` in `Warning::OutlineMissing`.** The string is now a `String`, not a `&'static str`. The fallback path in `main.rs` builds it from `requested_split_at.as_str()`.
- **Test-fixture conversions.** `BoundaryLevel::AnyBookmark` as test fixture data → `Boundary::Bookmark { depth: 4 }`. The literal `4` is arbitrary; only its position in the depth ordering matters for the tests that use it.

If `cargo test` fails after Task 8, look at which test failed and trace it to one of the substitutions above.
