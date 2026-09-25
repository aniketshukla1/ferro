# Ferro frontend spec

Owner: **Claude (frontend)**. Scope: `web/**` and `.github/workflows/web.yml`. Never edits backend paths. The interface is [API.md](API.md); coordination rules are in [README.md](README.md).

---

## 1. Mission

A review surface that feels instant on any repo size and makes a reviewer finish a PR faster than GitHub, px0, or an IDE:

- **Instant**: every local interaction paints within one frame of the data arriving; nothing blocks typing or scrolling.
- **Deep review**: split/unified diffs with syntax and word-level highlighting, viewed tracking, "since my last review", inline threads, suggestions, AI findings you accept or reject.
- **Safe on hostile input**: a malicious PR (file names, markdown, SVGs, comments) can never run script in ferro.
- **Keyboard-first**: every action is a command in the palette and has a shortcut that works in a browser tab.

### Principles

1. Vanilla ES modules, no framework, no build step to run. Optional dev tooling (type checking, Playwright) lives under `web/tests/` with its own `package.json`.
2. One transport: HTTP + SSE to `/api/v1`. No Tauri `invoke`; the desktop app loads the same UI from its in-process server.
3. Virtualize everything unbounded: code, diffs, tree, search results, palette lists, thread lists.
4. One audited HTML sink (§ 4). Everything else is built with DOM APIs.
5. Every async view has four states: loading (skeleton after 150 ms), empty, error (with retry), ready. Superseded requests are aborted.
6. Degrade by feature flag: if `meta.features` lacks a capability, hide or mock it; never assume.

---

## 2. What's wrong with the legacy UI (`web/app.js`, 1,534 lines)

The rewrite fixes all of these. "Verified" = reproduced on 2026-09-24; the rest are from reading the code.

| # | Problem | Where |
|---|---|---|
| L1 | Markdown HTML from the server goes straight into `innerHTML`; `<img onerror>` in a README runs (verified). | `app.js:123` |
| L2 | `esc()` doesn't escape quotes, but its output goes into attributes (`title="${esc(p)}"`, `data-p="${esc(…)}"`), so a crafted filename can break out of the attribute. | `app.js` `esc`, `renderSidebar`, `renderTabs`, git panel |
| L3 | Alt shortcuts compare `e.key`; on macOS Option+C produces `ç`, so Alt+C/A/U/R/M/Z don't fire. | `app.js` keydown handler |
| L4 | Ctrl/Cmd+W and Ctrl+Tab are bound, but browsers never deliver them to pages. | keydown handler |
| L5 | Palette renders whichever response lands last: a slow search can overwrite newer results. | `app.js:1168` `updatePalette` |
| L6 | Sidebar keeps only the first 6,000 files and renders ≤ 3,000 rows; expand state resets on every file open. | `app.js:208` `renderSidebar` |
| L7 | Find-in-file and outline download the whole file, which the server truncates at 512 KB, so matches past that point are missed. | `runFind`, `toggleOutline` |
| L8 | Word wrap only works up to 2,000 lines and drops syntax colors. | `renderWrapped` |
| L9 | `lineAt()` scans every cached window for every row on every paint. | `paint` |
| L10 | Diff: unified only, not virtualized, no syntax or word-level highlighting; the file path regex breaks on paths containing ` b/`. | `parseDiff`, `renderDiff` |
| L11 | Every API function has two transports (`invoke` vs `fetch`); the desktop path never received `window.__TAURI__`. | `app.js:17` and all `api*` functions |
| L12 | `linkify` runs a regex over HTML strings (can corrupt attributes) and scans the whole file list per match. | `linkify` |
| L13 | The file list is fetched once at boot; on a first run (no cache yet) the tree stays empty until a manual reindex. | `boot` |
| L14 | One file, global mutable state, no tests. | — |

---

## 3. Architecture

### 3.1 Files

As built (F0 + F1 on B1). Entries marked with a milestone are planned.

```
web/
  index.html            legacy shell (until the F1 flip)
  next.html             new shell during F0–F1 (meta CSP), becomes index.html at the flip
  assets/favicon.svg
  styles/
    tokens.css          type scale, spacing, radii, chrome sizes, z-index, motion (+ reduced motion)
    themes.css          shared derived tokens (color-mix) + one [data-theme] block per theme (UI + syntax)
    base.css            reset, focus rings, scrollbars, syntax classes (t-*), ::highlight rules, utilities
    components.css      buttons, inputs, kbd, chips, badges, tooltips, toasts, dialogs, menus, skeletons, banners
    shell.css           app grid, topbar, sidebar switcher, resizers, tabs, breadcrumbs, inspector, status bar
    tree.css  viewer.css (+ find bar, image view)  palette.css  panels.css  dialogs.css (+ settings)  home.css  markdown.css
    (F2+) diff.css  review.css  ai.css
  src/
    boot-theme.js       classic script: applies the saved theme before first paint (no flash)
    main.js             boot, panel + inspector registration, commands, deep links (?path=&line=)
    core/
      api.js            request(), v1 wrappers, ApiError, swappable transport (mock), Server-Timing, has(feature)
      sse.js            events client: EventSource, backoff reconnect, 3 s grace, dispatch to store/bus
      store.js          createStore: get/set/update/subscribe(slice), microtask-batched notifications
      bus.js            UI pub/sub for cross-feature events
      dom.js            h(), mount(), setTrustedHTML() (the only HTML sink), escapeHtml(), mark helpers, textRange() (UTF-16 → DOM Range)
      keys.js           keymap, e.code matching, platform labels (⌘⌥⇧ vs Ctrl/Alt/Shift), desktop-only bindings
      commands.js       command registry {id, title, keys, desktopKeys, when(), run()}
      virtual.js        VirtualList: fixed rows, keyed recycling, scaled scrolling (variable heights: F2)
      match.js          client fuzzy match/rank (commands, symbols, loaded tree nodes)
      util.js           debounce, rafThrottle, LRU, lazy(), whenIdle(), formatters, paths (joinPath), safe storage
      text.js           pure text helpers (search-hit trimming)
      (F4) stream.js    fetch-based SSE parser for POST streams (AI ask)
    ui/
      icons.js          inline SVG icon set (createElementNS), file-type icons
      overlay.js        tooltips, toasts, dialogs (focus trap, Esc, focus return)
    features/
      shell.js  tree.js  editor.js (tabs, history, view cache)  viewer.js (code + image, find decorations)
      markdown.js (preview/source document view)  find.js (find bar + in-browser engine)
      palette.js  panels.js (changes, search, outline)  home.js  status.js
      settings.js (schema-driven form, ui.* keys)  prefs.js (applies ui.* at boot)  themes.js  session.js
      chrome.js (help sheet, auth screen, inspector tabs)  connection.js (reconnect banner)
      compat.js         legacy fallbacks: /api/fuzzy, /api/search, /api/git-status (§ 3.3); deleted at the flip
      (F2) diff.js  git.js   (F3) review.js   (F4) ai.js   (F5) nav.js   (F6) vim.js
    mock/               in-browser mock server + event stream + markdown renderer (§ 3.4)
  tests/
    unit/*.test.js      node --test: HTML sinks, CSP rules, theme contrast, keys, fuzzy contract, highlighter,
                        find engine, markdown mock escaping, porcelain parser, path joins, hit trimming
    tools/              serve.py (no-cache dev server), gen-mock-files.mjs (mock snapshot), check-syntax.mjs
    (F1) e2e/           Playwright (dev-only package.json)
```

Loading: the boot graph is the static import closure of `main.js`: shell, tree, tabs, code viewer, home, status bar and core (23 modules, 136 KB uncompressed). `next.html` lists exactly these as `<link rel="modulepreload">`, so they download in parallel. The palette, sidebar panels, find, settings, help sheet, inspector tabs, markdown view and legacy fallbacks load with `import()` on first use (`lazy()` in `core/util.js`) and are warmed during idle time after the first screen (`whenIdle()`). Panels may render asynchronously (`showPanel` resolves once rendered); inspector tabs render only when the inspector is visible. `tests/unit/bootgraph.test.js` fails if an on-demand module leaks into the boot graph, if the preload list drifts, or if the graph exceeds the budget in § 9. From F2 on, diff, review, AI, navigation and vim load the same way.

### 3.2 State

One store with slices; features subscribe to the slices they render.

| Slice | Source | Notes |
|---|---|---|
| `meta` | `/meta`, `hello`, `workspace` | features, limits, host, mode |
| `index` | `index` events | ready state, file count, generation, search index state |
| `settings` | `/settings`, `settings` events | effective values; `ui.*` owned here |
| `session` | `/session` | tabs, active tab, open dirs, layout sizes, scroll positions; saved with a 1 s debounce |
| `git` | `/git/status`, `git` events | status, counts, branch |
| `pr` | `/pr`, `pr` events | PR metadata or null |
| `review` | drafts, viewed, rounds, threads, findings | per PR/workspace |
| `jobs` | `/jobs`, `job` events | running and recent jobs |
| `ui` | local | panels, focus, palette, dialogs, HUD |

On `workspace` or `resync` events the app resets every slice and refetches (no page reload). The session blob is re-read for the new workspace key.

### 3.3 API client rules

- `api(path, {method, query, body, signal})` returns JSON or throws `ApiError {status, code, message, detail}`.
- `401` → auth screen ("Open the link printed in your terminal"). Network failure or SSE down > 3 s → "ferro stopped, reconnecting…" banner with backoff (1, 2, 4, 8 s).
- Each view keeps an `AbortController` per logical request; a new query aborts the previous one; responses carry the request's sequence number and stale ones are dropped.
- The `Server-Timing` header (if present) is recorded for the status bar's latency readout.
- Feature gate: `has('search.stream')` etc. read from `meta.features`.
- Compat shims (temporary, `features/compat.js`, deleted at the flip § 11): without `fuzzy.v2` / `search.v2` the palette and search panel call the legacy `/api/fuzzy` and `/api/search`; without `git.status.v2` the legacy `/api/git-status` porcelain text is parsed into `GitStatus` and refreshed on `fs` events; without `file.find` the find bar searches `/file/raw` in the browser (one file cached, `maxRawBytes` cap).
- B1 deviations, fixed in the backend on `fix/b1-review` (2026-09-25); the frontend keeps tolerating older builds: the events stream is opened with `metrics=true` (accepted everywhere; `1` now works too), and a trailing line terminator inside per-line highlight HTML is still stripped defensively.

### 3.4 Mock mode

`?mock=1` swaps the `api.js` transport and the events stream onto `mock/`, an in-browser server that implements the API.md routes the frontend uses so far (meta, settings + schema, session, tree, file, lines, markdown, outline, find, fuzzy, search, paths/resolve, git/status, metrics, jobs, index/rebuild, events). `mock/markdown.js` renders a CommonMark subset into the backend's sanitized shape (heading ids, `data-line`, `data-path`, inert external images); a unit test feeds it hostile input.

- Files: this repository's real file list (snapshot in `mock/files.js`, regenerate with `node web/tests/tools/gen-mock-files.mjs`). Contents are fetched from the dev server, so the page must be served from the repo root. A 400k-line `generated/huge.log`, a git status fixture, and synthetic content for files that don't exist on disk are added.
- Variants: `?mock=big` adds 60,000 synthetic files (tree, palette, and search at scale); `?mock=auth` answers 401 (auth screen).
- Highlighting, fuzzy ranking, outline, and search are JS approximations of the backend contracts. `mock/fuzzy.js` follows the ranking contract in API.md § 6; `logic.test.js` checks it.
- Dev server: `python3 web/tests/tools/serve.py 4174` from the repo root, then open `http://127.0.0.1:4174/web/next.html?mock=1`. It sends `Cache-Control: no-store`, so edits show on reload.
- Real backend (B1): `cargo build --release -p ferro`, then `./target/release/ferro --no-open --port 7790 --token devtoken --dev-web web .`, open `http://127.0.0.1:7790/?token=devtoken` once (sets the cookie), then `http://127.0.0.1:7790/next.html`. `--dev-web` serves `web/` from disk with `no-store`.
- Still to add (F2–F3 as each lands): changes with renames/binary/CRLF/emoji lines, a PR with threads and drafts, AI findings.

---

## 4. Security rules (enforced by tests)

1. The only HTML sink is `setTrustedHTML(el, html, kind)` in `core/dom.js`, where `kind` ∈ `hl` (highlight spans from `/file/lines`, `/git/diff`, `/highlight`) or `markdown` (sanitized by the backend per API.md § 5.4). A unit test fails if `innerHTML`, `outerHTML`, `insertAdjacentHTML`, or `document.write` appears anywhere else in `web/src/**`.
2. All other text goes in through `textContent`, `setAttribute`, or `h()`. File names, commit messages, comment bodies, branch names, and AI output are untrusted.
3. `next.html` (later `index.html`) ships a meta CSP stricter than the server header: `style-src 'self'` (no inline styles). Dynamic styles are set through CSSOM (`el.style.transform = …`) only.
4. Links: only `http:`, `https:`, `mailto:`, and in-app `data-path` links. External links open with `rel="noopener noreferrer"`. In the desktop host they go through `/desktop/open-external`.
5. External images in markdown stay inert (`data-ext-src`) until the user clicks "Load external images" for that document (privacy: tracking pixels in untrusted READMEs).
6. The clipboard is written only in response to a user gesture. No `eval`, `new Function`, `javascript:` URLs, or inline event-handler attributes.
7. Streaming AI text is rendered by an escape-first mini renderer; the final answer is swapped for backend-rendered markdown (`/markdown/render`).
8. XSS canary e2e test: fixtures containing `<img onerror>`, `<svg onload>`, `javascript:` links, quote-breaking file names, and markdown/HTML in comments must never execute (the test page defines `window.__pwned` traps).

---

## 5. Design system

Direction (2026-09-25 redesign): calm, precise, near-monochrome, a Swiss-style tool UI. Hierarchy comes from type weight, spacing and surface elevation. The "accent" is the foreground itself (white on graphite, ink on porcelain). Hue appears only where it carries meaning: strings and numbers in code, git state, diffs, errors, find hits. The one exception is the logo (the ember forge square); otherwise no gradients, glows or decorative color.

### 5.1 Layout

```
┌──────────────────────────────────────────────────────────────────────────────────┐
│ ▣ ▯ │ repo  ⎇ branch      [ ⌕ Search files, symbols, text        ⌘K ]   ✦  ☀  ⚙  ▯ │ 44px
├───────────────────────┬──────────────────────────────────────────┬──────────────┤
│ [▤ Files] ⎇8  ⌕  ≡    │ tabs ─────────────────────────────────── │ AI · Info  × │ 38px (one line
│ repo          ◉ ⊟ ↻   │ crates › src › server.rs   Rust · 291 lines [Preview|Source] │    across all)
│ tree / changes /      │                                          │              │
│ search / outline      │ code · markdown · image · home           │              │
├───────────────────────┴──────────────────────────────────────────┴──────────────┤
│ ⎇ main · 8 changed · 187 files · 9 ms          Ln 42 of 291 · Rust · UTF-8 LF · 11 MB ● │ 26px
└──────────────────────────────────────────────────────────────────────────────────┘
```

- No activity rail. Panels switch from a row at the top of the sidebar (icon + label; the active one always shows its label, all show labels at ≥ 340 px via a container query; the Changes count rides on its button). The sidebar toggle sits next to the brand mark, so a collapsed sidebar is one click away.
- The sidebar switcher, tab strip and inspector header share one height (`--h-bar`, 38 px), so their bottom hairlines form one line across the window.
- Sidebar and inspector resize by drag (double-click resets) and are remembered in the session. Below 1100 px the inspector only shows when toggled; below 760 px both become overlays, the sidebar starts closed and closes after a file is picked.
- Density: 13 px UI text; code 13 px / 20 px (`ui.codeFontSize` 10–20, line height = size × 1.54).

### 5.2 Tokens

- Surfaces: `--bg-chrome` (topbar, sidebar, status), `--bg-0` (editor), `--bg-1` (raised: cards, inputs in chrome), `--bg-2` (keycaps, code chips), `--bg-3` (popovers: palette, dialogs, menus), `--bg-inset`.
- Text: `--fg`, `--fg-muted`, `--fg-faint`, `--code-fg` (code is a touch dimmer than UI text).
- Lines: `--border` (hairlines), `--border-strong` (controls). State: `--hover`, `--press`, `--active`, `--active-strong` (all derived from `--fg`), `--focus`, `--selection`/`--line-sel` (from a cool `--sel-tint`).
- Semantic: `--ok`, `--warn`, `--danger`, `--info` feed git (`--git-*`), diff (`--diff-*`, `--gutter-*`) and review (`--sev-*`, `--viewed`) tokens.
- Syntax: one variable per class in API.md § 11 (`--syn-keyword`, …).
- Contrast (unit-tested): `--fg`, `--fg-muted` ≥ 4.5:1 and `--fg-faint` ≥ 3:1 on `--bg-0`, `--bg-chrome`, `--bg-3`; `--code-fg` ≥ 4.5:1; accent and every syntax color ≥ 3:1 on the editor.

### 5.3 Themes

`auto` (default: follows `prefers-color-scheme` → graphite / porcelain), plus:

| Theme | Type | Character |
|---|---|---|
| `graphite` | dark (default) | zinc-neutral greys, `#141416` editor, white accent |
| `porcelain` | light | white editor, ink accent |
| `carbon` | dark | true black editor, highest contrast |

Syntax is deliberately restrained: keywords and punctuation are greys, function definitions are the brightest text (and semibold), types a cool light grey; only strings (mint / green) and numbers or constants (sand / amber) carry hue; comments are faint italics. One-click light/dark toggle in the top bar (`Alt+Shift+L`); the palette's theme mode and the settings gallery preview live. `ui.fileIconColors` opts back into language-tinted file icons. More themes (F6) must keep this token set and pass the contrast test.

### 5.4 Icons and motion

Inline SVG built by `ui/icons.js` with `createElementNS` (Lucide-style strokes, 24-unit viewBox, `currentColor`): no extra request, CSP-safe. File and folder icons are neutral (`--fg-faint`) and differ by glyph, not color. The brand mark is the forge square from `docs/BRAND.md` (ember gradient `#ff8c2e → #f2542d → #c22e3d`, white three-bar F, radius 22 %), drawn as SVG by `brandMark()` in the top bar, the home footer (with the tagline "Iron-clad code review."), the auth screen, the boot splash in `next.html` and the favicon. Motion 90–200 ms with a decelerating curve (exits faster than entrances), opacity and transform only, disabled under `prefers-reduced-motion`.

---

## 6. Feature specs

Each feature lists its API usage and acceptance checks. "F#" is the milestone that ships it.

### 6.1 Shell, auth, status (F0)

- Boot: parallel `meta`, `settings`, `session`, `/git/status` (if flagged), events stream; render shell immediately with skeletons; restore tabs from the session; honor `meta.initial` (`ferro file:line`) once.
- Auth screen on 401; disconnected banner on stream loss; "ferro was restarted" detection via `hello.generation`.
- Status bar (left → right): branch with ahead/behind, git counts, index state (files, ms, search-index badge), cursor position, selection size, language, encoding/EOL, RSS from `metrics`, last server timing, connection dot. Every item is a button that opens the relevant panel.
- Toasts (bottom-right, 4 s, pause on hover) and a Dialog component (focus trap, Esc, `aria-modal`).

### 6.2 File tree (F1)

- Lazy: `GET /tree?dir=` per expanded directory; flat virtualized list of visible nodes (100k+ entries without jank).
- Compact single-child folder chains (`src/main/java/com/x`), git badges, dirty dots on folders, ignored entries dimmed and expandable on demand, symlinks marked, file-type color dots.
- Expand all (max 4 requests in flight, skips ignored dirs, cancellable), collapse all, reveal active file, filter box (filters loaded nodes; Enter falls back to fuzzy).
- Keyboard: ↑↓, ← collapse / go to parent, → expand / first child, Enter open, Space preview, type-ahead.
- Updates from `fs` events (insert/remove entries in loaded dirs) and `git` events (badges); `overflow` → refetch open dirs.
- Open dirs persist in the session.

### 6.3 Tabs and history (F1)

- Preview tab: single-click opens in a reused italic preview tab; double-click, edit-like actions, or scrolling for 2 s pins it. Max 20 tabs (oldest unpinned closes first).
- Tab strip with overflow scrolling, middle-click close, drag reorder, dirty/changed dot, close button, context menu (close others, close to the right, copy path, reveal in tree).
- Back/forward history of (path, line) positions; reopen closed tab.
- Session restores tabs, active tab, per-tab scroll and cursor.
- Files changed on disk (`fs` event) reload in place, keeping scroll and cursor; deleted files show a banner.

### 6.4 Code viewer (F1)

- Data: `/file` meta, then `/file/lines?hl=1` in 500-line chunks (prefetch ±1 chunk, abort far requests, LRU of 12 chunks per tab). `exact: false` chunks refetch after the matching `hl` event.
- Rendering: `VirtualList` fixed-height fast path, recycled row pool, `transform: translateY`. Row = gutter (line number, git change bar, comment marker) + code (trusted `hl` HTML).
- Scaled scrolling above 15,000,000 px of content (so 5M-line files scroll correctly; browsers cap element height around 17–33M px).
- Decorations use the CSS Custom Highlight API (`CSS.highlights`) for search matches, find matches, occurrences, and intraline ranges, with a `<mark>` TreeWalker fallback when unsupported. Decorations never rebuild row markup.
- Selection: native text selection; the selection is saved as (line, col) before repaint and restored after. If a selection spans unmounted lines, the `copy` handler builds the text from cached or fetched lines. Cmd/Ctrl+A selects the whole file (fetched via `/file/raw` for copy). Gutter is `user-select: none`.
- Caret: click places it; arrows, Home/End, PageUp/PageDown, Cmd/Ctrl+Home/End move it; Shift extends the selection. The caret is an overlay element, not inline markup.
- Double-click a word: highlight all occurrences (visible rows via highlights; whole-file positions via `/file/find?word=1&case=sensitive` for the overview ruler).
- Overview ruler (right edge, canvas): git changes, find hits, occurrences, comments, AI findings; click to jump.
- Word wrap (`Alt+Z`): variable-height rows with measured heights for files ≤ 50k lines; above that, a toast explains it is off.
- Long lines: horizontal scroll; lines cut by the server show `… (N more)` and a "show full line" action (`/file/lines?count=1&maxCols=…`).
- Gutter actions: click a line number to select the line, Shift+click to extend, drag to select a range; hover shows `+` to comment (review mode) and a menu (copy ref, copy with context, ask AI, edit with agent).
- Breadcrumbs: path segments (click → reveal in tree) and the current symbol from the outline.
- Performance: a paint (≈ 60 rows) ≤ 4 ms; 60 fps while flinging a 400k-line file.

### 6.5 Find in file (F1)

`Cmd/Ctrl+F`: inline bar with case/word/regex toggles; server-side `/file/find` (no size cap); `n / total` counter; Enter / Shift+Enter; matches decorated in visible rows and marked on the overview ruler; Esc restores focus and clears.

### 6.6 Palette (F1, symbol modes F5)

One input, prefix modes:

| Prefix | Mode | Source |
|---|---|---|
| (none) | files, then blended content hits (≥ 3 chars) | `/fuzzy`, `/search` (maxFiles 8) |
| `>` | commands | command registry |
| `@` | symbols in the current file | `/file/outline` |
| `#` | workspace symbols | `/symbols` (F5) |
| `:` | go to line[:col] | local |
| `%` | text search | `/search/stream` → search panel |
| `?` | help: prefixes and shortcuts | local |

- Fuzzy results highlight `positions`; recents (from the session) rank first for an empty query and are sent as `boost`.
- A pasted PR/MR URL offers "Open pull request" (`/pr/open`).
- Up to 50 results, virtualized; ↑↓, Enter, Cmd/Ctrl+Enter opens to the side (split, F6), Esc closes and restores focus.
- Stale-response guard and abort per keystroke; fuzzy requests are not debounced (server ≤ 4 ms), content search debounces 60 ms.

### 6.7 Search panel (F1)

`Cmd/Ctrl+Shift+F`: query box with toggles (Aa, ab, .*), include/exclude fields, results streamed by file (`/search/stream`), grouped and virtualized, match counts, `def` hits badged, per-file "more" expander, "excluded `vendor/` (N files) — include" chip, engine and timing shown ("index · 12 ms"). Keyboard: F4/Shift+F4 next/previous result. Results survive panel toggling.

### 6.8 Outline (F1)

Sidebar panel from `/file/outline`, nested by `depth`, kind icons, filter box, follows the cursor (highlights the enclosing symbol), click to jump. Shows "regex" vs "tree-sitter" source subtly.

### 6.9 Markdown preview (F1)

- `.md` files open rendered by default (`ui.markdownPreview`), toggle with `Alt+M` or `[Preview | Source]`.
- Backend HTML via the trusted sink; `data-path` links open files at `data-line`; heading ids enable in-document anchors; code blocks get copy buttons; images open in a lightbox.
- External images: banner "N external images blocked · Load" (per document).
- Scroll sync between preview and source through `data-line`.

### 6.10 Image viewer (F1; image diffs in F2)

Port the legacy viewer (zoom 5–3200 %, fit, 1:1, pan, background toggle, pixelated). F2 adds image diffs from `/git/blob/raw`: side-by-side, swipe, and onion-skin.

### 6.11 Changes and git panel (F2)

- Changes view (workspace mode): staged / unstaged / untracked groups from `/git/status`; per-file stage toggle; discard (confirm dialog listing files); click opens the diff.
- Base selector: `HEAD` (default), `merge-base:origin/<default>` ("review my branch"), any ref typed in.
- Git panel: branch, ahead/behind, commit message box (multi-line, 72-char guide), amend toggle, ✦ AI message (F4), Commit (Cmd/Ctrl+Enter), Push, Pull (ff-only); recent commits from `/git/log`. Errors show git's stderr in a collapsible block.
- Live: `git` events refresh counts and badges without refetching the tree.

### 6.12 Diff view (F2)

- Opens from Changes, `Cmd/Ctrl+D` on a file, or the PR file list. Modes: split / unified (`ui.diffLayout`), whitespace toggle.
- Multi-file continuous view (like a GitHub PR) built on `VirtualList` with variable heights: sticky file headers, rows, hunk separators with "expand ↑20 / ↓20 / all" (`/git/blob/lines`), inline widgets (threads, drafts, composer, AI findings).
- Files load lazily as they approach the viewport (`/git/diff?path=…`); `tooLarge` shows "Load diff (N rows)"; binary shows metadata; images show the image diff.
- Row: old and new line numbers, sign, highlighted code, intraline ranges via highlights. Split mode keeps paired rows on one DOM row so both sides stay aligned without scroll syncing.
- File header: path (rename shown as `old → new`), status, +/−, viewed checkbox (PR mode), collapse, open file, copy path, comment count, finding count.
- Gutter markers in the code viewer come from `/git/gutter`; clicking a marker opens an inline mini-diff for that hunk.
- Keyboard: `Alt+↓/↑` next/previous change, `Alt+Shift+↓/↑` next/previous file, `V` toggle viewed (when the diff has focus), `C` comment on the focused line, `Cmd/Ctrl+D` back to source.
- Budget: first paint of a 50-file PR ≤ 150 ms after the changes list arrives; scrolling a 20k-row diff at 60 fps.

### 6.13 Review mode (F3)

- **PR bar** (top of main): `#123 Title`, author, `base ← head`, state pill (open/draft/merged/closed), checks state, files/+/−, viewed progress `12/40`, "Since last review ▾" (rounds), ✦ AI review, `Submit review ▾ (N drafts)`. Banner when `headMoved` ("New commits pushed · Refresh").
- **Open PR**: palette command or pasted URL → `/pr/open` job with a progress toast (metadata → fetch → worktree → merge-base) → workspace switch → review overview.
- **Review overview** (home in PR mode): PR description (rendered), file list with stats, threads summary, AI summary once available.
- **File list** (Changes panel in PR mode): tree or flat list, status, +/−, viewed checkbox, comment and finding counts; filters: unviewed, commented, has findings.
- **Threads**: existing threads inline at their line (outdated ones collapsed with a badge), replies (`/pr/threads/{id}/reply`), conversation tab in the inspector (PR body, issue comments, `/pr/conversation`).
- **Composer**: opens from the gutter `+`, `Alt+R`, or a selection (multi-line; side from the column the selection is in). Markdown textarea with preview (`/markdown/render` with `context`), "Suggest change" button inserting a ` ```suggestion ` block prefilled with the selected lines, `Cmd/Ctrl+Enter` saves a draft, Esc cancels (confirm if dirty). Drafts render inline with edit/delete; stale drafts are flagged.
- **Viewed**: checkbox per file; `changedSince` files show "changed since viewed" and reset to unviewed.
- **Since last review**: choosing a round switches the diff base to that round's head SHA (interdiff).
- **Submit dialog**: summary field, event (Comment / Approve / Request changes, default from settings), draft list with counts, stale warnings, disabled state with the reason (no token → shows `gh auth login` or `GITHUB_TOKEN` hint from `detail.hint`). Result toast links to the review on GitHub.
- **Multi-tab sync** through `drafts` events.

### 6.14 AI (F4)

- **Ask panel** (inspector "AI" tab): conversation list; input with context chips (current file, selection, current diff), editable before sending; streaming answer (escape-first renderer, then swapped for `/markdown/render` output); collapsible tool steps with durations; citations as chips (`/paths/resolve`) that open files at lines; usage footer (tokens, cache hits); stop button (aborts the stream).
- **AI review**: dialog (scope, focus areas) → `/ai/review` job → findings stream into the inspector list (grouped by file, severity chips, confidence) and as inline cards in the diff (collapsed to a severity badge in the gutter by default). Card actions: Accept (→ draft), Edit & accept, Dismiss (optional reason), Ask follow-up (opens ask with the finding as context). Bulk: accept all high. Filters: severity ≥, confidence ≥ (defaults: medium, 0.5), category. Findings for an older head show as outdated.
- **Commit message**: ✦ button in the git panel fills the message box (`/git/commit-message`).
- **Edit with agent** (needs `harness`): `Alt+E` on a selection → composer (instruction, harness and model pickers, first-use explicit choice) → job progress → diff of the changes since the snapshot with per-hunk keep/revert (`/git/hunk`) and revert all (`/harness/revert`). Overlapping ranges are refused client-side before calling the server.
- **Provider status**: if `ai.status.configured` is false, AI entry points show setup instructions instead of failing.

### 6.15 Navigation (F5)

- `F12` / Cmd/Ctrl+click: go to definition (`/nav/definition`); several targets → picker; external targets open read-only.
- `Shift+F12`: references panel in the inspector, grouped by file, virtualized, with previews.
- Hover card after 400 ms (`/nav/hover`): signature + doc, actions (definition, references).
- `#` palette mode for workspace symbols; `Alt+Shift+H` shows callers/callees if the backend adds call data later (not in API v1).

### 6.16 Settings (F1 basic, F6 complete)

`Cmd/Ctrl+,`: form generated from `/settings/schema` plus the frontend's own `ui.*` schema (theme, font size, line height, diff layout, markdown preview default, wrap, keymap mode, overview ruler, HUD); search box; user/workspace scope switch; per-key reset; raw JSON editor with validation; changes apply live and sync to other tabs through `settings` events.

### 6.17 Help and onboarding (F1, F6)

`?` palette mode and a shortcuts sheet with platform-correct labels; first-run tips on the home screen (open palette, review a PR, set up AI); home shows recents, workspace stats, and the PR summary in PR mode.

### 6.18 Vim mode (F6, optional)

Normal/visual modes over the caret and selection model (motions, search, marks, `gd` → definition). Off by default (`ui.keymap = "vim"`).

---

## 7. Keyboard map

`Mod` = Cmd on macOS, Ctrl elsewhere. Shortcuts match `e.code` (layout-independent) so Option-combos work on macOS. Browser-reserved keys are never bound in the browser host; the desktop host adds the native ones.

| Action | Browser | Desktop adds |
|---|---|---|
| Palette (files) | `Mod+K`, `Mod+P` | |
| Commands | `Mod+Shift+P` | |
| Symbols in file / workspace | `Mod+Shift+O` / `Mod+T` (desktop) or `#` in palette | `Mod+T` |
| Go to line | `Mod+G` | |
| Search panel | `Mod+Shift+F` | |
| Find in file | `Mod+F` | |
| Toggle sidebar / inspector | `Mod+B` / `Mod+J` | |
| Settings | `Mod+,` | |
| Toggle light / dark theme | `Alt+Shift+L` | |
| Find next / previous | `F3` / `Shift+F3` (Enter / Shift+Enter in the bar) | |
| Toggle diff for current file | `Mod+D` | |
| Close tab | `Alt+W` | `Mod+W` |
| Reopen closed tab | `Alt+Shift+T` | `Mod+Shift+T` |
| Next / previous tab | `Alt+]` / `Alt+[` | `Ctrl+Tab` / `Ctrl+Shift+Tab` |
| Tab 1–9 | `Alt+1…9` | |
| Back / forward | `Alt+←` / `Alt+→` | |
| Copy ref / with context | `Alt+C` / `Alt+A` | |
| Find usages of selection | `Alt+U` | |
| Comment (review) | `Alt+R` | |
| Edit with agent | `Alt+E` | |
| Markdown preview | `Alt+M` | |
| Word wrap | `Alt+Z` | |
| Definition / references | `F12` / `Shift+F12` | |
| Next / previous change, file | `Alt+↓/↑`, `Alt+Shift+↓/↑` | |
| Help | `?` (outside inputs) | |
| Dismiss (layered) | `Esc` | |

---

## 8. Accessibility (WCAG 2.1 AA)

- Tree: `role="tree"`/`treeitem`, `aria-expanded`, roving tabindex. Palette: combobox + listbox with `aria-activedescendant`. Tabs: `tablist`/`tab`. Dialogs: focus trap, `aria-modal`, focus return.
- Code and diff views are labelled regions; the caret line is announced through a polite live region; diff rows expose "added line 42" / "deleted line 17" labels, not color alone.
- Status letters and icons accompany every color signal (git status, severity).
- Visible focus rings everywhere; 200 % zoom keeps the layout usable; `prefers-reduced-motion` honored.
- axe-core runs in e2e on home, file, diff, review, settings, palette.

---

## 9. Performance budgets

| Interaction | Budget |
|---|---|
| Shell first paint after HTML response (localhost) | ≤ 50 ms |
| Boot graph JS (static closure of `main.js`, uncompressed, comments included) | ≤ 150 KB, enforced by `bootgraph.test.js` (2026-09-25: 136 KB, 23 modules); everything ≤ 350 KB |
| Palette keystroke → results painted (after response) | ≤ 16 ms |
| Code view paint (≈ 60 rows) | ≤ 4 ms; 60 fps fling on a 400k-line file |
| Tab switch to a cached file | ≤ 16 ms |
| Diff: 50-file PR first paint after changes list | ≤ 150 ms |
| Main-thread long tasks | none > 50 ms (PerformanceObserver in the HUD) |
| Tab memory after 20 files + a 5k-row diff | ≤ 150 MB |

The latency HUD (`ui.hud`, off by default; `Mod+Alt+P` toggles) shows frame times, long tasks, and per-request client and server times (`Server-Timing`). The status bar always shows the last search/fuzzy server time.

The boot budget was 80 KB until 2026-09-25. It assumed a minifier, but the no-build rule keeps comments and whitespace, and the boot graph must include the code viewer because sessions reopen files. Boot marks (`performance.mark`) are `ferro:boot`, `ferro:shell` and `ferro:ready`. Measured against B1 `--dev-web` (no-store, nothing cached): shell mounted at 102 ms and ready at 132 ms after navigation start, including the meta/settings/session round trips.

---

## 10. Testing

- **Unit** (`node --test web/tests/unit/*.test.js`; the bare directory form fails): virtual list math (including scaled scrolling), UTF-16 range helpers, escaping, keymap parsing, diff split pairing, highlight range merging, markdown mini-renderer, the "no stray HTML sinks" grep, and theme contrast checks.
- **E2E** (Playwright, Chromium + WebKit + Firefox, `web/tests/e2e`): boot + auth screen; tree with 20k files; open and fling a 400k-line file; palette modes and the stale-response race; streaming search + cancel; find-in-file past 512 KB; markdown XSS canary; diff split/unified with intraline; draft + submit (mock); AI findings accept/dismiss (mock); keyboard map on macOS and Linux key layouts; axe scans.
- Two targets: mock mode (every PR) and a real backend on a fixture repo (after B1).
- Syntax: `node web/tests/tools/check-syntax.mjs` parses every module as ESM from stdin (plain `node --check file.js` misses ESM syntax errors).
- Boot graph: `bootgraph.test.js` walks the static imports from `main.js` (on-demand modules excluded, preload list equal, size budget).
- CI: `.github/workflows/web.yml` (frontend-owned) runs the syntax check and the unit tests. Mock e2e joins it next.

---

## 11. Migration

1. F0–F1: build the new UI at `web/next.html` + `web/src/**` + `web/styles/**`. Legacy `index.html`, `app.js`, `style.css` stay untouched and keep working.
2. End of F1 (needs B1 + B2a): **flip** — legacy moves to `web/legacy.html` (+ `web/legacy/`), `next.html` becomes `index.html`. Legacy stays reachable at `/legacy.html` as a fallback.
3. End of F2: delete the legacy files and file an Open request in API.md for the backend to remove legacy routes (B8).

---

## 12. Milestones

| Milestone | Needs backend | Estimate | Ships |
|---|---|---|---|
| **F0 Foundation** | none (mock) | 2–3 days | modules, api/sse/store/dom/keys/commands/virtual, auth + disconnected screens, shell layout + resizers, status bar, 6 themes on the token system, mock mode, security test harness |
| **F1 Viewer & navigation** | B1, B2a | 4–5 days | tree, tabs + session, code viewer, find, palette, search panel, outline, markdown, image, settings (basic), help — then the flip |
| **F2 Changes & diff** | B3 | 4–5 days | changes view, git panel, diff view, gutter markers, image diffs, live updates |
| **F3 Review** | B4 | 4–5 days | PR bar, open PR flow, overview, file list + viewed, threads, composer + suggestions, since-last-review, submit |
| **F4 AI** | B5 (+ B7 for agent edits) | 3–4 days | ask panel, AI review UX, commit message, edit with agent |
| **F5 Navigation** | B6 | 2–3 days | definition, references, hover, workspace symbols |
| **F6 Polish** | — | ongoing | 16 themes, settings complete, onboarding, HUD, vim mode (optional), a11y fixes, legacy deletion |

Acceptance per milestone: its feature sections' checks pass, the e2e suite for it is green in mock mode (and against the real backend once available), performance budgets in § 9 hold for the features shipped, and no new HTML sinks.

---

## 13. Status (2026-09-25)

**Redesigned and running on the real B1 backend** (branch `fe/F1-redesign`). The graphite, porcelain and carbon themes replaced the six colorful F0 themes, and a sidebar switcher replaced the icon rail (§ 5). The forge-square logo (docs/BRAND.md) is the one colored element, and the tagline is now "Iron-clad code review." (px0.ai uses "Review code. Damn fast."). Features load on demand (§ 3.1): the boot graph went from 29 modules / 208 KB to 23 / 136 KB. 27 unit tests pass and all 45 modules parse. Checked against B1: tree, tabs, session restore, code viewer, palette, search, outline, Changes, markdown preview, settings, find, image view, events stream. Checked in mock mode: the same set plus the 60k-file and auth variants.

| Area | Built | Still missing |
|---|---|---|
| Tree | lazy dirs, compact chains, git badges, dirty dots (untracked folders too), keyboard nav, type-ahead, auto-reveal of the active file, collapse all, `fs` events | expand all, filter box, ignored-dir toggle |
| Tabs | preview/pinned tabs, 20-tab cap, middle-click close, history, reopen closed, session restore (tabs, scroll, cursor) | drag reorder, context menu |
| Viewer | 500-line chunks + LRU, scaled scrolling, sticky gutter, cursor/selection, gutter drag, copy lines, find bar (Mod+F, F3) with highlights | occurrences, overview ruler, wrap, long-line expander |
| Markdown | preview by default, Alt+M or [Preview/Source], scroll sync via `data-line`, in-repo links, anchors, blocked external images with opt-in, code copy buttons, live reload | image lightbox |
| Image | fit / 1:1 / zoom steps, Ctrl or ⌘ + wheel and pinch around the pointer, drag to pan, pixelated past 200 %, background toggle | image diffs (F2) |
| Palette | files / `>` / `@` / `:` / `%` / `?` / themes, blended content hits, stale guards + abort | `#` results (F5), PR URL detection, open to the side |
| Panels | search (Aa / ab / .*; hits trimmed to the match), outline (follows cursor), Changes (branch header, conflicts / staged / modified / untracked) | include/exclude fields, streaming search |
| Settings | schema-driven form (B1 keys + `ui.*`), sections, search, user/workspace scope, save on change, per-key reset, live theme / code size / icon tint | raw JSON editor (F6) |
| Other | light/dark toggle, shortcuts sheet (falls back to the command palette), narrow-screen overlays | — |
| Sign-in | paste-link-or-token screen; remembered browsers (30 days after each visit on 127.0.0.1/localhost, across restarts and ports; `meta.auth`); Settings → Security and palette commands sign out this browser or every browser (`POST /auth/logout[?all=1]`) | — |

Backend review of B0 + B1 (2026-09-25): 10 findings plus the two open requests above, all fixed on `fix/b1-review` with regression tests (114 Rust tests). Per-line highlight HTML and line text no longer carry `\n`/`\r`. `/events` accepts `metrics=1`. `maxCols` holds with highlighting on. Highlight windows read only the lookback plus the window, without holding the workspace lock. Markdown: externalImages counts real `<img>` only, `../` links resolve, `#anchor` links survive, attributes are escaped once, and entities unescape once. The token redirect keeps the requested page (never scheme-relative). Writes are refused into `.git`/`.hg`/`.svn` through symlinks and case variants.

Open items (frontend):

1. Budgets still to measure in CI: palette keystroke paint, code view paint, long tasks (§ 9), via the Playwright suite.
2. Flip `next.html` → `index.html` once B2a is merged, then delete `compat.js`.
3. Playwright e2e plus axe checks (§ 10).
4. Desktop side (not frontend-owned): the splash in `apps/desktop/splash/index.html` still uses the old `#1a1a1a` background and a text "F"; the app icons already show the forge square.

