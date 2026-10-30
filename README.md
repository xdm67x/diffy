# Diffy

Diffy is a desktop app for reviewing the pull requests that are waiting on **you**.
It finds the open PRs where you (or your team) are requested as a reviewer, plus the open
PRs you wrote yourself, groups them by repository, and runs a two-reviewer + judge AI review
for the PR you select. Several PRs can be reviewed at the same time, each run is kept in that
PR's review history so you can run it again after new commits and compare, the PR's diff is
shown read-only and syntax-highlighted in the main panel's Diff tab, where you can also ask
Claude about any line of it, and you can post a review to GitHub as an approval, a request
for changes, or a comment.

The window has three panels — repositories, the selected repo's pull requests, and the main
panel for the selected PR — and the two left panels resize by dragging their seams. The gear
button in the repositories panel's top strip opens Settings, which switches between light and
dark themes and sets a template for the comment posted to GitHub. The interface is built with
[GPUI](https://gpui.rs/) and [GPUI Kit](https://gpui-kit.com/). The design is documented in
[DESIGN.md](DESIGN.md).

## Requirements

- macOS (Diffy targets the macOS desktop)
- [`gh`](https://cli.github.com/) authenticated: `gh auth status`
- [Claude Code](https://docs.claude.com/en/docs/claude-code) installed and authenticated:
  `claude auth status`
- A git repository is **not** required; Diffy only reads PR data through `gh`.

## Install

Download `diffy-v<version>-aarch64-apple-darwin.dmg` from the
[releases](https://github.com/xdm67x/diffy/releases), open it, and drag **Diffy** into
Applications. Each release also ships the bare binary as a `.tar.gz`.

The app is ad-hoc signed, not notarized, so the first launch is blocked by Gatekeeper:
right-click **Diffy** and choose **Open**, or run
`xattr -d com.apple.quarantine /Applications/Diffy.app`.

## Run from source

```sh
cargo run --release
```

### Try it without a GitHub account

```sh
cargo run --release --features mock
```

The `mock` feature answers every `gh` and `claude` call with fixtures from `src/mock.rs`:
three PRs across two repos, their diffs, a review that walks through the same stages as
a real one, and an answer for every "ask to agent" question. Posting a review is skipped and
only logged. Neither `gh` nor Claude Code has to be installed or authenticated.

The review system prompt is embedded in the binary at build time with `include_str!`, so a
built Diffy does not need `SYSTEM_PROMPT.md` on disk. Set `DIFFY_SYSTEM_PROMPT` to a file
path to override the embedded prompt at runtime.

To include team review requests, list the teams in `~/.diffy/settings.json`:

```json
{
  "teams": ["acme/backend", "acme/platform"]
}
```

The file is optional; without it Diffy falls back to the comma-separated `DIFFY_TEAMS`
variable. Settings are re-read on every refresh (`⌘R`).

By default Diffy asks GitHub for open PRs with `--review-requested=@me`, plus your own open
PRs with `--author=@me`. Each target is fetched with `gh search prs --limit 100`; results are
deduplicated by URL and sorted by repository and PR number. Drafts are left out of the list:
an open draft is not ready to review yet.

## The window

Diffy opens in a light, white window with an orange accent (a dark near-black theme with the
same orange accent is available in Settings). The window has three panels: repositories on
the left, the selected repository's pull requests in the middle, and the main panel for the
selected PR on the right. Both left panels resize by dragging the seam between them
(double-click a seam to restore its default width), and their widths are remembered between
launches. The window controls sit inside the repositories panel's top strip, which also
carries the Diffy wordmark and the Settings gear.

The repositories panel lists every repo with open PRs waiting on you, with a count per repo
under a **Repositories** header that also shows the total. Selecting a repo switches the
middle list to its PRs and selects the newest one.

The PR list starts with the filter field and a refresh button on the same top row, then the
PR rows, newest first. Each row wraps long titles to two lines and shows a status marker for
that PR's latest review (not reviewed, running, merge, changes requested, failed), the
author, how long ago the PR changed, and the number. A PR that changed after its newest
review is marked **out of date**, so you know to run the review again before posting it.
While reviews run, the list's footer shows how many are running.

The main panel has a compact header — the PR title with a **Yours** tag on the PRs you
wrote and an "Open on GitHub" link, a metadata line with the number, repo, author, age, and
comment count, and the actions: **Run review**, and once a review exists **Comment**,
**Request changes**, **Approve**. Below the header a tab bar switches the body between
**PR**, **Diff**, and **Review**, with the changed-file count and the review's status chip at
its right end. The **PR** tab fills the panel with the PR description rendered as markdown
(images load over the network; embedded videos render as cards that open in the browser). The
**Diff** tab shows the PR's patch read-only and syntax-highlighted — every changed file,
with its hunk headers, line numbers, and added and removed lines, in one virtualized list
that scrolls on its own and expands folded context on click. The **Review** tab shows the
verdict and findings in a centred column. `⌘2` jumps to the **Review** tab and back. The
patch is fetched the first time a PR is selected and kept per PR until the next refresh, so
returning to a PR costs nothing. A failed fetch says why and offers **Try again**.

Hovering a line shows a **+** inside the line row (like an inline review comment); clicking it
opens a dialog that names the line (`path:line`), shows the line's code, and takes your question. On **Ask**,
Diffy creates a task for that line and starts a headless Claude session that answers the
question **in read-only mode**: the session is granted only the `Read`, `Glob`, and `Grep`
tools — no `Bash` at all — so it cannot run `gh` or `git` and can never write to the PR, and
nothing it answers is posted anywhere. The task appears as a compact card under the line with
a spinner while it runs, then the answer (selectable markdown, capped in height), or the error
with **Retry**. Several asks can run at once, one per line; tasks are kept per PR until the
next refresh.

The filter matches the PR title, the author, and the number (`#482`) within the selected
repository, so a queue can be narrowed without leaving the repo. The `⌘F` hint inside the
field focuses it. Repositories are switched in the left panel, not through the filter.

Failures (a failed refresh, review, save, ask, or post) are written to the log under
`~/.diffy/logs`, and wherever a failure belongs to something on screen it is shown inline:
a failed review renders an error card in the **Review** tab, a failed ask keeps its card with
a **Retry**, and a failed diff offers **Try again**.

The **Settings** dialog (the gear button in the repositories panel's top strip) has two
preferences: the color theme (**Light** / **Dark**, applied immediately) and the **GitHub
comment template** — a template with placeholders (`{pr}`, `{url}`, `{verdict}`, `{summary}`,
`{intent}`, `{findings}`, `{dismissed}`) for the body of the review comment posted to GitHub;
leave it empty for the built-in format. Both are saved to `~/.diffy/settings.json`.

## Keys

| Key | Action |
| --- | --- |
| `⌘2` | Jump to the Review tab (and back to the PR tab) |
| `⌘F` | Focus the PR filter |
| `⌘R` | Refresh the PR list from GitHub |
| `Esc` | Leave the filter |

Selection, running a review, stepping through review history, and submitting a review are
done with the window controls: click a PR row, use the **Run review** button, the review
history arrows in the verdict card, the **PR** / **Diff** / **Review** tabs, and the
**Comment** / **Request changes** / **Approve** buttons.

Posting uses the buttons in the main panel's header: **Comment**, **Request changes**, and
**Approve**. Each one opens a dialog naming the PR with the review body in an editable text
field, and nothing is posted before you confirm. They are disabled while a post is in flight
and while no finished review is shown. On a PR you wrote, GitHub refuses an approval and a
change request of your own work, so only **Comment** is offered there.

With the mouse you can click a repository or a PR to select it, drag a seam between panels to
resize it, scroll the PR list or the main panel, hover a diff line and click its
**+**, switch between the **PR**, **Diff**, and **Review** tabs, use the
refresh button beside the filter, and open Settings from the repositories panel's top strip.

The filter only hides rows of the selected repository. Every fetched PR stays loaded, so
switching repositories or clearing the filter brings all of your pull requests back, and the
selection follows a PR by its url rather than its position.

Reviews run per PR and concurrently: run one PR, select another, run it, and both run as
independent instances. Starting a review also switches the main panel to the **Review** tab.
The PR list footer shows how many reviews are running, and each list
row tracks its own PR's stage. A PR is never reviewed twice at the same time; its **Run review**
button is disabled while it is running. A finished review of the PR you are reading resets
the Review tab to the result; a background PR finishing does not disturb what you are looking at.

Every run is a new attempt appended to that PR's history. The Review tab shows when the
shown attempt finished and `Review 2 of 3` with arrows to step through them, and the list
row always reflects the newest attempt, including a failed one. If the PR changed after the
shown review (`gh`'s `updatedAt` moved past the review's timestamp), the review says so and
**Run review again** is the primary action.

## How a review works

Diffy fetches the PR diff with `gh pr diff` and sends the title, description, and diff to
three separate headless Claude Opus sessions. Each review is its own set of processes, so
several PRs can be in flight at once. `SYSTEM_PROMPT.md` is passed to every session
as its system prompt (`claude --system-prompt`), so the review protocol lives in one
versioned file. Sessions are not persisted.

Each session has `Bash`, `Read`, `Glob`, and `Grep`. Bash is pre-approved only for `gh`,
`git`, `mktemp`, and removal of `/tmp/diffy-review-*` directories; other actions are
denied rather than prompting. The prompt tells the reviewers to reach for `gh` first —
`gh pr view`, `gh pr diff`, then a temporary clone read with `Read`/`Glob`/`Grep` — and
never to report Bash as unavailable when a command is refused.

Sessions run with `--output-format stream-json`, so every tool call is relayed to the UI.
While a review runs, its list row shows the running state and the Review tab shows the
stage, the elapsed time, and the latest command (`reviewer 1 · gh pr diff …`).

1. **Two independent reviewer agents run at the same time.** Diffy starts two headless
   Claude sessions with the same flags as the judge and passes `SYSTEM_PROMPT.md` with
   `--system-prompt`. Both get the same task prompt and PR context, but each has a fresh
   context. Each returns a JSON list of findings with file, line, severity, and evidence.
2. **Diffy merges the findings.** A finding reported by both reviewers (same file and
   sufficiently similar text) is marked **confirmed** and is relevant by rule. A finding
   reported by only one reviewer goes to the judge.
3. **The judge** sees the description, diff, confirmed findings, and single findings. It
   decides whether each single finding is relevant, then returns a merge verdict:
   - `merge` — the code is sound and implements the PR description.
   - `request_changes` — there is a confirmed or accepted blocking issue, or the diff
     does not do what the description promises.

The Review tab shows the verdict, when it was reviewed, the judge's intent assessment, the
combined summary, and every finding it stands behind ordered worst severity first. Each card
says whether both reviewers reported it or the judge accepted it. Findings the judge
dismissed are listed after those with the reason.

## Posting a review to GitHub

The **Comment**, **Request changes**, and **Approve** buttons in the review header act on the
finished review currently shown. Diffy posts it with
`gh pr review --approve | --request-changes | --comment`, using that report as the review
body: a short summary, the confirmed and judge-accepted findings worst first, and the
verdict at the end. Dismissed findings are left out. A finding whose fix is a one-line
replacement is also posted as an inline GitHub suggestion through `gh api`, as a best
effort — GitHub rejects a line that is not part of the diff, and the review body already
explains the fix in prose. The confirmation dialog shows the body in a text field, and
whatever it holds when you confirm is what gets posted. The body goes to `gh` through stdin
(`--body-file -`), so its length is not limited by the command line.

The initial body comes from the **GitHub comment template** in Settings when one is set —
the template's placeholders (`{pr}`, `{verdict}`, `{summary}`, `{findings}`, …) are filled
with the report — and from the built-in format otherwise. You can still edit the body in the
confirmation dialog before posting.

Nothing is posted before you confirm the dialog, and a failure (for example a revoked token)
is logged to `~/.diffy/logs`. Diffy does not record which reviews were posted, so posting the
same report twice creates two GitHub reviews.

## Saved reviews

Every finished review is appended to `~/.diffy/reviews/reviews.jsonl` as one JSON object per
line, and reloaded on the next start so past verdicts show up again in the PR list and the
Review tab. The file is append-only, and every line for a PR url is kept, oldest first. A
failed attempt is shown for the session but not saved.

```json
{"url":"https://github.com/acme/backend/pull/482","reviewed_at":1767225600,"report":{"verdict":"merge","summary":"…","intent":"…","confirmed":[],"accepted":[],"dismissed":[]}}
```

Useful one-liners against the log:

```sh
jq -s 'group_by(.url) | map(last) | sort_by(-.reviewed_at) | .[] | {url, verdict: .report.verdict}' \
  ~/.diffy/reviews/reviews.jsonl
```

Reviews are keyed by PR url only, so a PR that changes keeps showing its previous reviews,
marked out of date, until you run it again. The saved `reviewed_at` is what Diffy compares
with the PR's `updatedAt`. Set `DIFFY_HOME` to keep these files somewhere else.

Both files are plain `serde_json`, so nothing else is needed to read them.

## Logs

Diffy appends one line per event to `~/.diffy/logs/diffy-YYYY-MM-DD.log` (UTC day), one
file per day: startup and exit, refreshes and how many PRs they found, review start, stage,
result, save, and failure, ask sessions, posts, and every error the app reports. Files are
never pruned, so delete old ones when you no longer need them. Logging never writes PR diffs
or review bodies, and a log write that fails is dropped rather than interrupting the app.

```sh
ls ~/.diffy/logs
tail -f "$(ls -t ~/.diffy/logs/*.log | head -1)"
```

## Configuration

`~/.diffy/settings.json`:

| Field | Meaning |
| --- | --- |
| `teams` | Teams whose review requests count as yours, e.g. `["acme/backend"]` |
| `theme` | `"light"` (default) or `"dark"`; also settable in the app's Settings dialog |
| `comment_template` | Template for the review comment posted to GitHub; placeholders `{pr}` `{url}` `{verdict}` `{summary}` `{intent}` `{findings}` `{dismissed}`. Empty/absent uses the built-in format |
| `panels` | `{"repos": 200, "prs": 240}` — persisted widths of the two left panels |

Environment:

| Variable | Meaning |
| --- | --- |
| `DIFFY_TEAMS` | Comma-separated teams, used when `settings.json` has no teams |
| `DIFFY_SYSTEM_PROMPT` | Path to a system prompt file that overrides the embedded one |
| `DIFFY_HOME` | Directory for `settings.json`, `reviews/`, and `logs/` (default `~/.diffy`) |

Diffs larger than 120 000 characters are truncated in the initial prompt. The prompt tells
the reviewers that they are seeing a partial diff and that they can fetch the rest with
`gh pr diff` or inspect the checked-out branch.

## Privacy and tool safety

The selected PR's title, description, author, diff, and any repository files a reviewer
chooses to read are sent to Anthropic through the Claude Code CLI. A posted review sends the
report text to GitHub. An "ask to agent" question sends the PR metadata, the full diff, and
the selected line with your question.

Claude runs commands as your user. The allowlist and system prompt reduce accidental
writes (temporary clones only, no builds or tests), but this is **not a sandbox or a
security boundary**. PR content is untrusted and may contain prompt injection. Only review
PRs you trust, or run Diffy inside a container or VM with credentials scoped to read-only
access.

The "ask to agent" sessions are stricter than the reviewers: they are granted no `Bash` tool
at all (`--tools Read,Glob,Grep`), so they cannot execute any command and cannot touch the PR
with write access. Their answers are shown in Diffy only and are never posted.

## Project layout

```
src/
  main.rs                    bootstrap, window options, key bindings, HTTP client install
  app.rs                     GPUI view: the three panels (repositories, PR list, main panel with
                             the PR/Diff/Review tabs), resizable seams, settings dialog,
                             ask-to-agent, posting dialog
  state.rs                   repositories, selected repo, PR list, filter, selection, review history
  models.rs                  PRs, findings, verdicts, repo grouping, comment template rendering
  api.rs                     gh search / gh pr diff / gh pr review / inline suggestions
  review.rs                  reviewer council, finding matching, judge, system prompt loading
  claude.rs                  headless Claude invocation (reviewer and read-only), JSON extraction
  ask.rs                     read-only "ask to agent" sessions about one diff line
  rich_text.rs               markdown plugins for the PR description (videos as browser-opening cards)
  http.rs                    macOS NSURLSession HTTP client so remote media in PR bodies loads
  exec.rs                    spawns gh/claude with a PATH that also sees Homebrew/mise installs
  store.rs                   ~/.diffy settings and saved review log
  theme.rs                   light and dark palettes, installed from the saved theme preference
  log.rs                     ~/.diffy/logs, one file per UTC day
  time.rs                    RFC 3339 parsing and age formatting for gh timestamps
  mock.rs                    fixtures behind the `mock` feature
SYSTEM_PROMPT.md             system prompt for reviewers and judge, embedded at build time
```

## Development

```sh
cargo fmt
cargo clippy --all-targets
cargo test
```

The matching, JSON-parsing, review-log, settings, comment-template, PR-list state, ask-prompt,
and video-embed logic have unit tests. `cargo test` also runs the headless GPUI integration
tests, which render the real window and drive the keyboard paths for repo selection, the
filter (`⌘F`/`Esc`), the **PR**/**Diff**/**Review** tabs (`⌘2`), the per-line ask cards, the
resizable panels, and the settings dialog. The Claude calls are not exercised in tests
because they need credentials and cost money.

A `bacon` dev job (`bacon.toml`, press `d` in bacon) reruns `cargo run --features mock` on
every change of `src/`, which is the fastest way to click through a change.
