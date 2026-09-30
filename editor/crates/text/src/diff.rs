//! Line diff for the git gutter and line staging.
//!
//! Myers' O(ND) algorithm after trimming the common prefix and suffix, which
//! is where almost all of a typical file is. Diffs that would cost more than
//! [`MAX_EDIT_DISTANCE`] steps collapse into one hunk instead of stalling.

use std::ops::Range;

/// A changed region: `old` rows in the base replaced by `new` rows in the
/// current text. Either side may be empty (pure insertion or deletion).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub old: Range<usize>,
    pub new: Range<usize>,
}

impl Hunk {
    pub fn is_insertion(&self) -> bool {
        self.old.is_empty()
    }

    pub fn is_deletion(&self) -> bool {
        self.new.is_empty()
    }
}

pub const MAX_EDIT_DISTANCE: usize = 4_000;

/// Splits text into lines the way the editor numbers rows: a trailing
/// newline leaves a final empty row.
pub fn lines(text: &str) -> Vec<&str> {
    text.split('\n').collect()
}

pub fn diff_lines(old: &[&str], new: &[&str]) -> Vec<Hunk> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let a = &old[prefix..old.len() - suffix];
    let b = &new[prefix..new.len() - suffix];
    if a.is_empty() && b.is_empty() {
        return Vec::new();
    }
    let script = match myers(a, b) {
        Some(script) => script,
        None => {
            return vec![Hunk {
                old: prefix..prefix + a.len(),
                new: prefix..prefix + b.len(),
            }];
        }
    };
    // Group runs of non-equal operations into hunks.
    let mut hunks = Vec::new();
    let (mut i, mut j) = (0, 0);
    let mut open: Option<(usize, usize)> = None;
    for op in script {
        match op {
            Op::Equal => {
                if let Some((oi, oj)) = open.take() {
                    hunks.push(Hunk {
                        old: prefix + oi..prefix + i,
                        new: prefix + oj..prefix + j,
                    });
                }
                i += 1;
                j += 1;
            }
            Op::Delete => {
                open.get_or_insert((i, j));
                i += 1;
            }
            Op::Insert => {
                open.get_or_insert((i, j));
                j += 1;
            }
        }
    }
    if let Some((oi, oj)) = open {
        hunks.push(Hunk {
            old: prefix + oi..prefix + i,
            new: prefix + oj..prefix + j,
        });
    }
    hunks
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Equal,
    Delete,
    Insert,
}

/// Shortest edit script, or `None` when it is longer than the budget.
fn myers(a: &[&str], b: &[&str]) -> Option<Vec<Op>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    let limit = max.min(MAX_EDIT_DISTANCE);
    let offset = max as isize;
    let mut v = vec![0isize; 2 * max + 2];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=limit as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let idx = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) {
                v[idx + 1]
            } else {
                v[idx - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, a.len(), b.len(), offset));
            }
            k += 2;
        }
    }
    None
}

fn backtrack(trace: &[Vec<isize>], n: usize, m: usize, offset: isize) -> Vec<Op> {
    let mut ops = Vec::new();
    let (mut x, mut y) = (n as isize, m as isize);
    for (d, v) in trace.iter().enumerate().rev() {
        let d = d as isize;
        let k = x - y;
        let idx = (k + offset) as usize;
        let prev_k = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops.push(Op::Equal);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            ops.push(if x == prev_x { Op::Insert } else { Op::Delete });
        }
        x = prev_x;
        y = prev_y;
    }
    ops.reverse();
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(old: &str, new: &str) -> Vec<(Range<usize>, Range<usize>)> {
        diff_lines(&lines(old), &lines(new))
            .into_iter()
            .map(|h| (h.old, h.new))
            .collect()
    }

    #[test]
    fn identical_text_has_no_hunks() {
        assert!(d("a\nb\n", "a\nb\n").is_empty());
    }

    #[test]
    fn insertion_deletion_and_modification() {
        assert_eq!(d("a\nc", "a\nb\nc"), vec![(1..1, 1..2)]);
        assert_eq!(d("a\nb\nc", "a\nc"), vec![(1..2, 1..1)]);
        assert_eq!(d("a\nb\nc", "a\nB\nc"), vec![(1..2, 1..2)]);
    }

    #[test]
    fn separate_changes_make_separate_hunks() {
        let old = "1\n2\n3\n4\n5\n6";
        let new = "1\nX\n3\n4\n5\n6\n7";
        assert_eq!(d(old, new), vec![(1..2, 1..2), (6..6, 6..7)]);
    }

    #[test]
    fn middle_edits_find_the_shortest_script() {
        // After trimming, "b c d" -> "c d e": one deletion and one insertion,
        // not a three-line replacement.
        assert_eq!(d("a\nb\nc\nd\nz", "a\nc\nd\ne\nz"), vec![(1..2, 1..1), (4..4, 3..4)]);
    }

    #[test]
    fn empty_sides() {
        assert_eq!(d("", "a\nb"), vec![(0..1, 0..2)]);
        assert_eq!(d("a\nb", ""), vec![(0..2, 0..1)]);
    }
}
