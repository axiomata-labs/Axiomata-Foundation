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

## Built (2026-11-19, first step of Q14)

- **`crates/axiomata-plugin`**: the shared host. WIT package `axiomata:plugin@0.1.0` (`wit/plugin.wit`): `log`,
  `settings` (the app's language), `storage`, `ui` (the blocks of Q5) and the export `page` (`show`, `handle`); an app's
  world includes `base` and exports its hooks from `<package>/hooks`. `Plugins::fetch` → `Offer` (manifest checked,
  SHA-256, component translated, imports matched against the rights asked for, the page and every provided hook
  exported, linked) → `Plugins::install` after the yes; `check`, `check_due` (daily, updating by itself only without a
  new right), `roll_back` (current ↔ previous), `remove`, `set_enabled`, `set_right`, `set_auto_update`; `page`,
  `handle`, `call` (typed, any hook of the app). On disk `<id>/state.json`, `<id>/versions/<version>/` (manifest,
  component, machine code with its SHA-256 in the state, translated again when it changed or another wasmtime made it),
  `<id>/data/`.
- **Every call meets a fresh instance** (14 µs for a Rust plugin, 60 µs with QuickJS inside), 2 s and the manifest's
  memory (64 MiB unless it asks for up to 512): no pool, nothing kept in memory between calls (Q6). Epochs cost
  nothing measurable (`../Lirvel/docs/research/plugin-runtimes.md`).
- **Hauch** (Q8): `hauch:plugin@0.1.0` (`crates/hauch-app/wit/hauch.wit`) with the hook `filter-recognised`, called
  for the workshop's transcript, `hauch transcribe` and `hauch dictate`; the page "Plugins" (list; adding with the
  publisher, "Von Axiomata" and the rights; switched on, each right, updating by itself; looking for updates, back to
  the version before, removing after a second click; the plugin's own page under "Vom Plugin gezeigt"; its log
  lines); `hauch plugins [install|update|back|on|off|remove|filter|wit]`. Hauch looks for updates once a day while
  its window is open.
- **The first plugin and the template** (Q13): `axiomata-labs/hauch-wortfilter` (Rust, 103 KB: the owner's own
  replacements for words the recogniser gets wrong) and `axiomata-labs/axiomata-plugin-template` (Rust ~80 KB and
  JavaScript ~1.4 MB through `componentize-qjs --sync`, TypeScript compiled to JS first); a tag builds the release
  with `plugin.json` and `plugin.wasm`.
- **Decided while building:** a right the user takes back is not unlinked but answers every call of its interface
  with `not granted: …` — the plugin keeps running without it (Q3 says the import is never linked; a component that
  imports an interface whose right its manifest does not ask for is refused at fetch, which is what that guarded).
  A plugin's publisher and "Von Axiomata" come from where it was fetched, never from its manifest. Another plugin
  under an id already installed from elsewhere is refused. Network and file rights, keys (Q12), the index (Q2) and
  Lirvel's world come with the next steps.
- **After review and security audit (2026-11-19):** a plugin may not wait — WASI's `poll` and `block` (a sleep, a
  blocking read) trap, since epochs end a plugin only in its own code and a waiting host call would hold the hook
  past its limit; everything a plugin hands the host in one transfer is bounded at 16 MiB before it is copied
  (wasmtime's hostcall fuel — strings aliasing one buffer count each time); one budget for all memories and table
  elements of a call; a page at most 1000 blocks, 8 deep, 1 MiB of text, 16 pictures of at most 2048 pixels a side;
  a component at most 16 MiB (it is translated before the user is asked); the offer shows the hooks (what the plugin
  gets from the app) and the memory beside the rights, and an update that asks for more of any installs itself
  never; a found update is kept in the state ("Update verfügbar" after a restart), the version gone back from is held
  back; a release's assets must lie under `github.com/<owner>/<repo>/releases/download/`; an address names its host
  and first path segment as publisher; names and descriptions with direction overrides are refused; the plugin folder
  is 0700. Accepted: an address may lead to any HTTPS host the user typed; a hook's signature is checked at its first
  call, not at install.

## Later

The free drawing surface (Q5), real signatures (Q4), measuring the cost of interrupting (Q6).
