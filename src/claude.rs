use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::thread;

/// Runs one headless Claude session with `SYSTEM_PROMPT.md` as the session
/// system prompt. The prompt is fed through stdin to avoid argument length
/// limits on large diffs. Output is streamed so `progress` sees every tool
/// call the session makes. `gh`, `git`, and read-only file tools are
/// pre-approved; everything else is denied instead of prompting.
pub fn run(system_prompt: &str, prompt: &str, progress: &(dyn Fn(&str) + Sync)) -> Result<String> {
    execute(base_command(system_prompt), prompt, progress)
}

fn base_command(system_prompt: &str) -> Command {
    let mut command = Command::new("claude");
    command
        .arg("--print")
        .args(["--model", "opus"])
        .arg("--no-session-persistence")
        .args(["--permission-mode", "dontAsk"])
        .args(["--tools", "Bash,Read,Glob,Grep"])
        .arg("--allowedTools")
        // Both the `gh *` and `gh:*` rule spellings are accepted by different
        // Claude Code versions; listing both keeps the allowlist working.
        .args([
            "Bash(gh *)",
            "Bash(git *)",
            "Bash(mktemp *)",
            "Bash(rm -rf /tmp/diffy-review-*)",
            "Bash(gh:*)",
            "Bash(git:*)",
            "Bash(mktemp:*)",
            "Read",
            "Glob",
            "Grep",
        ])
        .args(["--system-prompt", system_prompt]);
    command
}

fn execute(mut command: Command, prompt: &str, progress: &(dyn Fn(&str) + Sync)) -> Result<String> {
    let mut child = command
        .args(["--output-format", "stream-json"])
        .arg("--verbose")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err("failed to run `claude`; is Claude Code installed?")?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| eyre!("could not open claude stdin"))?;
        stdin
            .write_all(prompt.as_bytes())
            .wrap_err("could not write prompt to claude")?;
    }

    // Drain stderr on its own thread so a chatty session cannot fill the pipe.
    let stderr = child.stderr.take().map(|mut stderr| {
        thread::spawn(move || {
            let mut buffer = String::new();
            let _ = stderr.read_to_string(&mut buffer);
            buffer
        })
    });

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| eyre!("could not open claude stdout"))?;
    let mut result: Option<ResultEnvelope> = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.wrap_err("could not read claude output")?;
        if let Some(label) = progress_label(&line) {
            progress(&label);
        }
        if let Some(envelope) = result_envelope(&line) {
            result = Some(envelope);
        }
    }

    let status = child.wait()?;
    let stderr = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let result = result.ok_or_else(|| {
        eyre!(
            "claude produced no result event ({status}); stderr: {}",
            stderr.trim()
        )
    })?;

    if !status.success() || result.is_error {
        let detail = if result.text.trim().is_empty() {
            stderr.trim()
        } else {
            result.text.trim()
        };
        bail!("claude failed ({status}): {detail}");
    }
    Ok(result.text)
}

struct ResultEnvelope {
    text: String,
    is_error: bool,
}

/// The final `{"type":"result",...}` line of a `stream-json` session.
fn result_envelope(line: &str) -> Option<ResultEnvelope> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "result" {
        return None;
    }
    Some(ResultEnvelope {
        text: value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        is_error: value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// A one-line description of the first tool call in an assistant message, so
/// the UI can show what a reviewer is doing right now.
fn progress_label(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let content = value.get("message")?.get("content")?.as_array()?;
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let name = block.get("name")?.as_str()?;
        let input = block.get("input")?;
        let text = match name {
            "Bash" => input.get("command")?.as_str()?.to_string(),
            "Read" => format!("read {}", input.get("file_path")?.as_str()?),
            "Grep" => format!("grep {}", input.get("pattern")?.as_str()?),
            "Glob" => format!("glob {}", input.get("pattern")?.as_str()?),
            other => other.to_string(),
        };
        return Some(shorten(&text.replace('\n', " "), 100));
    }
    None
}

fn shorten(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T> {
    let candidate = extract_json(text).ok_or_else(|| {
        eyre!(
            "model response did not contain a JSON object: {}",
            preview(text)
        )
    })?;
    serde_json::from_str(&candidate)
        .wrap_err_with(|| format!("could not parse model JSON: {}", preview(&candidate)))
}

/// Pulls the first JSON object/array out of a response that may be wrapped in
/// prose or ```json fences.
fn extract_json(text: &str) -> Option<String> {
    let cleaned = text.replace("```json", "").replace("```", "");
    let start = cleaned.find(['{', '['])?;
    let end = cleaned.rfind(['}', ']'])?;
    (start < end).then(|| cleaned[start..=end].to_string())
}

fn preview(text: &str) -> String {
    let text = text.trim().replace('\n', " ");
    text.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_json_from_fences_and_prose() {
        let text = "Here is the result:\n```json\n{\"verdict\":\"merge\"}\n```\nDone.";
        assert_eq!(
            extract_json(text).as_deref(),
            Some("{\"verdict\":\"merge\"}")
        );
    }

    #[test]
    fn extracts_nested_json() {
        let text = "{\"a\":{\"b\":1},\"c\":[2]}";
        assert_eq!(extract_json(text).as_deref(), Some(text));
    }

    #[test]
    fn reports_bash_calls_as_progress() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"gh pr diff https://github.com/a/b/pull/1"}}]}}"#;
        assert_eq!(
            progress_label(line).as_deref(),
            Some("gh pr diff https://github.com/a/b/pull/1")
        );
    }

    #[test]
    fn ignores_non_tool_messages() {
        assert!(
            progress_label(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#
            )
            .is_none()
        );
        assert!(progress_label(r#"{"type":"user"}"#).is_none());
    }

    #[test]
    fn reads_the_final_result_event() {
        let line = r#"{"type":"result","is_error":false,"result":"{\"verdict\":\"merge\"}"}"#;
        let envelope = result_envelope(line).expect("result event");
        assert_eq!(envelope.text, r#"{"verdict":"merge"}"#);
        assert!(!envelope.is_error);
        assert!(result_envelope(r#"{"type":"assistant"}"#).is_none());
    }
}
