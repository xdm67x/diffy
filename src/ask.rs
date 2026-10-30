//! "Ask to agent": a read-only Claude session that answers a question about one
//! line of a PR diff.
//!
//! The session runs without `Bash`, so it physically cannot run `gh` or `git`
//! and can never write to the PR: only the read-only file tools are granted.
//! Nothing the session produces is posted anywhere; the answer is shown in the
//! diff panel as a task under the line that was asked about.

use crate::claude;
use crate::models::PullRequest;
use color_eyre::eyre::{Result, WrapErr};
use std::thread;

/// System prompt for the read-only ask sessions. Kept separate from
/// `SYSTEM_PROMPT.md`, which is written for reviewers that may run `gh`.
const ASK_SYSTEM_PROMPT: &str = r#"ROLE: read-only diff assistant.

You answer questions about a specific line of a pull request diff. The PR metadata, the
selected line, and the full diff are provided in the prompt.

You are running in a strictly read-only session: you have no command execution and no write
tools, so you cannot modify files, run gh or git, or post anything anywhere. Never claim
that you changed, posted, or will change anything. If the question asks for a fix, describe
the fix in prose instead of applying it.

Answer in plain sentences, no headings, bullets, bold, em dashes, or filler. Be concise and
ground every claim in the provided diff; if the diff does not contain enough to answer,
say exactly what is missing instead of guessing."#;

/// Which side of the diff a line belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AskSide {
    Original,
    Modified,
}

impl AskSide {
    pub fn label(self) -> &'static str {
        match self {
            AskSide::Original => "old",
            AskSide::Modified => "new",
        }
    }
}

/// Everything a session needs to answer one question about one line.
#[derive(Clone, Debug)]
pub struct AskRequest {
    pub pr: PullRequest,
    /// The full patch, as context for the question.
    pub diff: String,
    pub path: String,
    pub side: AskSide,
    /// One-based line number on `side`.
    pub line: usize,
    /// The code of that line, when it could be read from the patch.
    pub code: Option<String>,
    pub question: String,
}

/// One question asked about a line, with the answer as it arrives.
#[derive(Clone, Debug)]
pub struct AskTask {
    pub id: u64,
    pub path: String,
    pub side: AskSide,
    pub line: usize,
    pub code: Option<String>,
    pub question: String,
    pub progress: String,
    pub status: AskStatus,
}

#[derive(Clone, Debug)]
pub enum AskStatus {
    Running,
    Done(String),
    Failed(String),
}

/// What an ask session reports while it runs. Sent from the session thread, so
/// the GPUI view forwards it onto the foreground thread like review events.
#[derive(Debug)]
pub enum AskEvent {
    Progress {
        url: String,
        id: u64,
        text: String,
    },
    Done {
        url: String,
        id: u64,
        answer: String,
    },
    Failed {
        url: String,
        id: u64,
        error: String,
    },
}

/// Runs one read-only ask session on its own thread.
pub fn spawn(id: u64, request: AskRequest, on_event: impl Fn(AskEvent) + Send + Sync + 'static) {
    thread::spawn(move || {
        let url = request.pr.url.clone();
        let progress = |text: String| {
            on_event(AskEvent::Progress {
                url: url.clone(),
                id,
                text,
            });
        };
        match run(&request, &progress) {
            Ok(answer) => on_event(AskEvent::Done { url, id, answer }),
            Err(error) => on_event(AskEvent::Failed {
                url,
                id,
                error: format!("{error:#}"),
            }),
        }
    });
}

fn run(request: &AskRequest, progress: &(dyn Fn(String) + Sync)) -> Result<String> {
    #[cfg(feature = "mock")]
    return Ok(crate::mock::ask_answer(request));
    let prompt = build_prompt(request);
    claude::run_readonly(ASK_SYSTEM_PROMPT, &prompt, &|line: &str| {
        progress(line.to_string())
    })
    .wrap_err("the read-only ask session failed")
}

/// The prompt carries the PR, the selected line, the question, and the whole
/// diff, so the session can answer without any tool access at all.
pub(crate) fn build_prompt(request: &AskRequest) -> String {
    let (diff, truncated) =
        crate::review::truncate_utf8(&request.diff, crate::review::MAX_DIFF_CHARS);
    let diff_block = if truncated {
        format!(
            "{diff}\n\n[diff truncated at {} bytes; answer from what is visible]",
            crate::review::MAX_DIFF_CHARS
        )
    } else {
        diff.to_string()
    };
    let code_block = match &request.code {
        Some(code) => format!("```\n{}\n```", code.trim_end()),
        None => "(the line is not part of the visible patch)".to_string(),
    };
    let pr = &request.pr;
    format!(
        r#"PR: {repo}#{number} — {title}
URL: {url}
Author: {author}

PR DESCRIPTION
---
{description}
---

SELECTED LINE
file: {path}
side: {side} (the {side} version of the file)
line: {line}
code:
{code_block}

QUESTION
---
{question}
---

FULL DIFF (context)
---
{diff_block}
---

Answer the question about the selected line. Stay read-only: describe any fix in prose,
never claim to apply or post anything."#,
        repo = pr.repo_name(),
        number = pr.number,
        title = pr.title,
        url = pr.url,
        author = pr.author.login,
        description = pr.description(),
        path = request.path,
        side = request.side.label(),
        line = request.line,
        code_block = code_block,
        question = request.question.trim(),
        diff_block = diff_block,
    )
}

/// Reads the code of one line out of a unified patch, without reconstructing
/// anything: only lines the patch supplies are considered. `path` is matched
/// against both the `a/` and `b/` names of each file, so deleted files work.
pub fn line_code(patch: &str, path: &str, side: AskSide, line: usize) -> Option<String> {
    let mut in_file = false;
    let mut old_no = 0usize;
    let mut new_no = 0usize;
    // Lines still owed to the current hunk on each side; both zero means the
    // hunk ended and the next lines are headers, not content.
    let mut old_left = 0usize;
    let mut new_left = 0usize;

    for raw in patch.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            let mut names = rest.split_whitespace();
            let a = names.next().unwrap_or("").strip_prefix("a/").unwrap_or("");
            let b = names.next().unwrap_or("").strip_prefix("b/").unwrap_or("");
            in_file = a == path || b == path;
            old_left = 0;
            new_left = 0;
            continue;
        }
        if !in_file {
            continue;
        }
        if old_left == 0 && new_left == 0 {
            // Outside a hunk: file headers, or a hunk header up next.
            if raw.starts_with("@@")
                && let Some((old_start, old_count, new_start, new_count)) = parse_hunk(raw)
            {
                old_no = old_start;
                new_no = new_start;
                old_left = old_count;
                new_left = new_count;
            }
            continue;
        }
        let (prefix, content) = if raw.is_empty() {
            (" ", "")
        } else {
            raw.split_at(1)
        };
        match prefix {
            "-" => {
                if side == AskSide::Original && old_no == line {
                    return Some(content.to_string());
                }
                old_no += 1;
                old_left = old_left.saturating_sub(1);
            }
            "+" => {
                if side == AskSide::Modified && new_no == line {
                    return Some(content.to_string());
                }
                new_no += 1;
                new_left = new_left.saturating_sub(1);
            }
            " " | "" => {
                // A context line counts on both sides; an empty patch line is a
                // context line whose content is empty.
                if side == AskSide::Original && old_no == line {
                    return Some(content.to_string());
                }
                if side == AskSide::Modified && new_no == line {
                    return Some(content.to_string());
                }
                old_no += 1;
                new_no += 1;
                old_left = old_left.saturating_sub(1);
                new_left = new_left.saturating_sub(1);
            }
            _ => {}
        }
    }
    None
}

/// Parses a `@@ -old[,old_count] +new[,new_count] @@` header into
/// `(old_start, old_count, new_start, new_count)`; a missing count is 1.
fn parse_hunk(header: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = header.trim_start().strip_prefix("@@")?.trim_start();
    let (old_side, rest) = rest.split_once(' ')?;
    let (new_side, _) = rest.split_once(' ').unwrap_or((rest, ""));
    let mut old = old_side.strip_prefix('-')?.split(',');
    let old_start = old.next()?.parse().ok()?;
    let old_count = old.next().and_then(|count| count.parse().ok()).unwrap_or(1);
    let mut new = new_side.strip_prefix('+')?.split([',', ' ', '@']);
    let new_start = new.next()?.parse().ok()?;
    let new_count = new.next().and_then(|count| count.parse().ok()).unwrap_or(1);
    Some((old_start, old_count, new_start, new_count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Author, Repository};

    const PATCH: &str = "diff --git a/src/pay.rs b/src/pay.rs
index 111..222 100644
--- a/src/pay.rs
+++ b/src/pay.rs
@@ -38,4 +38,5 @@ impl Payment for Card {
     fn charge(&self, order: &Order) -> Result<Receipt> {
-        self.gateway.charge(order.total())
+        let mut attempts = 0;
+        self.retry(order)
         Ok(receipt)
     }
diff --git a/src/gone.rs b/src/gone.rs
deleted file mode 100644
--- a/src/gone.rs
+++ /dev/null
@@ -1,1 +0,0 @@
-bye
";

    #[test]
    fn line_code_reads_added_removed_and_context_lines() {
        assert_eq!(
            line_code(PATCH, "src/pay.rs", AskSide::Modified, 39),
            Some("        let mut attempts = 0;".to_string())
        );
        assert_eq!(
            line_code(PATCH, "src/pay.rs", AskSide::Modified, 40),
            Some("        self.retry(order)".to_string())
        );
        assert_eq!(
            line_code(PATCH, "src/pay.rs", AskSide::Original, 39),
            Some("        self.gateway.charge(order.total())".to_string())
        );
        assert_eq!(
            line_code(PATCH, "src/pay.rs", AskSide::Modified, 41),
            Some("        Ok(receipt)".to_string())
        );
        assert_eq!(
            line_code(PATCH, "src/pay.rs", AskSide::Original, 40),
            Some("        Ok(receipt)".to_string())
        );
        assert_eq!(
            line_code(PATCH, "src/pay.rs", AskSide::Modified, 42),
            Some("    }".to_string())
        );
        assert_eq!(line_code(PATCH, "src/pay.rs", AskSide::Modified, 99), None);
    }

    #[test]
    fn line_code_matches_the_original_path_of_a_deleted_file() {
        assert_eq!(
            line_code(PATCH, "src/gone.rs", AskSide::Original, 1),
            Some("bye".to_string())
        );
        assert_eq!(line_code(PATCH, "src/gone.rs", AskSide::Modified, 1), None);
    }

    #[test]
    fn line_code_ignores_files_that_do_not_match() {
        assert_eq!(
            line_code(PATCH, "src/other.rs", AskSide::Modified, 40),
            None
        );
    }

    fn request(question: &str) -> AskRequest {
        AskRequest {
            pr: PullRequest {
                number: 412,
                title: "Retry card charges".to_string(),
                url: "https://github.com/acme/storefront/pull/412".to_string(),
                body: "Retries the charge.".to_string(),
                repository: Repository {
                    name_with_owner: "acme/storefront".to_string(),
                },
                author: Author {
                    login: "dana".to_string(),
                },
                is_draft: false,
                by_me: false,
                updated_at: String::new(),
                comments_count: 0,
            },
            diff: PATCH.to_string(),
            path: "src/pay.rs".to_string(),
            side: AskSide::Modified,
            line: 41,
            code: Some("        self.retry(order)".to_string()),
            question: question.to_string(),
        }
    }

    #[test]
    fn the_prompt_carries_the_line_the_question_and_the_diff() {
        let prompt = build_prompt(&request("why does this retry synchronously?"));
        assert!(prompt.contains("acme/storefront#412 — Retry card charges"));
        assert!(prompt.contains("file: src/pay.rs"));
        assert!(prompt.contains("side: new"));
        assert!(prompt.contains("line: 41"));
        assert!(prompt.contains("        self.retry(order)"));
        assert!(prompt.contains("why does this retry synchronously?"));
        assert!(prompt.contains("self.gateway.charge(order.total())"));
        assert!(prompt.contains("Stay read-only"));
    }

    #[test]
    fn the_prompt_notes_when_the_line_is_not_in_the_patch() {
        let mut req = request("what is this?");
        req.code = None;
        let prompt = build_prompt(&req);
        assert!(prompt.contains("the line is not part of the visible patch"));
    }

    #[test]
    fn hunk_counts_default_to_one_and_parse_explicit_counts() {
        assert_eq!(
            parse_hunk("@@ -38,3 +38,4 @@ impl X {").unwrap(),
            (38, 3, 38, 4)
        );
        assert_eq!(parse_hunk("@@ -1 +0,0 @@").unwrap(), (1, 1, 0, 0));
        assert_eq!(parse_hunk("@@ -1,1 +0,0 @@").unwrap(), (1, 1, 0, 0));
        assert_eq!(parse_hunk("@@ -0,0 +1,3 @@").unwrap(), (0, 0, 1, 3));
        assert_eq!(
            parse_hunk("not a hunk").unwrap_or((0, 0, 0, 0)),
            (0, 0, 0, 0)
        );
        assert!(parse_hunk("@@ -x +y @@").is_none());
    }

    #[test]
    fn ask_events_are_debuggable() {
        let event = AskEvent::Done {
            url: "u".to_string(),
            id: 7,
            answer: "because".to_string(),
        };
        assert!(format!("{event:?}").contains("because"));
    }
}
