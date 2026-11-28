//! The difference between two sequences (Axiomata-OS `editor/diff/myers.ts`): Myers' O(ND) algorithm, for the lines
//! a new text changed (`text_edits`, the trail) and later the words inside a changed line and the diff view.
//!
//! * **Common ends are trimmed first**, so an edit in the middle of a long file costs as much as the edit, not the
//!   file.
//! * **Bounded.** Past [`DEFAULT_MAX_COST`] differences the rest is reported as one block removed and one inserted:
//!   still a correct diff, just not a minimal one — better than a frozen window on two unrelated files.

/// What a run of the edit script does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffKind {
    /// Consumes both sides: the elements are the same.
    Equal,
    /// Consumes only the old side.
    Delete,
    /// Consumes only the new side.
    Insert,
}

/// A run of the edit script: `count` elements of one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiffOp {
    /// What the run does.
    pub kind: DiffKind,
    /// How many elements it covers; never 0.
    pub count: usize,
}

/// The default bound: a thousand edits is far past anything worth diffing finely.
pub const DEFAULT_MAX_COST: usize = 1000;

/// Appends `count` of `kind`, merging with the run before it.
fn push(ops: &mut Vec<DiffOp>, kind: DiffKind, count: usize) {
    if count == 0 {
        return;
    }
    match ops.last_mut() {
        Some(last) if last.kind == kind => last.count += count,
        _ => ops.push(DiffOp { kind, count }),
    }
}

/// The edit script turning `a` into `b`, as runs in order, minimal up to [`DEFAULT_MAX_COST`] differences.
#[must_use]
pub fn diff<T: PartialEq>(a: &[T], b: &[T]) -> Vec<DiffOp> {
    diff_bounded(a, b, DEFAULT_MAX_COST)
}

/// [`diff`] with its own bound: past `max_cost` differences the middle becomes one removed and one inserted block.
#[must_use]
pub fn diff_bounded<T: PartialEq>(a: &[T], b: &[T], max_cost: usize) -> Vec<DiffOp> {
    let start = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let tail = a[start..]
        .iter()
        .rev()
        .zip(b[start..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let mut ops = Vec::new();
    push(&mut ops, DiffKind::Equal, start);
    for op in middle(
        &a[start..a.len() - tail],
        &b[start..b.len() - tail],
        max_cost,
    ) {
        push(&mut ops, op.kind, op.count);
    }
    push(&mut ops, DiffKind::Equal, tail);
    ops
}

/// Myers on what trimming left over; the fallback block when it costs too much.
#[allow(
    clippy::many_single_char_names,
    reason = "the names of Myers' paper: n, m, d, k, x, y"
)]
fn middle<T: PartialEq>(a: &[T], b: &[T], max_cost: usize) -> Vec<DiffOp> {
    let (n, m) = (a.len(), b.len());
    let block = || {
        let mut ops = Vec::new();
        push(&mut ops, DiffKind::Delete, n);
        push(&mut ops, DiffKind::Insert, m);
        ops
    };
    if n == 0 || m == 0 {
        return block();
    }
    let limit = (n + m).min(max_cost.max(1));
    // Diagonal k lives at index k + offset; k runs over -limit-1 ..= limit+1.
    let offset = limit + 1;
    let mut v = vec![0usize; 2 * limit + 3];
    // trace[d] holds the furthest x on each diagonal k ∈ [-d, d] after step d.
    let mut trace: Vec<Vec<usize>> = Vec::new();
    for d in 0..=limit {
        for step in 0..=d {
            let k = 2 * step + offset - d;
            let down = step == 0 || (step != d && v[k - 1] < v[k + 1]);
            let mut x = if down { v[k + 1] } else { v[k - 1] + 1 };
            // y = x - (k - offset); x - y never leaves [-d, d], so this never underflows.
            let mut y = (x + offset).saturating_sub(k);
            while x < n && y < m && a[x] == b[y] {
                x += 1;
                y += 1;
            }
            v[k] = x;
            if x >= n && y >= m {
                trace.push(v[offset - d..=offset + d].to_vec());
                return backtrack(&trace, n, m);
            }
        }
        trace.push(v[offset - d..=offset + d].to_vec());
    }
    block()
}

/// Walks the trace back from the end, collecting single steps, and returns them as runs.
#[allow(
    clippy::many_single_char_names,
    reason = "the names of Myers' paper: n, m, d, x, y"
)]
fn backtrack(trace: &[Vec<usize>], n: usize, m: usize) -> Vec<DiffOp> {
    let mut steps = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..trace.len()).rev() {
        let prev = &trace[d - 1];
        // `prev` covers diagonals -(d-1) ..= d-1; diagonal k sits at index k + d - 1. Diagonals are kept as
        // `k + d` (never negative) to stay in unsigned numbers.
        let at = |kd: usize| prev[kd - 1];
        let kd = x + d - y;
        let up = kd == 0 || (kd != 2 * d && at(kd - 1) < at(kd + 1));
        let prev_kd = if up { kd + 1 } else { kd - 1 };
        let prev_x = at(prev_kd);
        let prev_y = prev_x + d - prev_kd;
        while x > prev_x && y > prev_y {
            steps.push(DiffKind::Equal);
            x -= 1;
            y -= 1;
        }
        if x == prev_x {
            steps.push(DiffKind::Insert);
            y -= 1;
        } else {
            steps.push(DiffKind::Delete);
            x -= 1;
        }
    }
    while x > 0 && y > 0 {
        steps.push(DiffKind::Equal);
        x -= 1;
        y -= 1;
    }
    let mut ops = Vec::new();
    for kind in steps.into_iter().rev() {
        push(&mut ops, kind, 1);
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Applies an edit script to `a`, taking inserted elements from `b` — must give `b`.
    fn apply<T: PartialEq + Clone + std::fmt::Debug>(a: &[T], b: &[T], ops: &[DiffOp]) -> Vec<T> {
        let mut out = Vec::new();
        let (mut i, mut j) = (0, 0);
        for op in ops {
            match op.kind {
                DiffKind::Equal => {
                    for _ in 0..op.count {
                        assert_eq!(a[i], b[j]);
                        out.push(a[i].clone());
                        i += 1;
                        j += 1;
                    }
                }
                DiffKind::Delete => i += op.count,
                DiffKind::Insert => {
                    out.extend_from_slice(&b[j..j + op.count]);
                    j += op.count;
                }
            }
        }
        assert_eq!(i, a.len());
        out
    }

    fn cost(ops: &[DiffOp]) -> usize {
        ops.iter()
            .filter(|o| o.kind != DiffKind::Equal)
            .map(|o| o.count)
            .sum()
    }

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn op(kind: DiffKind, count: usize) -> DiffOp {
        DiffOp { kind, count }
    }

    #[test]
    fn finds_a_minimal_script_and_reports_it_as_merged_runs() {
        let (a, b) = (chars("ABCABBA"), chars("CBABAC"));
        let ops = diff(&a, &b);
        assert_eq!(apply(&a, &b, &ops), b);
        // The classic example: D = 5.
        assert_eq!(cost(&ops), 5);
        assert!(ops.windows(2).all(|w| w[0].kind != w[1].kind));
    }

    #[test]
    fn handles_empty_sides_and_identical_input() {
        assert_eq!(diff::<&str>(&[], &[]), []);
        assert_eq!(diff(&["a"], &[]), [op(DiffKind::Delete, 1)]);
        assert_eq!(diff(&[], &["a", "b"]), [op(DiffKind::Insert, 2)]);
        assert_eq!(diff(&["x", "y"], &["x", "y"]), [op(DiffKind::Equal, 2)]);
    }

    #[test]
    fn trims_common_ends_around_a_change_in_the_middle() {
        assert_eq!(
            diff(&["1", "2", "3", "4", "5"], &["1", "2", "X", "4", "5"]),
            [
                op(DiffKind::Equal, 2),
                op(DiffKind::Delete, 1),
                op(DiffKind::Insert, 1),
                op(DiffKind::Equal, 2),
            ]
        );
    }

    #[test]
    fn falls_back_to_one_block_when_the_difference_exceeds_the_bound() {
        let a: Vec<String> = (0..50).map(|i| format!("a{i}")).collect();
        let b: Vec<String> = (0..50).map(|i| format!("b{i}")).collect();
        let ops = diff_bounded(&a, &b, 10);
        assert_eq!(ops, [op(DiffKind::Delete, 50), op(DiffKind::Insert, 50)]);
        assert_eq!(apply(&a, &b, &ops), b);
    }

    /// The smallest number of deletions and insertions, by the textbook table (for small inputs).
    fn edit_distance(a: &[u8], b: &[u8]) -> usize {
        let mut row: Vec<usize> = (0..=b.len()).collect();
        for x in a {
            let mut diag = row[0];
            row[0] += 1;
            for (j, y) in b.iter().enumerate() {
                let next = if x == y {
                    diag
                } else {
                    row[j].min(row[j + 1]) + 1
                };
                diag = row[j + 1];
                row[j + 1] = next;
            }
        }
        row[b.len()]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Replaces the TS test's 200 seeded rounds: every script is valid and minimal.
        #[test]
        fn every_script_is_valid_and_minimal(
            a in proptest::collection::vec(0u8..4, 0..20),
            b in proptest::collection::vec(0u8..4, 0..20),
        ) {
            let ops = diff(&a, &b);
            prop_assert_eq!(apply(&a, &b, &ops), b.clone());
            prop_assert_eq!(cost(&ops), edit_distance(&a, &b));
        }
    }
}
