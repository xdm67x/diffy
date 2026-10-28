# Diffy

Diffy is a terminal UI for reviewing the pull requests that are waiting on **you**.
It finds open PRs where you (or your team) are requested as a reviewer via the GitHub
CLI, groups them by repository, and runs a two-reviewer + judge AI review for the PR you
select.

```
┌ Diffy ──────────────────────────────────────────────────────────────┐
│ ▾ acme/backend  2                                                   │
│   #482 Add cursor pagination to /orders          ✔ merge            │
│   #477 Retry webhook delivery                    ● reviewing        │
│ ▾ acme/frontend 1                                                   │
│   #310 Fix empty state on invoices               ○ not reviewed     │
└─────────────────────────────────────────────────────────────────────┘
```

## Requirements

- [`gh`](https://cli.github.com/) authenticated: `gh auth status`
- [Claude Code](https://docs.claude.com/en/docs/claude-code) installed and authenticated:
  `claude auth status`
- A git repository is **not** required; Diffy only reads PR data through `gh`.

## Run

```sh
cargo run --release
```

Diffy looks for the review system prompt in this order:

1. The path in `DIFFY_SYSTEM_PROMPT`
2. `./SYSTEM_PROMPT.md`
3. `SYSTEM_PROMPT.md` next to the project at build time

To include team review requests, list the teams in `~/.diffy/settings.json`:

```json
{
  "teams": ["acme/backend", "acme/platform"]
}
```

The file is optional; without it Diffy falls back to the comma-separated `DIFFY_TEAMS`
variable. Settings are re-read on every refresh (`R`).

By default Diffy asks GitHub for open PRs with `--review-requested=@me`. Each target is
fetched with `gh search prs --limit 100`; results are deduplicated by URL and sorted by
repository and PR number.

## Keys

| Key | Action |
| --- | --- |
| `j` / `↓` | Next PR |
| `k` / `↑` | Previous PR |
| `g` / `Home` | First PR |
| `G` / `End` | Last PR |
| `Enter` / `r` | Run the review council on the selected PR |
| `J` / `PageDown` | Scroll the detail pane down |
| `K` / `PageUp` | Scroll the detail pane up |
| `R` | Refresh the PR list from GitHub |
| `q` / `Esc` | Quit |

Only one review runs at a time. The list and detail pane update live as the review moves
through its stages.

## How a review works

Diffy fetches the PR diff with `gh pr diff` and sends the title, description, and diff to
three separate headless Claude Opus sessions. `SYSTEM_PROMPT.md` is passed to every session
as its system prompt (`claude --system-prompt`), so the review protocol lives in one
versioned file. Sessions are not persisted.

Each session has `Bash`, `Read`, `Glob`, and `Grep`. Bash is pre-approved only for `gh`,
`git`, `mktemp`, and removal of `/tmp/diffy-review-*` directories; other actions are
denied rather than prompting. Reviewers can therefore verify the PR with `gh`, fetch the
full diff, or clone the PR branch into a temporary directory and inspect files outside
the diff.

1. **Two independent reviewer agents run at the same time.** Diffy starts two instances
   of the same custom Claude agent (`reviewer`, defined through `--agents` and launched
   with `--agent reviewer`). Both get the same system prompt, the same task prompt, and
   the same PR context, but each has a fresh context. Each returns a JSON list of
   findings with file, line, severity, and evidence.
2. **Diffy merges the findings.** A finding reported by both reviewers (same file and
   sufficiently similar text) is marked **confirmed** and is relevant by rule. A finding
   reported by only one reviewer goes to the judge.
3. **The judge** sees the description, diff, confirmed findings, and single findings. It
   decides whether each single finding is relevant, then returns a merge verdict:
   - `merge` — the code is sound and implements the PR description.
   - `request_changes` — there is a confirmed or accepted blocking issue, or the diff
     does not do what the description promises.

The result pane shows the verdict, the judge's intent assessment, the combined summary,
findings confirmed by both reviewers, single findings the judge accepted, and findings it
dismissed with a reason.

## Saved reviews

Every finished review is appended to `~/.diffy/reviews/reviews.jsonl` as one JSON object per
line, and reloaded on the next start so past verdicts show up again in the PR list and the
detail pane. The file is append-only; the newest line for a PR url wins. Failures are not
saved.

```json
{"url":"https://github.com/acme/backend/pull/482","reviewed_at":1767225600,"report":{"verdict":"merge","summary":"…","intent":"…","confirmed":[],"accepted":[],"dismissed":[]}}
```

Useful one-liners against the log:

```sh
jq -s 'group_by(.url) | map(last) | sort_by(-.reviewed_at) | .[] | {url, verdict: .report.verdict}' \
  ~/.diffy/reviews/reviews.jsonl
```

A saved review is keyed by PR url only, so a PR that receives new commits keeps showing its
previous review until you run it again. Set `DIFFY_HOME` to keep these files somewhere else.

Both files are plain `serde_json`, so nothing else is needed to read them.

## Configuration

`~/.diffy/settings.json`:

| Field | Meaning |
| --- | --- |
| `teams` | Teams whose review requests count as yours, e.g. `["acme/backend"]` |

Environment:

| Variable | Meaning |
| --- | --- |
| `DIFFY_TEAMS` | Comma-separated teams, used when `settings.json` has no teams |
| `DIFFY_SYSTEM_PROMPT` | Path to an alternative system prompt file |
| `DIFFY_HOME` | Directory for `settings.json` and `reviews/` (default `~/.diffy`) |

Diffs larger than 120 000 characters are truncated in the initial prompt. The prompt tells
the reviewers that they are seeing a partial diff and that they can fetch the rest with
`gh pr diff` or inspect the checked-out branch.

## Privacy and tool safety

The selected PR's title, description, author, diff, and any repository files a reviewer
chooses to read are sent to Anthropic through the Claude Code CLI.

Claude runs commands as your user. The allowlist and system prompt reduce accidental
writes (temporary clones only, no builds or tests), but this is **not a sandbox or a
security boundary**. PR content is untrusted and may contain prompt injection. Only review
PRs you trust, or run Diffy inside a container or VM with credentials scoped to read-only
access.

## Project layout

```
src/
  main.rs                    event loop, layout, key handling
  api.rs                     gh search / gh pr diff
  models.rs                  PRs, findings, verdicts, repo grouping
  state.rs                   UI state, selection, review status map
  review.rs                  reviewer council, finding matching, judge
  claude.rs                  headless Claude invocation, reviewer agent, JSON extraction
  store.rs                   ~/.diffy settings and saved review log
  widgets/
    pr_list_widget.rs        repo-grouped PR list
    detail_widget.rs         PR details and review report
SYSTEM_PROMPT.md             system prompt for reviewers and judge
```

## Development

```sh
cargo fmt
cargo clippy --all-targets
cargo test
```

The matching, JSON-parsing, and review-log logic has unit tests; the Claude calls are not
exercised in tests because they need credentials and cost money.
