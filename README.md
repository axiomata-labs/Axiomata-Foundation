# Axiomata Foundation

The crates the Axiomata apps share, [Glut](https://github.com/axiomata-labs/glut) among them.

- **`axiomata-files`** — guarded file roots: every path is resolved inside its root and no link leads out of it;
  versioned reads, atomic writes, search, file watching and language servers. Glut's file tools stand on it.
- **`axiomata-sources`** — what an app may read on the Mac, behind a fence: calendars and reminders (EventKit),
  files (Spotlight), mail (Mail.app), contacts, shortcuts.
- **`axiomata-script`** — scripts in JavaScript or TypeScript that reach the outside only through a host's tools:
  QuickJS in the process, with limits on memory, calls, output and time.

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
