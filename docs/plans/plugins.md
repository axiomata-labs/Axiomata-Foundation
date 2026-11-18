# Plugins and scripts for the Axiomata apps

Plugins for Glut, Lirvel, Hauch and the later Axiomata Studio (GPUI, once it leaves Axiomata-OS), written by the owner
and by anyone who wants to. Scripts, the smaller sibling, chain what an app can already do. Measurements:
`../Lirvel/docs/research/plugin-runtimes.md` (2026-11-18).

## Decided (owner, 2026-11-18)

- **Scripts** (Glut's code mode, Lirvel `companion.md` §8x): JavaScript or TypeScript in QuickJS through `rquickjs`,
  in the process, in a new crate `axiomata-script` here. It knows no tools, only a host that answers each
  `tools.<name>(args)`. Lirvel first; Glut moves over a few days after its launch (2026-11-19) and stays untouched
  until then.
- **Plugins are WebAssembly components** with a WIT interface, run in `wasmtime`. Any language that builds a component
  works — the community writes in its own; we ship templates for Rust and for JS/TS (built with `componentize-qjs`,
  QuickJS inside the component: 1.4 MiB, 60 µs to a fresh instance). A bug in a plugin's JS engine stays inside the
  sandbox.
- **Authors deliver compiled components** (`.wasm`); the app never builds source. The app carries Cranelift and
  translates a plugin once at install into machine code (`.cwasm`, 66–510 ms), again after an app update that changes
  wasmtime.
- **Plugins may do everything, interface included** — within the rights the user granted.
- **Where from:** an app's website lists plugins (like pi.dev's packages), and anyone can keep a plugin in a Git
  repository and users install it from there; ours live in Git too.
- **Where first:** most use in Glut and Lirvel; Hauch is the first test, because a mistake hurts least there.
- **The shared host** lives here in the Foundation; each app describes its own world in WIT.

## Grilled (owner, 2026-11-18, Q1–Q8)

- **Q1 Release:** the repository holds the source; a tagged release (`v1.2.0`) carries the built `.wasm` and the
  manifest as assets (GitHub Releases first, other hosts later by a direct release URL). Installed by `owner/repo` or a
  URL; the app takes the newest release. Users never need a Rust or Node toolchain.
- **Q2 Website:** hosts no files — a curated index per app (repositories with description, picture, author), read by
  the app as "discover plugins". Listing by pull request to the index; installing straight from any repository works
  without it.
- **Q3 Rights:** a plugin starts with none; its manifest asks, the app shows them at install. Shared rights (network
  to named hosts only, files in named folders only, its own storage, interface) and per-app rights (Lirvel: read the
  calendar, add events — with the owner's yes as always; Glut: offer a tool, run commands through Glut's gate). Each
  revocable on a page "Plugins" in every app. A right not granted does not exist for the plugin: the import is never
  linked.
- **Q4 Trust:** the manifest carries a checksum of the `.wasm`; the app shows the repository and the publisher;
  "Gelistet" for the curated index, "Von Axiomata" for ours. Real signatures (Sigstore, bound to the GitHub account)
  once there are strangers.
- **Q5 Interface:** a plugin never draws itself; it describes a tree of building blocks (card, row, text, picture,
  list, button, switch, field, choice), each app draws it — the GPUI apps with Schimmer, Glut the same blocks in its
  TUI —, clicks come back as events. A free surface to draw on (shapes, pictures, transitions) is a second step.
- **Q6 Limits:** 64 MiB per plugin by default, up to 512 MiB if the manifest asks (shown); a time limit per call
  through wasmtime's epochs — a hanging plugin is ended, the app goes on; instances short-lived or in a small pool,
  since Wasm memory never shrinks. The cost of interrupting is still to be measured.
- **Q7 Names:** crate `axiomata-plugin`; shared WIT package `axiomata:plugin` (log, settings, storage, interface,
  network, files); one world per app (`hauch:plugin`, `lirvel:plugin`, `glut:plugin`); versioned like Zed — a plugin
  names the interface version it was built against, old versions keep running.
- **Q8 Hauch first:** a filter for recognised text (the user's own word replacements) with a settings page; then an
  export of the subtitles to a file (the files right).

## Grilled (owner, 2026-11-18, Q9–Q14)

- **Q9 Updates:** once a day the app asks GitHub for the newest releases of the installed plugins, only those, and
  shows "Update verfügbar" on "Plugins". The user's click installs it; more rights are asked again like at install.
  Per plugin "automatisch aktualisieren", only while no right is added. The previous version stays — "Zurück zur
  vorigen Version" is one click. An update that needs a newer interface than the app knows is not offered ("braucht
  eine neuere App").
- **Q10 Network in Lirvel:** an explicit exception to "nothing leaves the Mac but what a model must read": installing
  and "Nach Updates suchen" only on the owner's click, never daily on their own; a plugin's network right only to the
  named hosts, shown prominently at install. Lirvel's `AGENTS.md` rule is amended when it is built. Glut and Hauch
  check daily (Q9).
- **Q11 Tools for a model** (Lirvel, Glut): a plugin's tool description is short (≈ 500 characters) and shown at
  install; its results are data, like a web page; in Lirvel a plugin's tool never acts without the owner's yes, not
  even in a trusted area; a plugin sees the conversation only with its own right "Gespräch mitlesen".
- **Q12 Storage and keys:** each plugin its own folder (`~/.lirvel/plugins/<name>/` and the like) and nothing else;
  uninstalling deletes it after asking. An API key is typed into a key field on the plugin's page, kept in the
  Keychain under the app's entry with the plugin's name; a plugin reads only its own.
- **Q13 Templates:** a repository `axiomata-plugin-template` with Rust and TS/JS, each with a build and a GitHub
  Action that builds the release (`.wasm` and manifest) on a tag. Each of our plugins has its own repository and is
  installed like any stranger's.
- **Q14 Order:** finish Lirvel's Stufe 6 → `axiomata-script` into Lirvel (§8x) → `axiomata-plugin` with Hauch's filter
  (Q8) and the templates → Lirvel → Glut after its launch → the Studio once it is an app of its own. Website and
  index once the first strangers' plugins exist.

## Later

The free drawing surface (Q5), real signatures (Q4), measuring the cost of interrupting (Q6).
