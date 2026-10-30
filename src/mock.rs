//! Fixture data so Diffy can be tried without a GitHub account or Claude Code.
//! Built only with `--features mock`; every `gh` and `claude` call is then
//! answered from here.

use crate::ask::AskRequest;
use crate::models::{Author, Finding, PullRequest, Repository, ReviewReport, Verdict};
use crate::review::Stage;
use crate::time;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Fixtures run instantly under test; a real mock run steps through the stages.
fn pause(millis: u64) {
    if cfg!(test) {
        return;
    }
    sleep(Duration::from_millis(millis));
}

fn hours_ago(hours: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default();
    time::utc_iso(now - (hours as i64) * 3_600)
}

fn pr(
    repo: &str,
    number: u64,
    title: &str,
    author: &str,
    body: &str,
    hours: u64,
    by_me: bool,
) -> PullRequest {
    PullRequest {
        number,
        title: title.to_string(),
        url: format!("https://github.com/{repo}/pull/{number}"),
        body: body.to_string(),
        repository: Repository {
            name_with_owner: repo.to_string(),
        },
        author: Author {
            login: author.to_string(),
        },
        is_draft: false,
        by_me,
        updated_at: hours_ago(hours),
        comments_count: 1,
    }
}

/// Two repos, three PRs: one waiting on your review, one of your own, and one
/// from a different repo so the grouping and the filter are visible.
pub fn prs() -> Vec<PullRequest> {
    vec![
        pr(
            "acme/storefront",
            412,
            "Checkout: retry failed card charges",
            "dana",
            "Retries the gateway charge up to three times so a flaky card charge does not \
             fail the order.\n\n![retry flow](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP438NAEiJN9aiGUQ1DSgMA/YOLELHsQkgAAAAASUVORK5CYII=)\n\n\
             <video src=\"https://github.com/user-attachments/assets/0f2b1c3d-e4f5\" controls></video>",
            2,
            false,
        ),
        pr(
            "acme/storefront",
            408,
            "Cache product images for a day",
            "me",
            "Product images do not change during the day, so the cache can live for 24 hours \
             instead of one minute.",
            7,
            true,
        ),
        pr(
            "acme/api",
            97,
            "Add pagination to the orders endpoint",
            "rafael",
            "Adds a cursor to `GET /orders` and passes it to the database query.",
            26,
            false,
        ),
    ]
}

const PAY_DIFF: &str = "diff --git a/src/checkout/pay.rs b/src/checkout/pay.rs
--- a/src/checkout/pay.rs
+++ b/src/checkout/pay.rs
@@ -38,4 +38,12 @@ impl Payment for Card {
     fn charge(&self, order: &Order) -> Result<Receipt> {
-        self.gateway.charge(order.total())
+        let mut attempts = 0;
+        loop {
+            match self.gateway.charge(order.total()) {
+                Ok(receipt) => return Ok(receipt),
+                Err(error) if attempts < 3 && error.is_retryable() => attempts += 1,
+                Err(error) => return Err(error),
+            }
+            std::thread::sleep(std::time::Duration::from_millis(200));
+        }
     }
 }
diff --git a/src/checkout/pay_test.rs b/src/checkout/pay_test.rs
--- a/src/checkout/pay_test.rs
+++ b/src/checkout/pay_test.rs
@@ -1,2 +1,7 @@
 use super::*;
 
+#[test]
+fn retry_stops_after_three_attempts() {
+    let card = Card::flaky(4);
+    assert!(card.charge(&order()).is_err());
+}
";

const CACHE_DIFF: &str = "diff --git a/src/catalog/image_cache.rs b/src/catalog/image_cache.rs
--- a/src/catalog/image_cache.rs
+++ b/src/catalog/image_cache.rs
@@ -12,3 +12,3 @@ impl ImageCache {
     pub fn ttl(&self) -> Duration {
-        Duration::from_secs(60)
+        Duration::from_secs(24 * 60 * 60)
     }
";

const ORDERS_DIFF: &str = "diff --git a/src/api/orders.rs b/src/api/orders.rs
--- a/src/api/orders.rs
+++ b/src/api/orders.rs
@@ -20,4 +20,5 @@ pub fn orders(query: &Query) -> Result<Page<Order>> {
     let limit = query.limit.unwrap_or(20);
-    let rows = db::orders(limit)?;
+    let offset = query.cursor.unwrap_or(0);
+    let rows = db::orders(limit, offset)?;
     Ok(Page { rows, next: None })
 }
";

/// The patch shown in the diff tab for a fixture PR.
pub fn diff(pr: &PullRequest) -> String {
    match pr.number {
        412 => PAY_DIFF,
        408 => CACHE_DIFF,
        _ => ORDERS_DIFF,
    }
    .to_string()
}

fn finding(
    file: &str,
    line: u64,
    severity: &str,
    title: &str,
    detail: &str,
    suggestion: Option<&str>,
) -> Finding {
    Finding {
        file: file.to_string(),
        line: Some(line),
        severity: severity.to_string(),
        title: title.to_string(),
        detail: detail.to_string(),
        suggestion: suggestion.map(str::to_string),
        intent_related: false,
    }
}

/// A review that walks through the same stages as a real run, so the progress
/// text, the findings and the verdict are all visible without Claude Code.
pub fn review(
    pr: &PullRequest,
    notify: &dyn Fn(Stage),
    progress: &(dyn Fn(String) + Sync),
) -> ReviewReport {
    notify(Stage::FetchingDiff);
    pause(300);
    notify(Stage::Reviewing);
    progress("reviewer 1 · gh pr diff".to_string());
    pause(600);
    progress("reviewer 2 · gh pr view".to_string());
    pause(600);
    notify(Stage::Judging);
    progress("judge · weighing findings".to_string());
    pause(400);

    match pr.number {
        412 => ReviewReport {
            verdict: Verdict::RequestChanges,
            summary: "The retry loop is the right idea, but a timed-out charge is retried \
                      without an idempotency key, so a customer can be charged twice."
                .to_string(),
            intent: "the retry is implemented, the duplicate charge is not handled".to_string(),
            confirmed: vec![finding(
                "src/checkout/pay.rs",
                41,
                "high",
                "Retry can charge the card twice",
                "The gateway call is repeated after a timeout without an idempotency key, so a \
                 charge that succeeded on the server but timed out in the client runs again. \
                 Derive a key from the order id and pass it to every attempt.",
                None,
            )],
            accepted: vec![
                finding(
                    "src/checkout/pay.rs",
                    46,
                    "medium",
                    "Backoff is constant",
                    "Every retry waits the same 200 ms, so a slow gateway sees three attempts \
                     within half a second.",
                    Some(
                        "std::thread::sleep(std::time::Duration::from_millis(200 * attempts + 200));",
                    ),
                ),
                finding(
                    "src/checkout/pay_test.rs",
                    6,
                    "low",
                    "Retry count is not asserted",
                    "The test checks that a charge eventually fails but not that the gateway was \
                     called exactly four times.",
                    None,
                ),
            ],
            dismissed: vec![(
                finding(
                    "src/checkout/pay.rs",
                    39,
                    "low",
                    "Method name should say retry",
                    "Rename `charge` so callers know the retries happen inside.",
                    None,
                ),
                "naming preference, not a defect".to_string(),
            )],
        },
        408 => ReviewReport {
            verdict: Verdict::Merge,
            summary: "One-line change, correct for images that are immutable per deploy."
                .to_string(),
            intent: "the description matches the change".to_string(),
            confirmed: Vec::new(),
            accepted: Vec::new(),
            dismissed: vec![(
                finding(
                    "src/catalog/image_cache.rs",
                    14,
                    "low",
                    "Day length is hard to read",
                    "Spell out the multiplication as a constant.",
                    None,
                ),
                "style only, filtered".to_string(),
            )],
        },
        _ => ReviewReport {
            verdict: Verdict::RequestChanges,
            summary: "The cursor is read but never handed back, so the client cannot ask for \
                      the second page."
                .to_string(),
            intent: "pagination is half implemented".to_string(),
            confirmed: vec![finding(
                "src/api/orders.rs",
                23,
                "high",
                "Next page cursor is always None",
                "`Page { next: None }` is still returned after adding the cursor, so every \
                 client stops after one page. Return `Some(offset + limit)` when the page came \
                 back full.",
                None,
            )],
            accepted: vec![finding(
                "src/api/orders.rs",
                22,
                "medium",
                "Offset without a stable order can skip rows",
                "Pages skip or repeat orders unless the query sorts by a unique column first. \
                 Add `ORDER BY id` before applying the offset.",
                None,
            )],
            dismissed: Vec::new(),
        },
    }
}

/// The fixture answer to an "ask to agent" question about one diff line. Like a
/// real ask session it only reads: it echoes the line and the question.
pub fn ask_answer(request: &AskRequest) -> String {
    pause(600);
    let code = request.code.as_deref().map(str::trim).unwrap_or("");
    format!(
        "On `{path}:{line}` ({side} side) the code is `{code}`. You asked: {question} \
         (fixture answer; a real run answers from the diff in a read-only session).",
        path = request.path,
        line = request.line,
        side = request.side.label(),
        code = code,
        question = request.question.trim(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::diff::DiffFile;

    #[test]
    fn every_fixture_pr_has_a_parseable_diff() {
        for pr in prs() {
            let patch = diff(&pr);
            match DiffFile::parse(&patch) {
                Ok(files) => assert!(!files.is_empty(), "{} parsed to no files", pr.url),
                Err(error) => panic!("the fixture diff for {} does not parse: {error}", pr.url),
            }
        }
    }

    #[test]
    fn fixture_urls_are_unique_and_timestamps_parse() {
        let prs = prs();
        for (index, pr) in prs.iter().enumerate() {
            assert!(time::parse_utc(&pr.updated_at).is_some(), "{}", pr.url);
            assert!(prs[index + 1..].iter().all(|other| other.url != pr.url));
        }
    }

    #[test]
    fn the_fixture_answer_names_the_line_and_echoes_the_question() {
        let pr = prs().into_iter().next().unwrap();
        let patch = diff(&pr);
        let request = AskRequest {
            pr,
            diff: patch,
            path: "src/checkout/pay.rs".to_string(),
            side: crate::ask::AskSide::Modified,
            line: 41,
            code: Some("        let mut attempts = 0;".to_string()),
            question: "why retry here?".to_string(),
        };
        let answer = ask_answer(&request);
        assert!(answer.contains("src/checkout/pay.rs:41"));
        assert!(answer.contains("why retry here?"));
        assert!(answer.contains("read-only"));
    }

    #[test]
    fn mock_reviews_keep_their_findings_within_their_diff() {
        for pr in prs() {
            let report = review(&pr, &|_| {}, &|_| {});
            for finding in report.confirmed.iter().chain(report.accepted.iter()) {
                assert!(
                    diff(&pr).contains(finding.file.as_str()),
                    "{} mentions a file outside its diff: {}",
                    pr.url,
                    finding.file
                );
            }
        }
    }
}
