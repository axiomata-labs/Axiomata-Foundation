# AGENTS.md

Guidance for coding agents in this repository (Claude Code reads it through `CLAUDE.md`).

**Axiomata-Foundation** holds the building blocks the apps of the Axiomata family share, so that each exists once:

- `crates/axiomata-files` — the guarded file service: every action walks from a root's directory fd with
  `openat(O_NOFOLLOW)` (`pinned.rs`), roots are strict or contained, versioned reads and writes, the language-server
  table and the tools' environment. Used by Axiomata-OS (the editor, the core, the CLI, the app) and by Glut
  (`../Axiomata-Glut`, its file tools and LSP errors); later by the Studio as an app of its own.
- `crates/axiomata-sources` — what an app may read on the Mac (Lirvel `docs/plans/companion.md` §8j): the `fence`
  (a fixed baseline no app lifts — only inside home, never `Library`, hidden files, Photos libraries, key files,
  password exports — plus the owner's closed folders; a path is judged as written and as it really is) and the
  sources behind it (`calendar`: EventKit, read only). It knows nothing of models. Used by Lirvel.
- Next to move in when the Studio leaves Axiomata: the terminal emulator (`axiomata-terminal`).

The apps build these crates from this folder (path dependencies): a change here takes effect in each app at its next
build — run their gates too when a public item changes. **Security code lives here once**: never copy a part of it into
an app.

```sh
cargo build && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```
