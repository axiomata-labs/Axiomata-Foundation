# Axiomata Foundation

The crates the Axiomata apps share, [Glut](https://github.com/axiomata-labs/glut) among them.

- **`axiomata-files`** — guarded file roots: every path is resolved inside its root and no link leads out of it;
  versioned reads, atomic writes, search, file watching and language servers. Glut's file tools stand on it.
- **`axiomata-sources`** — what an app may read on the Mac, behind a fence: calendars and reminders (EventKit),
  files (Spotlight), mail (Mail.app), contacts, shortcuts.
- **`axiomata-script`** — scripts in JavaScript or TypeScript that reach the outside only through a host's tools:
  QuickJS in the process, with limits on memory, calls, output and time.

- **`axiomata-plugin`** — plugins as WebAssembly components: fetched from a release, checked, translated once,
  run in wasmtime on a fresh instance per call with no rights but those the user granted. Hauch's plugins stand on
  it; [a template](https://github.com/axiomata-labs/axiomata-plugin-template) shows how to write one.
- **`axiomata-texts`** — the apps' texts in English and German, one TOML catalog per area, checked when built.
- **`axiomata-dock`** — a dock layout: a tree of splits and tab groups, every operation a view performs on it, where a
  dragged pane lands, and its stored form. No UI; the Axiomata studio draws it in a webview, Lohe on Schimmer.

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
