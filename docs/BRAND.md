# Ferro brand

**Ferro** — Latin for iron. Fast, dense, forged. Tagline: *Iron-clad code review.*

The tagline replaced "Review code. Damn fast." on 2026-09-25 because px0.ai uses that exact line. Avoid px0's phrasing ("Built for review. Engineered for speed.", "the IDE for humans and AI"); lean on iron, rigor and local-first instead of raw speed.

## Mark

The forge square: rounded square with an ember gradient and a white **F** cut from
three bars (vertical + top + mid). The F reads as both the initial and a forged beam.

- Gradient: `#ff8c2e` → `#f2542d` (55%) → `#c22e3d`
- Radius: ~22% of size. Clear space: height of the F mid-bar on all sides.
- Web: `.mark` CSS class (`web/style.css`) + SVG favicon in `web/index.html` (legacy UI); `brandMark()` in `web/src/ui/icons.js` + `.mark` in `web/styles/shell.css` + `web/assets/favicon.svg` (new UI, three-bar F as SVG).
- Desktop: `apps/desktop/src-tauri/icon.png` (source) → icns/ico via `tauri icon`.

## Color

The new app UI (`web/next.html`) is near-monochrome: graphite (dark, default), porcelain (light) and carbon (true black), specified in `docs/spec/FRONTEND.md` § 5. The ember gradient is reserved for the mark; everything else uses neutral greys plus meaning-only color (git state, diffs, errors). The table below is the legacy UI (`web/index.html`).

| Token | Forge (dark default) | Paper (light) | Mocha |
|---|---|---|---|
| bg0 / bg1 / bg2 | `#1a1a1a` `#232120` `#2a2a2a` | `#f6f4ee` `#efece3` `#e4dfd2` | `#11111b` `#181825` `#1e1e2e` |
| text / muted | `#c3c1ba` `#96928a` | `#2b2a26` `#6f6a5c` | `#cdd6f4` `#7f849c` |
| accent (ember) | `#d87757` | `#c2410c` | `#fab387` |
| good / bad / warn | `#4eba65` `#ff6b80` `#d9a45b` | `#3f7d2c` `#b3261e` `#8a6d1a` | `#a6e3a1` `#f38ba8` `#f9e2af` |

Forge dark matches Claude Code's dark look: `#1a1a1a` background, `#c3c1ba`
cream-gray text (VS Code "Claude Code" theme values), terracotta `#d87757`
accent (Claude's brand token `rgb(215,119,87)`), and its dark diff green/red
(`rgb(34,92,43)` / `rgb(122,41,54)` family via translucent mixes).

Rules: one accent (ember) for actions/selection; muted metadata stays AA
(4.5:1); red/green always paired with a text or icon badge, never color alone.

## Voice

Short, concrete, no superlatives. Shortcuts everywhere, text where it matters.
