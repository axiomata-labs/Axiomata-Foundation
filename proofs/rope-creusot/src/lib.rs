//! The rope's invariant, proved with Creusot (Axiomata-OS `docs/plans/rust-core.md` R7, Q6b): a model of
//! `axiomata-editor`'s line handling over bytes, since Creusot gives `str` no byte-level slicing to reason about.
//! Lines are `Vec<u8>`; the functions follow `rope.rs` (`split_lines`, a leaf's byte count, the lines `replace`
//! builds) step for step. Proved with Alt-Ergo and CVC5 only.

use creusot_std::prelude::*;

/// A line holds no line break.
#[logic(open)]
pub fn no_break(s: Seq<u8>) -> bool {
    pearlite! { forall<i> 0 <= i && i < s.len() ==> s[i]@ != 10 && s[i]@ != 13 }
}

/// The byte at `i` starts a character (or `i` is the end): it is no UTF-8 continuation byte `10xx_xxxx`.
#[logic(open)]
pub fn boundary(s: Seq<u8>, i: Int) -> bool {
    pearlite! { i == s.len() || (0 <= i && i < s.len() && !(128 <= s[i]@ && s[i]@ < 192)) }
}

/// Bytes in the first `n` lines of `lines`.
#[logic(open)]
#[variant(n)]
pub fn bytes_upto(lines: Seq<Vec<u8>>, n: Int) -> Int {
    pearlite! { if n <= 0 { 0 } else { bytes_upto(lines, n - 1) + lines[n - 1]@.len() } }
}

/// The bytes of fewer lines are never more.
#[logic]
#[variant(m - n)]
#[requires(0 <= n && n <= m && m <= lines.len())]
#[ensures(bytes_upto(lines, n) <= bytes_upto(lines, m))]
pub fn bytes_grow(lines: Seq<Vec<u8>>, n: Int, m: Int) {
    if n < m {
        bytes_grow(lines, n, m - 1);
    }
}

/// `split_lines`: the text cut at LF, CRLF and a lone CR. Every line it gives holds no break, and there is at least
/// one.
#[ensures(result@.len() >= 1)]
#[ensures(forall<k> 0 <= k && k < result@.len() ==> no_break(result@[k]@))]
pub fn split_lines(text: &[u8]) -> Vec<Vec<u8>> {
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut line: Vec<u8> = Vec::new();
    let mut i = 0;
    #[invariant(i@ <= text@.len())]
    #[invariant(no_break(line@))]
    #[invariant(forall<k> 0 <= k && k < lines@.len() ==> no_break(lines@[k]@))]
    while i < text.len() {
        let b = text[i];
        if b == b'\n' {
            lines.push(line);
            line = Vec::new();
        } else if b == b'\r' {
            lines.push(line);
            line = Vec::new();
            if i + 1 < text.len() && text[i + 1] == b'\n' {
                i += 1;
            }
        } else {
            line.push(b);
        }
        i += 1;
    }
    lines.push(line);
    lines
}

/// A leaf's cached byte count: the sum of its lines' lengths.
#[requires(bytes_upto(lines@, lines@.len()) <= usize::MAX@)]
#[ensures(result@ == bytes_upto(lines@, lines@.len()))]
pub fn leaf_bytes(lines: &Vec<Vec<u8>>) -> usize {
    let mut sum: usize = 0;
    let mut i = 0;
    #[invariant(i@ <= lines@.len())]
    #[invariant(sum@ == bytes_upto(lines@, i@))]
    while i < lines.len() {
        proof_assert! { bytes_grow(lines@, i@ + 1, lines@.len()); bytes_upto(lines@, i@ + 1) <= usize::MAX@ };
        sum += lines[i].len();
        i += 1;
    }
    sum
}

/// `bytes` with `extra[from..to]` appended.
#[requires(from@ <= to@ && to@ <= extra@.len())]
#[ensures((^bytes)@ == bytes@.concat(extra@.subsequence(from@, to@)))]
fn append(bytes: &mut Vec<u8>, extra: &[u8], from: usize, to: usize) {
    let old = snapshot! { bytes@ };
    let mut i = from;
    #[invariant(from@ <= i@ && i@ <= to@)]
    #[invariant(bytes@ == old.concat(extra@.subsequence(from@, i@)))]
    while i < to {
        bytes.push(extra[i]);
        i += 1;
    }
}

/// One line of what `replace` puts in place of the lines it touched: `before`'s first `start` bytes, the part,
/// `after` from byte `end`. Holds no break when its pieces hold none. Where the part begins, the line starts a
/// character when the part does; where the part ends, when `after` did at `end` — so a cut made on a character
/// boundary stays one in the joined line.
#[requires(start@ <= before@.len() && end@ <= after@.len())]
#[requires(no_break(before@) && no_break(part@) && no_break(after@))]
#[ensures(no_break(result@))]
#[ensures(result@.len() == start@ + part@.len() + (after@.len() - end@))]
#[ensures(part@.len() > 0 && boundary(part@, 0) ==> boundary(result@, start@))]
#[ensures(boundary(after@, end@) ==> boundary(result@, start@ + part@.len()))]
pub fn joined_line(before: &[u8], start: usize, part: &[u8], after: &[u8], end: usize) -> Vec<u8> {
    let mut line = Vec::new();
    append(&mut line, before, 0, start);
    append(&mut line, part, 0, part.len());
    append(&mut line, after, end, after.len());
    proof_assert! { forall<k> 0 <= k && k < part@.len() ==> line@[start@ + k] == part@[k] };
    proof_assert! { forall<k> end@ <= k && k < after@.len() ==> line@[start@ + part@.len() + k - end@] == after@[k] };
    line
}
