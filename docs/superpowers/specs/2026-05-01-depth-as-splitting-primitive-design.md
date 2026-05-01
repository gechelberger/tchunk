# Depth as the splitting primitive — design

Date: 2026-05-01
Status: approved, ready for implementation planning

## Motivation

Today's `BoundaryLevel` enum (`Chapter, Section, Subsection, AnyBookmark, Page`)
bakes in one outline convention. Books that put Parts at the top with Chapters
nested inside, or whose outline starts with a title node, are misfit by
`from_outline_depth`. The 0|1 → Chapter merge is a defensive heuristic for one
specific outline shape that silently lies for others.

The fix: make raw outline depth the native primitive, and demote the named
levels (`chapter`/`section`/`subsection`) to syntactic sugar over specific
depths.

This refactor is scoped to the type-shape change. The sidecar field rename
(`split_at_effective` → `finest_level_used`/`split_at_finest`),
`--show-outline`, the pointier oversized-page warning, and the HF tokenizer
fetch progress message are explicitly out of scope and tracked in
`tchunk-pdf/next-steps.md`.

## Type changes (`plan.rs`)

Replace the single `BoundaryLevel` enum with two distinct types — one for
per-page data, one for the user/recursion target:

```rust
// per-page data, recorded by pdf.rs from the outline walk.
// Carries the actual outline depth, no level-name mapping.
pub enum Boundary {
    Page,
    Bookmark { depth: u32 },
}

// what the user (or recursion) is splitting at.
pub enum SplitAt {
    Page,
    AnyBookmark,
    Depth(u32),  // matches Bookmark{d} where d <= N
}
```

`BoundaryLevel::PartialOrd`/`Ord` are dropped. The `lvl >= split_at`
comparisons in `segment_units`, `next_effective_level`, and
`best_balanced_cut` become a `SplitAt::matches(&self, b: Boundary) -> bool`
method:

```rust
impl SplitAt {
    fn matches(&self, b: &Boundary) -> bool {
        match (self, b) {
            (SplitAt::Page, _) => unreachable!("Page is special-cased upstream"),
            (SplitAt::AnyBookmark, Boundary::Bookmark { .. }) => true,
            (SplitAt::AnyBookmark, Boundary::Page) => false,
            (SplitAt::Depth(n), Boundary::Bookmark { depth }) => depth <= n,
            (SplitAt::Depth(_), Boundary::Page) => false,
        }
    }
}
```

The recursion chain (`next_finer`) becomes:
- `SplitAt::Depth(N) → SplitAt::Depth(N+1) → ...` (probing for the next depth
  that has interior boundaries inside the unit; falls through to
  `SplitAt::Page` when no deeper bookmarks exist).
- `SplitAt::AnyBookmark → SplitAt::Page`.
- `SplitAt::Page → None`.

`next_effective_level` keeps the skip-empty-levels behavior: starting from
`current+1`, find the smallest depth `d` such that some interior page in the
range carries a `Boundary::Bookmark{depth: d2}` with `d2 <= d`. If none,
return `SplitAt::Page`. (When `current` is `AnyBookmark`, jump straight to
`Page`.)

## CLI (`cli.rs`)

Keep the existing `--split-at` named-value flag (`page | any-bookmark |
subsection | section | chapter`), but resolve it to `SplitAt`:

| `--split-at`    | resolves to              |
|-----------------|--------------------------|
| `page`          | `SplitAt::Page`          |
| `any-bookmark`  | `SplitAt::AnyBookmark`   |
| `subsection`    | `SplitAt::Depth(3)`      |
| `section`       | `SplitAt::Depth(2)`      |
| `chapter`       | `SplitAt::Depth(1)`      |

Add a peer flag `--split-at-depth <N>` (`u32`) that resolves to
`SplitAt::Depth(N)` directly. Mutually exclusive with `--split-at` via clap
`ArgGroup` (same shape as the existing `hf_source` group on `--tokenizer-file`
/ `--tokenizer-model`). Default stays `--split-at chapter`.

The `SplitAt` enum from clap (currently named `SplitAt` in `cli.rs` — rename
the CLI value enum to avoid collision with the new `plan::SplitAt` type;
`cli::SplitAtArg` or similar).

## Outline walk (`pdf.rs`)

Drop the 0|1 → Chapter merge in `from_outline_depth` — that function goes
away. The walk already starts at depth 1, so depths in the recorded
`Vec<Boundary>` are 1, 2, 3, … as nested.

Per-page boundary recording: when multiple outline entries point at the same
page, keep the *coarsest* one (smallest depth). Today's code keeps the
"max enum value" via `if lvl > levels[idx] { levels[idx] = lvl; }`, which
maps to "smallest depth wins" in the new model:

```rust
match (&levels[idx], &incoming) {
    (Boundary::Page, _) => levels[idx] = incoming,
    (Boundary::Bookmark { depth: cur }, Boundary::Bookmark { depth: new }) if new < cur => {
        levels[idx] = incoming;
    }
    _ => {}
}
```

First-page seed: today's code initializes `levels[0] = BoundaryLevel::Chapter`
unconditionally so that any first-page request `> Page` is honored without
special-casing. The new equivalent is `levels[0] = Boundary::Bookmark { depth: 1 }`.
This preserves behavior: a `--split-at chapter` (depth 1) request matches the
first page's depth-1 default, even when the PDF has no actual outline entry
on page 1.

## Main flow (`main.rs`)

The `split_at > BoundaryLevel::Page` outline-fallback check becomes
`split_at != SplitAt::Page` (i.e., any non-Page request needs an outline; Page
doesn't).

`PlannedChunk::effective_level` becomes `SplitAt`-typed (it's the cut
threshold the chunk's adjacent cuts were taken at — same conceptual type as
the user's request). The aggregate `effective_level` `min` reporting
currently relies on `BoundaryLevel`'s derived `Ord`. Replace with an explicit
"finest" comparison over `SplitAt`, ordering coarsest → finest:

`Depth(1) < Depth(2) < ... < Depth(N) < AnyBookmark < Page`

(Larger `Depth(N)` = more boundaries = finer. `AnyBookmark` matches every
bookmark regardless of depth, so it's strictly finer than any specific
`Depth(N)`. `Page` is finest. The `AnyBookmark`-vs-`Depth` ordering matters
only when a single run produces chunks at both — e.g., a unit whose interior
has no further bookmarks recurses straight to `Page` while a sibling lands
at `Depth(2)` — and the rule above gives the right answer in that case.)

## Sidecar strings (`as_str` outputs, no field renames)

| value                                | string         |
|--------------------------------------|----------------|
| `Boundary::Page`                     | `"page"`       |
| `Boundary::Bookmark { depth: N }`    | `"depth-N"`    |
| `SplitAt::Page`                      | `"page"`       |
| `SplitAt::AnyBookmark`               | `"any-bookmark"` |
| `SplitAt::Depth(N)`                  | `"depth-N"`    |

`split_at_requested`, `split_at_effective`, and per-chunk `effective_level`
keep their field names. The only behavior change in the sidecar is that
named-flag input no longer round-trips to its name: `--split-at chapter` now
produces `"split_at_requested": "depth-1"` instead of `"chapter"`. This is
the "surface depth honestly" principle — the named flags are sugar on input
only.

The `&'static str` typing on `Config::split_at_requested`,
`Config::split_at_effective`, and `ChunkEntry::effective_level` (in
`index.rs`) becomes `String`, since `"depth-N"` strings are formatted at
runtime from the `N` value.

## Tests

`Vec<BoundaryLevel>` fixtures in `plan.rs` convert mechanically:

| old                          | new                                |
|------------------------------|------------------------------------|
| `BoundaryLevel::Chapter`     | `Boundary::Bookmark { depth: 1 }`  |
| `BoundaryLevel::Section`     | `Boundary::Bookmark { depth: 2 }`  |
| `BoundaryLevel::Subsection`  | `Boundary::Bookmark { depth: 3 }`  |
| `BoundaryLevel::AnyBookmark` | `Boundary::Bookmark { depth: 4 }`  |
| `BoundaryLevel::Page`        | `Boundary::Page`                   |

The numeric mapping for `AnyBookmark`-as-fixture is arbitrary — tests don't
care about the literal depth, only that it sorts as "finer than Subsection
when used as a request." Existing assertions about `effective_level` strings
update to the new format (e.g. `"chapter"` → `"depth-1"`).

Recursion-chain tests (`next_finer_chain`,
`next_effective_level_skips_empty_levels`) are rewritten against the new
types. Behavior-level tests (`oversized_chapter_recurses_to_section_and_balances`,
`recursed_chunks_never_mergeable_with_neighbors`,
`recursed_chapter_does_not_oversplit`, etc.) keep the same input shape with
the new boundary types and the same assertions on chunk page ranges and
sums.

End-to-end tests in `tests/end_to_end.rs` use `BoundaryLevel::Page` only,
which becomes `Boundary::Page` — no other changes needed there.

## Risks

- **0|1 merge dropped**: a book whose outline has a title node at depth 1
  with chapters nested at depth 2 will now treat title-cut as
  `--split-at-depth 1` and chapter-cut as `--split-at-depth 2`. Users wanting
  chapter behavior pass `--split-at-depth 2`. Worth a brief README note that
  named flags map to specific depths and `--split-at-depth N` is the
  general-purpose escape hatch.
- **`split_at_requested` string change**: pre-1.0; documented in the
  README's sidecar example.
- **Naming collision**: `cli::SplitAt` (clap value enum) and the new
  `plan::SplitAt` type. Rename the CLI enum (`cli::SplitAtArg` or similar)
  to keep both unambiguous at use sites in `main.rs`.

## Out of scope (deferred)

- `split_at_effective` → `finest_level_used` / `split_at_finest` rename.
- `--show-outline` discoverability flag.
- Pointier oversized-page warning.
- HF tokenizer fetch progress message on cache miss.
- Cross-unit carry-in/carry-out fold + post-sweep in `greedy_pack`.

All five remain tracked in `tchunk-pdf/next-steps.md`.
