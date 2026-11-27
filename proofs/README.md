# Proofs about the editor's rope (trial)

The proof trial of Axiomata-OS `docs/plans/rust-core.md` R7 (Q6b): the same rope invariant with two tools, one
day's effort between them. Neither is part of the workspace build (`cargo test` needs neither tool); each
directory is a root of its own.

Both prove a **model** of `crates/axiomata-editor/src/rope.rs`, not the file itself: lines as bytes (`Vec<u8>`),
children as `Vec` where the rope shares with `Arc`. The functions follow the rope step for step:

| Claim | `rope-creusot` | `rope-aeneas` |
|---|---|---|
| `split_lines`: at least one line, none holds a break | ✔ | ✔ (and no push overflows, for a text shorter than `usize::MAX`) |
| a leaf caches the sum of its lines' bytes | ✔ | ✔ (no overflow) |
| a branch caches the sums of its children's counts | ✘ — Creusot refuses a type recursive through `Vec` | ✔ (no overflow) |
| a line `replace` joins holds no break | ✔ | ✔ |
| the cuts in that line stay on character boundaries | ✔ | ✔ |

## Creusot (`rope-creusot`)

Specifications are attributes in the Rust source (`#[requires]`, `#[ensures]`, `#[invariant]`); Why3 discharges them
with **Alt-Ergo and CVC5 only** (`why3find.json`; Z3 is Microsoft's and left out on purpose).

```sh
cd ~/Tools/creusot && ./INSTALL      # once; INSTALL.opts holds `--external z3`, PATH needs rustup's proxies first
cd proofs/rope-creusot && PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo creusot
```

Creusot commit `3409561`.

## Aeneas + Lean (`rope-aeneas`)

The Rust source carries no annotations. Charon turns it into LLBC, Aeneas into Lean (`lean/RopeAeneas.lean`,
generated, untouched), and `lean/Proofs.lean` proves the claims with Aeneas' tactics (`step`, `scalar_tac`).

```sh
cd proofs/rope-aeneas
PATH="/opt/homebrew/opt/rustup/bin:$PATH" ~/Tools/aeneas/charon/bin/charon cargo --preset=aeneas
~/Tools/aeneas/bin/aeneas -backend lean -loops-to-rec rope_aeneas.llbc -dest lean
cd lean && lake build                 # the first build fetches Mathlib's cache
```

Aeneas commit `b434e6e`, Charon `d8d7ce8db0`, Lean 4.36.0.
