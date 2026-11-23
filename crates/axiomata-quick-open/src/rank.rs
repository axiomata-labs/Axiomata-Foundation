//! Matching a query against a path and ranking every candidate (the crate docs give the rules).

use std::cmp::Ordering;
use std::collections::HashMap;

/// A file quick open can offer: its root and its path relative to that root, `/`-separated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Candidate<'a> {
    /// The root's id.
    pub root: &'a str,
    /// The path relative to the root.
    pub rel: &'a str,
}

/// How well a query matched one path.
#[derive(Debug, Clone, PartialEq)]
pub struct PathMatch {
    /// Higher is better; only comparable between matches of the same query.
    pub score: f64,
    /// The matched letters, as indexes into the path's `char`s, ascending.
    pub positions: Vec<usize>,
}

/// One ranked file.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    /// The root's id.
    pub root: String,
    /// The path relative to the root.
    pub rel: String,
    /// The match's score with the recency boost and the length penalty in.
    pub score: f64,
    /// The matched letters, as indexes into `rel`'s `char`s, ascending ([`utf16_offsets`] for a web view).
    pub positions: Vec<usize>,
}

/// Points for every matched letter.
const LETTER: f64 = 1.0;
/// Extra points for a letter at a word start.
const WORD_START: f64 = 4.0;
/// Extra points for a letter right after the one matched before it.
const RUN: f64 = 3.0;
/// Points taken per letter skipped between two matched ones, and the most taken for one gap.
const GAP: f64 = 0.1;
const GAP_CAP: f64 = 3.0;
/// Extra points when the whole query matched inside the file name.
const IN_NAME: f64 = 10.0;
/// Extra points when the file name starts with the query.
const NAME_PREFIX: f64 = 8.0;
/// Points per place in the recently-opened list, counted from its end.
const RECENT: f64 = 0.5;
/// Points taken per UTF-16 unit of the path: the shorter path wins a tie.
const LENGTH: f64 = 0.01;

/// Matches one query against path after path, reusing its buffers — the query is prepared once.
#[derive(Debug, Clone, Default)]
pub struct Matcher {
    /// The query in lower case, without whitespace.
    query: Vec<char>,
    text: Vec<char>,
    lower: Vec<char>,
    positions: Vec<usize>,
}

impl Matcher {
    /// Prepares `query`: case is ignored, and so is whitespace typed into it.
    #[must_use]
    pub fn new(query: &str) -> Self {
        Self {
            query: query
                .chars()
                .filter(|c| !c.is_whitespace())
                .map(lower)
                .collect(),
            ..Self::default()
        }
    }

    /// How well the query matches `rel`; `None` if its letters are not all there in order. An empty query matches
    /// every path with score 0.
    pub fn find(&mut self, rel: &str) -> Option<PathMatch> {
        let score = self.score(rel)?;
        Some(PathMatch {
            score,
            positions: self.positions.clone(),
        })
    }

    /// The score of `rel`, leaving the matched letters in `self.positions`.
    fn score(&mut self, rel: &str) -> Option<f64> {
        self.text.clear();
        self.text.extend(rel.chars());
        self.positions.clear();
        if self.query.is_empty() {
            return Some(0.0);
        }
        // Every letter needs a place of its own.
        if self.query.len() > self.text.len() {
            return None;
        }
        self.lower.clear();
        self.lower.extend(self.text.iter().copied().map(lower));
        let name_start = self
            .text
            .iter()
            .rposition(|&c| c == '/')
            .map_or(0, |slash| slash + 1);
        if let Some(score) = self.match_from(name_start) {
            let prefix = if self.lower[name_start..].starts_with(&self.query) {
                NAME_PREFIX
            } else {
                0.0
            };
            return Some(score + IN_NAME + prefix);
        }
        self.match_from(0)
    }

    /// Matches the query from `from` on, greedily: for each letter, a word start or the place right after the letter
    /// before beats the first plain occurrence.
    fn match_from(&mut self, from: usize) -> Option<f64> {
        self.positions.clear();
        let mut score = 0.0;
        let mut at = from;
        for &letter in &self.query {
            let mut found = None;
            for i in at..self.lower.len() {
                if self.lower[i] != letter {
                    continue;
                }
                found.get_or_insert(i);
                let runs_on = self.positions.last().is_some_and(|&last| i == last + 1);
                if is_word_start(&self.text, i) || runs_on {
                    found = Some(i);
                    break;
                }
            }
            let found = found?;
            score += LETTER;
            if is_word_start(&self.text, found) {
                score += WORD_START;
            }
            if let Some(&previous) = self.positions.last() {
                if found == previous + 1 {
                    score += RUN;
                }
                score -= GAP_CAP.min(float(found - previous - 1) * GAP);
            }
            self.positions.push(found);
            at = found + 1;
        }
        Some(score)
    }
}

/// The best `limit` of `candidates` for `query`, best first; `recent` lists recently opened files, most recent first
/// (a file listed twice counts at its later place). An empty query lists the recent files first, then the rest.
pub fn rank<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = Candidate<'a>>,
    recent: &[Candidate<'_>],
    limit: usize,
) -> Vec<Ranked> {
    if limit == 0 {
        return Vec::new();
    }
    let mut boosts = HashMap::with_capacity(recent.len());
    for (i, file) in recent.iter().enumerate() {
        boosts.insert((file.root, file.rel), recent.len() - i);
    }
    let mut matcher = Matcher::new(query);
    let mut hits: Vec<Hit<'a>> = Vec::new();
    for candidate in candidates {
        let Some(score) = matcher.score(candidate.rel) else {
            continue;
        };
        let boost = boosts
            .get(&(candidate.root, candidate.rel))
            .copied()
            .unwrap_or(0);
        let length: usize = matcher.text.iter().map(|c| c.len_utf16()).sum();
        hits.push(Hit {
            candidate,
            score: score + float(boost) * RECENT - float(length) * LENGTH,
        });
    }
    if hits.len() > limit {
        hits.select_nth_unstable_by(limit - 1, Hit::order);
        hits.truncate(limit);
    }
    hits.sort_by(Hit::order);
    // Only the winners need their letters: matched once more, now keeping them.
    hits.into_iter()
        .map(|hit| {
            let positions = matcher
                .find(hit.candidate.rel)
                .map_or_else(Vec::new, |found| found.positions);
            Ranked {
                root: hit.candidate.root.to_owned(),
                rel: hit.candidate.rel.to_owned(),
                score: hit.score,
                positions,
            }
        })
        .collect()
}

/// `positions` (ascending `char` indexes into `text`) as UTF-16 offsets — how a web view indexes the same string.
#[must_use]
pub fn utf16_offsets(text: &str, positions: &[usize]) -> Vec<u32> {
    let mut out = Vec::with_capacity(positions.len());
    let mut wanted = positions.iter().copied().peekable();
    let mut offset = 0u32;
    for (i, c) in text.chars().enumerate() {
        while wanted.next_if_eq(&i).is_some() {
            out.push(offset);
        }
        if wanted.peek().is_none() {
            break;
        }
        offset += if c.len_utf16() == 2 { 2 } else { 1 };
    }
    out
}

/// A candidate that matched, before it is known to be among the best.
struct Hit<'a> {
    candidate: Candidate<'a>,
    score: f64,
}

impl Hit<'_> {
    /// Best score first; on a tie the path in alphabetical order (case first ignored, then not), then the root.
    fn order(a: &Self, b: &Self) -> Ordering {
        b.score
            .total_cmp(&a.score)
            .then_with(|| {
                let (x, y) = (a.candidate.rel, b.candidate.rel);
                x.chars()
                    .map(lower)
                    .cmp(y.chars().map(lower))
                    .then_with(|| x.cmp(y))
            })
            .then_with(|| a.candidate.root.cmp(b.candidate.root))
    }
}

/// A letter in lower case — itself when lower case would take more than one letter, so indexes stay aligned.
fn lower(c: char) -> char {
    let mut lowered = c.to_lowercase();
    match (lowered.next(), lowered.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

/// After `/ . _ -` or a space, at the start, or a capital right after a lower-case letter (camelCase).
fn is_word_start(text: &[char], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let before = text[i - 1];
    if matches!(before, '/' | '.' | '_' | '-' | ' ') {
        return true;
    }
    let changes_lower = |c: char| !c.to_lowercase().eq(std::iter::once(c));
    let changes_upper = |c: char| !c.to_uppercase().eq(std::iter::once(c));
    changes_lower(text[i]) && !changes_lower(before) && changes_upper(before)
}

/// A count as a score — exact far beyond any path length or list.
fn float(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(query: &str, rel: &str) -> Option<PathMatch> {
        Matcher::new(query).find(rel)
    }

    fn files<'a>(rels: &[&'a str]) -> Vec<Candidate<'a>> {
        rels.iter()
            .map(|rel| Candidate { root: "p", rel })
            .collect()
    }

    fn rels(ranked: &[Ranked]) -> Vec<&str> {
        ranked.iter().map(|r| r.rel.as_str()).collect()
    }

    #[test]
    fn finds_the_letters_in_order_not_necessarily_together() {
        assert!(find("mnrs", "src/main.rs").is_some());
        assert_eq!(find("srm", "src/main.rs").unwrap().positions, [0, 1, 4]);
        assert!(find("xyz", "src/main.rs").is_none());
    }

    #[test]
    fn ranks_a_match_in_the_file_name_above_one_across_folders() {
        let name = find("main", "src/main.rs").unwrap().score;
        let folders = find("main", "m/a/i/n.rs").unwrap().score;
        assert!(name > folders);
    }

    #[test]
    fn prefers_word_starts_and_runs() {
        assert!(
            find("fe", "FileEditor.svelte").unwrap().score > find("fe", "safely.ts").unwrap().score
        );
    }

    #[test]
    fn ignores_spaces_typed_into_the_query() {
        assert_eq!(find("m a i n", "src/main.rs"), find("main", "src/main.rs"));
    }

    #[test]
    fn matches_case_insensitively() {
        assert_eq!(find("MAIN", "src/main.rs"), find("main", "src/main.rs"));
    }

    #[test]
    fn finds_camel_case_word_starts_inside_the_file_name() {
        // Both `F` and `E` are word starts in `FileEditor.svelte`.
        assert_eq!(find("fe", "FileEditor.svelte").unwrap().positions, [0, 4]);
    }

    #[test]
    fn finds_nothing_for_a_query_longer_than_the_path() {
        assert!(find("verylongquery", "a.md").is_none());
    }

    #[test]
    fn gives_positions_across_folders_when_the_file_name_alone_does_not_match() {
        assert_eq!(
            find("sdm", "src/deep/main.rs").unwrap().positions,
            [0, 4, 9]
        );
    }

    #[test]
    fn an_empty_query_matches_everything_with_nothing_marked() {
        assert_eq!(
            find("", "src/main.rs"),
            Some(PathMatch {
                score: 0.0,
                positions: Vec::new()
            })
        );
    }

    #[test]
    fn counts_the_score_as_before() {
        // m(word start) a i n as a run in the name, which it starts: 4·1 + 4 + 3·3 + 10 + 8.
        assert!((find("main", "src/main.rs").unwrap().score - 35.0).abs() < 1e-9);
        // Not in the name; across folders: s at the start 1 + 4, r as a run 1 + 3, m at a word start after a gap of
        // two 1 + 4 - 0.2.
        assert!((find("srm", "src/main.rs").unwrap().score - 13.8).abs() < 1e-9);
    }

    #[test]
    fn puts_the_best_match_first_the_shorter_path_on_a_tie() {
        let ranked = rank(
            "main",
            files(&["lib/domain.rs", "src/main.rs", "src/deep/main.rs"]),
            &[],
            50,
        );
        assert_eq!(
            rels(&ranked),
            ["src/main.rs", "src/deep/main.rs", "lib/domain.rs"]
        );
    }

    #[test]
    fn brings_recently_opened_files_forward_and_first_for_an_empty_query() {
        let candidates = files(&["a/x.md", "b/x.md"]);
        let recent = [Candidate {
            root: "p",
            rel: "b/x.md",
        }];
        assert_eq!(rank("x", candidates.clone(), &recent, 50)[0].rel, "b/x.md");
        assert_eq!(rank("", candidates, &recent, 50)[0].rel, "b/x.md");
    }

    #[test]
    fn keeps_to_the_limit() {
        assert_eq!(rank("", files(&["a", "b", "c"]), &[], 2).len(), 2);
        assert_eq!(rank("", files(&["a"]), &[], 0), Vec::<Ranked>::new());
    }

    #[test]
    fn the_limit_keeps_the_best_not_the_first() {
        let ranked = rank(
            "zz",
            files(&["zaz.md", "b/zz.md", "zz.md", "z/z.md"]),
            &[],
            2,
        );
        assert_eq!(rels(&ranked), ["zz.md", "b/zz.md"]);
    }

    #[test]
    fn lists_the_same_relative_path_from_two_roots_separately() {
        let candidates = [
            Candidate {
                root: "b",
                rel: "src/main.rs",
            },
            Candidate {
                root: "a",
                rel: "src/main.rs",
            },
        ];
        let ranked = rank("main", candidates, &[], 50);
        let roots: Vec<&str> = ranked.iter().map(|r| r.root.as_str()).collect();
        assert_eq!(roots, ["a", "b"]);
    }

    #[test]
    fn never_lets_a_recent_file_boost_put_a_clearly_worse_match_first() {
        let candidates = [
            Candidate {
                root: "a",
                rel: "y.md",
            },
            Candidate {
                root: "b",
                rel: "xyz.md",
            },
        ];
        // "xyz.md" is the recently opened file, but "y.md" is still the far better match for "y".
        let ranked = rank(
            "y",
            candidates,
            &[Candidate {
                root: "b",
                rel: "xyz.md",
            }],
            50,
        );
        assert_eq!(ranked[0].rel, "y.md");
    }

    #[test]
    fn a_file_listed_twice_as_recent_counts_at_its_later_place() {
        let recent = [
            Candidate {
                root: "p",
                rel: "a.md",
            },
            Candidate {
                root: "p",
                rel: "b.md",
            },
            Candidate {
                root: "p",
                rel: "a.md",
            },
        ];
        // a.md's later place (boost 1) loses to b.md's (boost 2).
        assert_eq!(
            rank("", files(&["a.md", "b.md"]), &recent, 50)[0].rel,
            "b.md"
        );
    }

    #[test]
    fn ties_go_alphabetically_case_first_ignored() {
        assert_eq!(
            rels(&rank("", files(&["b.md", "B.md", "a.md"]), &[], 50)),
            ["a.md", "B.md", "b.md"]
        );
    }

    #[test]
    fn hands_the_winners_their_marked_letters() {
        let ranked = rank("fe", files(&["FileEditor.svelte"]), &[], 50);
        assert_eq!(ranked[0].positions, [0, 4]);
    }

    #[test]
    fn utf16_offsets_count_letters_outside_the_basic_plane_twice() {
        // `😀` is two UTF-16 units: the `a` after it sits at 3, not 2.
        assert_eq!(utf16_offsets("x😀a/b", &[0, 2, 4]), [0, 3, 5]);
        assert_eq!(utf16_offsets("abc", &[]), Vec::<u32>::new());
    }

    #[test]
    fn umlauts_match_in_either_case() {
        assert_eq!(find("ÄR", "ärger.md").unwrap().positions, [0, 1]);
    }
}
