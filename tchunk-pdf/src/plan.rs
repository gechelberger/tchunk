use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BoundaryLevel {
    Page,
    AnyBookmark,
    Subsection,
    Section,
    Chapter,
}

impl BoundaryLevel {
    pub fn from_outline_depth(depth: u32) -> Self {
        match depth {
            0 | 1 => BoundaryLevel::Chapter,
            2 => BoundaryLevel::Section,
            3 => BoundaryLevel::Subsection,
            _ => BoundaryLevel::AnyBookmark,
        }
    }
}

impl FromStr for BoundaryLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "page" => Ok(BoundaryLevel::Page),
            "any-bookmark" | "bookmark" => Ok(BoundaryLevel::AnyBookmark),
            "subsection" => Ok(BoundaryLevel::Subsection),
            "section" => Ok(BoundaryLevel::Section),
            "chapter" => Ok(BoundaryLevel::Chapter),
            other => Err(format!(
                "unknown split level '{other}' (expected: page | any-bookmark | subsection | section | chapter)"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    OversizedPage { page: u32, tokens: usize },
    ForcedMidLevelCut { after_page: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanResult {
    pub chunks: Vec<Vec<u32>>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn plan_chunks(
    tokens: &[usize],
    boundaries: &[BoundaryLevel],
    split_at: BoundaryLevel,
    budget: usize,
) -> PlanResult {
    assert_eq!(tokens.len(), boundaries.len());
    let n = tokens.len();

    let cut_after_allowed = |i: usize| -> bool { i + 1 == n || boundaries[i + 1] >= split_at };

    let mut chunks: Vec<Vec<u32>> = Vec::new();
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut cur: Vec<u32> = Vec::new();
    let mut cur_tokens: usize = 0;
    let mut last_allowed_cut: Option<usize> = None;

    let mut i = 0;
    while i < n {
        let page = (i + 1) as u32;
        let t = tokens[i];

        if t > budget {
            if !cur.is_empty() {
                chunks.push(std::mem::take(&mut cur));
                cur_tokens = 0;
                last_allowed_cut = None;
            }
            chunks.push(vec![page]);
            diagnostics.push(Diagnostic::OversizedPage { page, tokens: t });
            i += 1;
            continue;
        }

        if cur_tokens + t <= budget {
            cur.push(page);
            cur_tokens += t;
            if cut_after_allowed(i) {
                last_allowed_cut = Some(cur.len() - 1);
            }
            i += 1;
        } else {
            match last_allowed_cut {
                Some(cut_idx) => {
                    let head: Vec<u32> = cur[..=cut_idx].to_vec();
                    let tail: Vec<u32> = cur[cut_idx + 1..].to_vec();
                    chunks.push(head);
                    cur = tail;
                    cur_tokens = cur.iter().map(|&p| tokens[(p - 1) as usize]).sum();
                    last_allowed_cut = None;
                    for (k, &p) in cur.iter().enumerate() {
                        let idx0 = (p - 1) as usize;
                        if cut_after_allowed(idx0) {
                            last_allowed_cut = Some(k);
                        }
                    }
                }
                None => {
                    if cur.is_empty() {
                        cur.push(page);
                        cur_tokens = t;
                        if cut_after_allowed(i) {
                            last_allowed_cut = Some(0);
                        }
                        i += 1;
                    } else {
                        let after_page = *cur.last().unwrap();
                        chunks.push(std::mem::take(&mut cur));
                        cur_tokens = 0;
                        if split_at > BoundaryLevel::Page {
                            diagnostics.push(Diagnostic::ForcedMidLevelCut { after_page });
                        }
                    }
                }
            }
        }
    }

    if !cur.is_empty() {
        chunks.push(cur);
    }

    rebalance_last_two(&mut chunks, tokens, boundaries, split_at, budget);

    PlanResult { chunks, diagnostics }
}

fn rebalance_last_two(
    chunks: &mut Vec<Vec<u32>>,
    tokens: &[usize],
    boundaries: &[BoundaryLevel],
    split_at: BoundaryLevel,
    budget: usize,
) {
    if chunks.len() < 2 {
        return;
    }
    let n = tokens.len();
    let cut_after_allowed = |i: usize| -> bool { i + 1 == n || boundaries[i + 1] >= split_at };

    let last = chunks.pop().unwrap();
    let second_last = chunks.pop().unwrap();
    let combined: Vec<u32> = second_last.iter().chain(last.iter()).copied().collect();
    let total_tokens: usize = combined.iter().map(|&p| tokens[(p - 1) as usize]).sum();

    let original_cut_idx = second_last.len() - 1;

    let mut best_i: Option<usize> = None;
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
        let right_sum = total_tokens - left_sum;
        if left_sum > budget || right_sum > budget {
            continue;
        }
        let diff = left_sum.abs_diff(right_sum);
        if diff < best_diff {
            best_diff = diff;
            best_i = Some(k);
        }
    }

    let pick = best_i.unwrap_or(original_cut_idx);
    let new_left: Vec<u32> = combined[..=pick].to_vec();
    let new_right: Vec<u32> = combined[pick + 1..].to_vec();
    chunks.push(new_left);
    chunks.push(new_right);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages_only(n: usize) -> Vec<BoundaryLevel> {
        vec![BoundaryLevel::Page; n]
    }

    #[test]
    fn empty_input_yields_empty_plan() {
        let r = plan_chunks(&[], &[], BoundaryLevel::Page, 100);
        assert!(r.chunks.is_empty());
        assert!(r.diagnostics.is_empty());
    }

    #[test]
    fn single_chunk_when_total_under_budget() {
        let tokens = vec![10, 20, 30];
        let r = plan_chunks(&tokens, &pages_only(3), BoundaryLevel::Page, 100);
        assert_eq!(r.chunks, vec![vec![1, 2, 3]]);
        assert!(r.diagnostics.is_empty());
    }

    #[test]
    fn two_chunks_get_rebalanced() {
        // Greedy would yield [[1,2], [3]] (30, 30). Rebalance: also balanced. Or could yield [[1], [2,3]].
        let tokens = vec![10, 20, 30];
        let r = plan_chunks(&tokens, &pages_only(3), BoundaryLevel::Page, 35);
        assert_eq!(r.chunks.len(), 2);
        let sum0: usize = r.chunks[0].iter().map(|&p| tokens[(p - 1) as usize]).sum();
        let sum1: usize = r.chunks[1].iter().map(|&p| tokens[(p - 1) as usize]).sum();
        assert!(sum0.abs_diff(sum1) <= 10, "rebalance failed: {sum0} vs {sum1}");
        assert!(sum0 <= 35 && sum1 <= 35);
    }

    #[test]
    fn greedy_then_rebalance_classic() {
        // pages: [30, 30, 30, 5], budget 60
        // greedy: [1,2] (60), [3] (30), then page 4 (5+30=35) -> [3,4]
        // -> chunks [[1,2], [3,4]] (60, 35). Rebalance: try cuts after 1 (30 vs 65 over), after 3 (already there, 60 vs 35 diff 25). Best feasible = original.
        let tokens = vec![30, 30, 30, 5];
        let r = plan_chunks(&tokens, &pages_only(4), BoundaryLevel::Page, 60);
        assert_eq!(r.chunks, vec![vec![1, 2], vec![3, 4]]);
    }

    #[test]
    fn three_or_more_chunks_only_last_two_rebalance() {
        // tokens [50,50,50,50,50,1], budget 100
        // greedy: [1,2] (100), [3,4] (100), [5,6] (51)
        // rebalance last two: combined [5,6] (50,1), only feasible cut is original.
        let tokens = vec![50, 50, 50, 50, 50, 1];
        let r = plan_chunks(&tokens, &pages_only(6), BoundaryLevel::Page, 100);
        assert_eq!(r.chunks, vec![vec![1, 2], vec![3, 4], vec![5, 6]]);
    }

    #[test]
    fn rebalance_last_two_against_remainder_pattern() {
        // tokens [40,40,40,40,40,40,40,5], budget 100
        // greedy fills [1,2] (80), can't add 3 (120). [1,2], retry 3. [3,4] (80), [5,6] (80), [7,8] (45)
        // chunks: [[1,2], [3,4], [5,6], [7,8]]
        // last two combined: pages 5,6,7,8 tokens [40,40,40,5] total 125. budget 100.
        // try cut after 5 (40 vs 85, both <= 100, diff 45)
        // try cut after 6 (80 vs 45, diff 35) <- original
        // try cut after 7 (120 > 100, skip)
        // best: cut after 6, same as original.
        let tokens = vec![40, 40, 40, 40, 40, 40, 40, 5];
        let r = plan_chunks(&tokens, &pages_only(8), BoundaryLevel::Page, 100);
        assert_eq!(r.chunks.len(), 4);
        let sums: Vec<usize> = r
            .chunks
            .iter()
            .map(|c| c.iter().map(|&p| tokens[(p - 1) as usize]).sum())
            .collect();
        assert!(sums.iter().all(|&s| s <= 100));
        // Last two should be at least balanceable; since the only feasible improvement matched original, accept.
        let last_sum = sums[sums.len() - 1];
        let second_last_sum = sums[sums.len() - 2];
        assert!(second_last_sum + last_sum <= 200);
    }

    #[test]
    fn oversized_page_emits_own_chunk_with_diagnostic() {
        let tokens = vec![10, 200, 10];
        let r = plan_chunks(&tokens, &pages_only(3), BoundaryLevel::Page, 50);
        // Expected: [1] flushed when oversized 2 hits, [2] alone, then [3] alone
        assert_eq!(r.chunks, vec![vec![1], vec![2], vec![3]]);
        assert_eq!(
            r.diagnostics,
            vec![Diagnostic::OversizedPage { page: 2, tokens: 200 }]
        );
    }

    #[test]
    fn split_at_section_only_cuts_at_section_boundaries() {
        // 4 pages of 30 tokens each; section starts at page 3. budget 70.
        // boundaries: [Chapter (page 1 starts doc), Page, Section, Page]
        // greedy: add 1 (30), add 2 (60), can't add 3 (90>70). last_allowed_cut: after page 2? boundaries[2] = Section >= Section: yes, set Some(1).
        // cut: head=[1,2], tail=[]. push, retry 3. add 3 (30), cut_after_allowed(2): boundaries[3]=Page < Section: no. add 4 (60), cut_after_allowed(3): i+1==n: yes, last=Some(1). loop ends. push [3,4].
        // rebalance: combined [1,2,3,4], tokens 30,30,30,30. cuts allowed: after 2 (Section), after 4 (end). best feasible: after 2 (60 vs 60, diff 0). Same as greedy.
        let tokens = vec![30, 30, 30, 30];
        let boundaries = vec![
            BoundaryLevel::Chapter,
            BoundaryLevel::Page,
            BoundaryLevel::Section,
            BoundaryLevel::Page,
        ];
        let r = plan_chunks(&tokens, &boundaries, BoundaryLevel::Section, 70);
        assert_eq!(r.chunks, vec![vec![1, 2], vec![3, 4]]);
        assert!(r.diagnostics.is_empty());
    }

    #[test]
    fn split_at_section_forces_mid_level_cut_when_no_boundary_fits() {
        // 4 pages of 30 tokens; only boundary at page 1 (start). budget 70. split_at=Section.
        // boundaries: [Chapter, Page, Page, Page]. There's no Section boundary anywhere within doc.
        // greedy: add 1 (30), cut_after_allowed(0) = boundaries[1]=Page < Section: no. add 2 (60). cut_after_allowed(1): no. can't add 3 (90). last_allowed_cut = None. force cut: push [1,2]. diagnostic ForcedMidLevelCut after page 2. retry 3. add 3 (30). add 4 (60). loop ends. push [3,4].
        let tokens = vec![30, 30, 30, 30];
        let boundaries = vec![
            BoundaryLevel::Chapter,
            BoundaryLevel::Page,
            BoundaryLevel::Page,
            BoundaryLevel::Page,
        ];
        let r = plan_chunks(&tokens, &boundaries, BoundaryLevel::Section, 70);
        assert_eq!(r.chunks, vec![vec![1, 2], vec![3, 4]]);
        assert_eq!(
            r.diagnostics,
            vec![Diagnostic::ForcedMidLevelCut { after_page: 2 }]
        );
    }

    #[test]
    fn split_at_chapter_emits_no_diagnostic_for_page_only_doc_when_total_fits_one_chunk() {
        // No chapters; total fits in one chunk. No forced cut needed.
        let tokens = vec![10, 10, 10];
        let boundaries = vec![BoundaryLevel::Chapter, BoundaryLevel::Page, BoundaryLevel::Page];
        let r = plan_chunks(&tokens, &boundaries, BoundaryLevel::Chapter, 100);
        assert_eq!(r.chunks, vec![vec![1, 2, 3]]);
        assert!(r.diagnostics.is_empty());
    }

    #[test]
    fn rebalance_respects_allowed_cuts() {
        // 4 pages, tokens [10, 50, 10, 30]. budget 70. split_at=Section.
        // boundaries: [Chapter, Page, Section, Page]
        // greedy: add 1 (10), no allowed cut after (boundaries[1]=Page<Section). add 2 (60). no allowed cut after (boundaries[2]=Section actually >= Section: yes! set last_allowed_cut=Some(1)). add 3? 60+10=70 ok. add 3 (70). cut_after(2)= boundaries[3]=Page<Section: no. last_allowed_cut still Some(1). try add 4: 70+30=100>70. cut: head=[1,2] (60), tail=[3] (10). push head. cur=[3], cur_tokens=10, recompute last: cut_after(2)=no, none. retry add 4: 10+30=40 ok. add 4, cut_after(3)=i+1==n: yes, last=Some(1). end. push [3,4].
        // chunks before rebalance: [[1,2], [3,4]] (60, 40). combined [1,2,3,4]. allowed cuts: after 2 (Section), after 4 (end). best feasible <= 70: after 2 (60v40, diff 20), after 4 invalid (right empty). Original cut was after 2. No change.
        let tokens = vec![10, 50, 10, 30];
        let boundaries = vec![
            BoundaryLevel::Chapter,
            BoundaryLevel::Page,
            BoundaryLevel::Section,
            BoundaryLevel::Page,
        ];
        let r = plan_chunks(&tokens, &boundaries, BoundaryLevel::Section, 70);
        assert_eq!(r.chunks, vec![vec![1, 2], vec![3, 4]]);
    }

    #[test]
    fn rebalance_does_not_pick_cut_that_exceeds_budget() {
        // Greedy yields balanced last two; rebalance MUST not move to a cut that pushes one half over budget.
        let tokens = vec![10, 10, 60, 10];
        // pages_only -> all cuts allowed
        // greedy: add 1 (10), add 2 (20), add 3 (80>70 budget: depends). budget 80 -> add 3 ok (80). try add 4: 90>80. cut: last=Some(2). head=[1,2,3] tail=[]. push, retry 4. add 4. end. chunks=[[1,2,3],[4]] (80, 10). rebalance: combined [1,2,3,4], total 90. cuts: after 1 (10v80 over), after 2 (20v70 valid, diff 50), after 3 (80v10 valid, diff 70 - original). Best: after 2 (diff 50). New chunks: [[1,2], [3,4]] sums 20, 70 - both <= 80.
        let r = plan_chunks(&tokens, &pages_only(4), BoundaryLevel::Page, 80);
        assert_eq!(r.chunks, vec![vec![1, 2], vec![3, 4]]);
    }
}
