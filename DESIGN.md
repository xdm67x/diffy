# Diffy's design

Diffy is a macOS desktop app for reviewing the pull requests that are waiting on you. This
document describes the product's design as it ships: the window shell, the color system, the
settings, and the panels' behavior. The interface is built with [GPUI](https://gpui.rs/) and
[GPUI Kit](https://gpui-kit.com/); the visual rules follow the GPUI Kit design guides and the
app-screen conventions they reference (a three-pane list workflow, one accent on the action
and the data, quiet chrome separated by tone steps rather than decoration).

## The window: three panels

The window is a three-pane shell: repositories on the left, the selected repository's PRs in
the middle, the selected PR's main panel on the right.

```
┌────────────────────┬──────────────────────────┬────────────────────────────────────┐
│ top strip:         │ filter (⌘F)          ⟳   │ top strip: drag region             │
│  ●●●    Diffy     ⚙├──────────────────────────┼────────────────────────────────────┤
│ Repositories 2     │ PR rows…                 │ header: title, Yours tag, link,    │
│  acme/api   2      │   (status marker per     │ meta, actions (Run review, posts)  │
│  acme/web   1      │    row)                  ├────────────────────────────────────┤
│                    │                          │ tabs: PR Diff Review · 3 files ●   │
│                    │ (N reviews running)      ├────────────────────────────────────┤
│                    │                          │ tab content, fills the panel:      │
│                    │                          │ PR description / diff / review     │
└────────────────────┴──────────────────────────┴────────────────────────────────────┘
   ▲ drag to resize        ▲ drag to resize
```

- **Left panel — repositories.** Every repo with open PRs waiting on you, with a count per
  repo under a **Repositories** header that also carries the total. Selecting one switches
  the middle list to its PRs and selects its newest PR. The top strip holds the traffic
  lights, the centred wordmark, and the Settings gear at its right end.
- **Middle panel — the PR list.** The selected repository's PRs, each with a status marker
  (not reviewed / running / merge / changes requested / failed). The filter (⌘F) sits beside
  the refresh button on the top row and narrows this list by title, author, and number. The
  selected row gets a soft orange fill; while reviews run, the bottom strip shows the
  running-review count.
- **Right panel — the main panel.** The selected PR: a compact header band (title with the
  `Yours` tag and the "Open on GitHub" link, a metadata line with number, repo, author, age,
  comments, and the actions — `Run review`, and once a review exists `Comment`,
  `Request changes`, `Approve`), then a tab bar, then the selected tab's content filling the
  rest of the panel.
- **Tabs.** The main panel's body is a tab bar (**PR**, **Diff**, **Review**) plus one tab's
  content — not overlays and not stacked sections. The **PR** tab renders the description as
  full markdown directly in the panel (images load over HTTP, videos render as cards that
  open in the browser). The **Diff** tab shows the read-only, syntax-highlighted patch and
  carries a per-line "+" ask affordance whose answers render as cards on the line. The
  **Review** tab shows the report in a centred 40 rem column. The tab bar's right end shows
  the changed-file count once the patch is loaded and the newest review's status chip (the
  verdict tag, or the running stage with a spinner). ⌘2 jumps to the **Review** tab and back
  to the **PR** tab.

The native title bar is transparent and the traffic lights float inside the repositories
panel's top strip, with the wordmark centred in it and the Settings gear at the right end.
The window opens centred at 1180×790 px and cannot shrink below 880×560 px; closing the last
window keeps Diffy in the dock, and reopening it (dock click) brings the window back.

## The panels resize

Both panels on the left are always shown. Each seam between panels is a resize handle
(`resize_handle` hugging the panel's trailing edge): dragging it sets that panel's width
from the pointer's x position, clamped to **160–320 px** for repositories (default
**200 px**) and **208–480 px** for the PR list (default **240 px**, inside the 210–292 px
range labelled list panes usually sit in). At rest a seam is a 1 px hairline; hovering or
dragging grows a small pill indicator on it. Double-clicking a seam restores its default
width. The widths are stored in `~/.diffy/settings.json` under `panels`
(`{"panels": {"repos": 200, "prs": 240}}`), restored clamped on launch, and written once
when a drag ends, not on every pointer move.

## Settings

The Settings gear in the repositories panel's top strip opens a dialog with two preferences,
both persisted in `~/.diffy/settings.json`:

- **Appearance — Light or Dark.** The color theme. Applied immediately on save and on
  launch (`{"theme": "light"}` / `{"theme": "dark"}`). Light is the default: white ground,
  orange accent. Dark is a near-black ground with the same orange accent brightened
  (orange-600 primary fill with a near-black label); both palettes are defined in
  `src/theme.rs` and every contrast pair was checked (text at or above 4.5:1, accent
  graphics at or above 3:1).
- **GitHub comment template.** The template for the review comment body posted to GitHub,
  stored as `comment_template`. Placeholders: `{pr}` (`acme/api#12`), `{url}`, `{verdict}`
  (`approve` / `request changes`), `{summary}`, `{intent}`, `{findings}` (markdown block,
  worst first), `{dismissed}` (count of filtered-out findings). Unknown placeholders are
  left as written. Empty or absent uses the built-in format (`ReviewReport::to_markdown`).
  The rendered template is what the post-confirmation dialog opens with, still editable
  before anything reaches GitHub.

## Color: light by default, white and orange

Diffy opens in a **light** theme (switchable to dark in Settings): one ground, one ink, one
accent. The accent is orange and
appears on the primary action, links, the focus ring, the caret, and the selected PR row —
nowhere else. Chrome surfaces (sidebar, top strips, detail header) sit one tone
off the white ground (`#FAFAFA`); cards and popovers are white; there are no gradients, glows,
or shadows on resting surfaces. All colors are defined in `src/theme.rs` as GPUI Kit theme
tokens, so every component reads them by semantic role.

| Role | Token | Value | Notes |
| --- | --- | --- | --- |
| Ground (canvas, popover, code) | `background`, `popover`, `colors.list` | `#FFFFFF` | |
| Ink | `foreground` | `#171717` | 17.9:1 on white |
| Chrome surfaces | `sidebar`, `title_bar` | `#FAFAFA` | one step off the ground |
| Raised/inset surfaces | `muted`, `secondary`, `group_box` | `#F5F5F5` | |
| Secondary text | `muted_foreground` | `#6B6B6B` | 5.3:1 on white |
| Hairlines | `border`, `sidebar_border` | `#E5E5E5` | the sidebar seam is the resize handle's own line |
| Control borders | `input` | `#D4D4D4` | one step darker than region hairlines |
| Hover fills | `list_hover`, `secondary_hover` | `#F0F0F0` / `#ECECEC` | |
| **Accent: primary action** | `primary` (+ hover/active) | `#C2410C` / `#9A3412` / `#7C2D12` | white label at 5.2:1 |
| Accent: focus ring, caret, progress | `ring`, `caret`, `progress_bar` | `#EA580C` | 3.6:1 on white, the non-text floor |
| Accent: links | `link` (+ hover/active) | `#C2410C` / `#9A3412` / `#7C2D12` | |
| Accent: selected PR row | `colors.list_active` (+ border) | `#FFEDD5` / `#FDBA74` | soft orange fill, orange edge |
| Text selection | `selection` | `#FED7AA` | |
| Success / danger / warning | `success`, `danger`, `warning` | `#15803D` / `#B42318` / `#B45309` | dark steps, ≥5:1 as text on white and as button fills with white labels |

Code in the diff tab is highlighted with tree-sitter grammars compiled in through the
`tree-sitter-languages` feature of gpui-kit, themed for the active mode (light or dark).
Radii are 7 px for controls and 11 px for large surfaces. The dark palette mirrors the
table above: ground `#050506`, ink `#EDEDED`, muted text `#A1A1AA`, hairlines `#26262C`,
primary `#EA580C` with a black label (5.9:1), ring/caret `#EA580C` (5.7:1 on the ground),
links `#FB923C`, selected row `#331D0E`/`#9A3412`, status colors `#3FC07A` / `#E5566F` /
`#FBBF24` (all ≥5.7:1 on the ground).

## Type, spacing, states

- The platform UI font everywhere except code and the mono location/progress lines. Base text
  is `text_sm` (14 px); the PR title is the display size (`text_xl` semibold), and the empty
  and failure states use `text_lg`.
- Spacing follows the shared semantic scale; the review tab keeps a centred 40 rem column so
  long reports keep a readable measure, while the PR description and the diff use the full
  width.
- States are visible without hover: the selected row keeps its fill, running reviews show a
  spinner with the stage and elapsed seconds, a review older than the latest push is marked
  "out of date" in the warning color, failures render inline (an error card in the review
  tab, **Try again** on a failed diff, **Retry** on a failed ask), and the empty main panel
  explains what will appear there. Errors and notices are logged to `~/.diffy/logs`, not
  shown as banners.

## Keyboard

| Shortcut | Action |
| --- | --- |
| `⌘F` | Focus the PR filter (Esc returns to the list) |
| `⌘R` | Reload pull requests |
| `⌘2` | Jump to the Review tab (and back to the PR tab) |

## Where it lives

- `src/theme.rs` — both palettes, installed from the saved `theme` preference.
- `src/app.rs` — the shell (`render`, `repo_panel`, `pr_panel`, `main_pane`, `main_top_bar`,
  `tabs_bar`), the resize handles and their drag handling, the settings dialog, and the UI
  integration tests that drive the real window (repo selection, filter, the PR/Diff/Review
  tabs, the per-line ask cards, drag both seams, check the clamps and the persisted settings,
  save from the settings dialog).
- `src/state.rs` — the list model: repositories, the selected repo, the per-repo PR rows,
  filter, and selection.
- `src/models.rs` — `render_comment_template`, the placeholder rendering for the GitHub
  comment.
- `src/rich_text.rs` — the markdown plugins that render a PR description's media: images
  through the app's HTTP client, `<video>` embeds as cards that open in the browser.
- `src/http.rs` — the macOS `NSURLSession` HTTP client, installed at startup.
- `src/exec.rs` — spawns `gh`/`claude` with a widened PATH (Homebrew, mise shims) for bundled
  launches.
- `src/time.rs` — RFC 3339 parsing and age formatting for the timestamps `gh` returns.
- `src/store.rs` — `settings.json` persistence (theme, comment template, panel widths) with
  `#[serde(default)]` fields so older settings files keep loading.

Visual changes need a screenshot or screen recording attached to the PR — run
`cargo run --release --features mock` to click through the app without GitHub or Claude
credentials.
