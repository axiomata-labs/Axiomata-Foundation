//! The rope's invariant for Aeneas + Lean (Axiomata-OS `docs/plans/rust-core.md` R7, Q6b): the same model as
//! `../rope-creusot`, as plain Rust without annotations. Charon turns it into LLBC, Aeneas into Lean, and the proofs
//! live in `lean/` beside the generated code.

/// `split_lines`: the text cut at LF, CRLF and a lone CR.
pub fn split_lines(text: &[u8]) -> Vec<Vec<u8>> {
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut line: Vec<u8> = Vec::new();
    let mut i = 0;
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
pub fn leaf_bytes(lines: &Vec<Vec<u8>>) -> usize {
    let mut sum: usize = 0;
    let mut i = 0;
    while i < lines.len() {
        sum += lines[i].len();
        i += 1;
    }
    sum
}

/// `bytes` with `extra[from..to]` appended.
pub fn append(bytes: &mut Vec<u8>, extra: &[u8], from: usize, to: usize) {
    let mut i = from;
    while i < to {
        bytes.push(extra[i]);
        i += 1;
    }
}

/// One line of what `replace` puts in place of the lines it touched.
pub fn joined_line(before: &[u8], start: usize, part: &[u8], after: &[u8], end: usize) -> Vec<u8> {
    let mut line = Vec::new();
    append(&mut line, before, 0, start);
    append(&mut line, part, 0, part.len());
    append(&mut line, after, end, after.len());
    line
}

/// A node of the tree, as `rope.rs` has it (with `Vec` where the rope shares with `Arc`).
pub enum Node {
    /// Whole lines and their bytes.
    Leaf(Vec<Vec<u8>>, usize),
    /// Children, the lines and the bytes under them.
    Branch(Vec<Node>, usize, usize),
}

fn line_count(n: &Node) -> usize {
    match n {
        Node::Leaf(lines, _) => lines.len(),
        Node::Branch(_, lines, _) => *lines,
    }
}

fn bytes(n: &Node) -> usize {
    match n {
        Node::Leaf(_, bytes) | Node::Branch(_, _, bytes) => *bytes,
    }
}

/// `Node::branch`: a new branch caches the sums of what its children cache.
pub fn branch(children: Vec<Node>) -> Node {
    let mut lines: usize = 0;
    let mut sum: usize = 0;
    let mut i = 0;
    while i < children.len() {
        lines += line_count(&children[i]);
        sum += bytes(&children[i]);
        i += 1;
    }
    Node::Branch(children, lines, sum)
}
