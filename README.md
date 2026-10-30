# Diffy

Diffy is a desktop app for reviewing the pull requests that are waiting on **you**.
It finds the open PRs where you (or your team) are requested as a reviewer, plus the open
PRs you wrote yourself, groups them by repository, and runs a two-reviewer + judge AI review
for the PR you select. Several PRs can be reviewed at the same time, each run is kept in that
PR's review history so you can run it again after new commits and compare, you can read the
PR's diff beside the review, and you can post a review to GitHub as an approval, a request
for changes, or a comment.

The review list lives in an always-open sidebar beside the review. The interface is built with
[GPUI](https://gpui.rs/) and [GPUI Kit](https://gpui-kit.com/).

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
three PRs across two repos, their diffs, and a review that walks through the same stages as
a real one. Posting a review is skipped and only logged. Neither `gh` nor Claude Code has to be
installed or authenticated.

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

Diffy opens in a dark, near-black window that sits next to an editor. The pull requests
live in a full-height sidebar on the left and the selected PR on the right. The window
controls sit inside the sidebar, which stays open beside the review.

The sidebar starts with the filter field, then a **Pull requests** header with the visible
count and a refresh button, then the list grouped by repository, newest PR first. Each row
wraps long titles to two lines and shows the author, how long ago the PR changed, the
number, and the state of that PR's latest review. A PR that changed after its newest review
is marked **out of date**, so you know to run the review again before posting it. The footer
shows the total pull requests and repositories, or the reviews running right now.

The review pane has a fixed header with the PR title, author, age, comment count, a **Yours**
tag on the PRs you wrote, a link to GitHub, and the review actions. Under it, the **Review**
and **Diff** tabs switch the pane between two bodies.

**Review** leads with the review, because that is what you opened the PR for: the verdict,
the intent, the summary, the findings worst severity first, and the findings the judge
filtered out. The PR description follows below as context.

**Diff** is the PR's patch: every changed file, with its hunk headers, line numbers, and
added and removed lines, in one virtualized list that scrolls on its own and expands folded
context on click. The diff is fetched the first time you open the tab and kept per PR until
the next refresh, so returning to a PR costs nothing. A failed fetch says why and offers
**Try again**.

The status bar at the bottom of the review pane shows your place in the list, the shortcut
summary, and the last error or success message.

The filter matches the repository, the PR title, the author, and the number (`#482`), so a
queue can be narrowed without remembering which repository a PR belongs to. The `⌘F` hint
inside the field focuses it.

## Keys

| Key | Action |
| --- | --- |
| `⌘1` | Show the review |
| `⌘2` | Show the diff |
| `⌘F` | Focus the PR filter |
| `⌘R` | Refresh the PR list from GitHub |
| `Esc` | Leave the filter |

Selection, running a review, stepping through review history, switching tabs, and submitting
a review are done with the window controls: click a PR row, use the **Run review** button,
the review history arrows, the **Review** / **Diff** tabs, and the
**Comment** / **Request changes** / **Approve** buttons.

Posting uses the buttons in the review header: **Comment**, **Request changes**, and
**Approve**. Each one opens a dialog naming the PR with the review body in an editable text
field, and nothing is posted before you confirm. They are disabled while a post is in flight
and while no finished review is shown. On a PR you wrote, GitHub refuses an approval and a
change request of your own work, so only **Comment** is offered there.

With the mouse you can click a PR to select it, scroll the PR list or the review pane, switch
between the review and the diff, use the refresh button in the **Pull requests** header.

The filter only hides rows. Every fetched PR stays loaded, so clearing the filter brings
all of your pull requests back, and the selection follows a PR by its url rather than its
position. Failures (a failed refresh, review, save, or post) appear in the
status bar and stay there until the next action.

Reviews run per PR and concurrently: run one PR, select another, run it, and both run as
independent instances. The sidebar footer shows how many reviews are running, and each list
row tracks its own PR's stage. A PR is never reviewed twice at the same time; its **Run review**
button is disabled while it is running. A finished review of the PR you are reading resets
the pane to the result; a background PR finishing does not disturb what you are looking at.

Every run is a new attempt appended to that PR's history. The review section shows when the
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
While a review runs, its list row shows the running state and the review pane shows the
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

The result pane shows the verdict, when it was reviewed, the judge's intent assessment, the
combined summary, and every finding it stands behind ordered worst severity first. Each card
says whether both reviewers reported it or the judge accepted it. Findings the judge
dismissed are listed after those with the reason.

## Posting a review to GitHub

The **Comment**, **Request changes**, and **Approve** buttons in the review header act on the
finished review currently shown. Diffy posts it with
`gh pr review --approve | --request-changes | --comment`, using that report as the review
body: a short summary, the confirmed and judge-accepted findings worst first, and the
verdict at the end. Dismissed findings are left out. A finding whose fix is a one-line
replacement is also posted as an inline GitHub suggestion through `gh api`, when the line
is part of the diff. The confirmation dialog shows the body in a text field, and whatever
it holds when you confirm is what gets posted. The body goes to `gh` through stdin, so its
length is not limited by the command line.

Nothing is posted before you confirm the dialog, and a failure (for example a revoked token)
shows up in the status bar. Diffy does not record which reviews were posted, so posting the
same report twice creates two GitHub reviews.

## Saved reviews

Every finished review is appended to `~/.diffy/reviews/reviews.jsonl` as one JSON object per
line, and reloaded on the next start so past verdicts show up again in the PR list and the
review pane. The file is append-only, and every line for a PR url is kept, oldest first. A
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
result, save, and failure, posts, and every error shown in the status bar. Files are never
pruned, so delete old ones when you no longer need them. Logging never writes PR diffs or
review bodies, and a log write that fails is dropped rather than interrupting the app.

```sh
ls ~/.diffy/logs
tail -f "$(ls -t ~/.diffy/logs/*.log | head -1)"
```

## Configuration

`~/.diffy/settings.json`:

| Field | Meaning |
| --- | --- |
| `teams` | Teams whose review requests count as yours, e.g. `["acme/backend"]` |

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
report text to GitHub.

Claude runs commands as your user. The allowlist and system prompt reduce accidental
writes (temporary clones only, no builds or tests), but this is **not a sandbox or a
security boundary**. PR content is untrusted and may contain prompt injection. Only review
PRs you trust, or run Diffy inside a container or VM with credentials scoped to read-only
access.

## Project layout

```
src/
  main.rs                    bootstrap, window options, key bindings
  app.rs                     GPUI view: sidebar, review and diff panes, actions, posting dialog
  state.rs                   PR list, filter, selection, review history (no UI types)
  api.rs                     gh search / gh pr diff / gh pr review
  models.rs                  PRs, findings, verdicts, repo grouping
  review.rs                  reviewer council, finding matching, judge
  claude.rs                  headless Claude invocation, reviewer agent, JSON extraction
  store.rs                   ~/.diffy settings and saved review log
  log.rs                     ~/.diffy/logs, one file per UTC day
SYSTEM_PROMPT.md             system prompt for reviewers and judge, embedded at build time
```

## Development

```sh
cargo fmt
cargo clippy --all-targets
cargo test
```

The matching, JSON-parsing, review-log, and PR-list state logic have unit tests. `cargo
test` also runs the headless GPUI integration tests, which render the real window and drive
the keyboard paths for selection and the repository filter. The Claude
calls are not exercised in tests because they need credentials and cost money.
