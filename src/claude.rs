use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Runs one headless Claude session with `SYSTEM_PROMPT.md` as the session
/// system prompt. The prompt is fed through stdin to avoid argument length
/// limits on large diffs. `gh`, `git`, and read-only file tools are
/// pre-approved; everything else is denied instead of prompting.
pub fn run(system_prompt: &str, prompt: &str) -> Result<String> {
    execute(base_command(Some(system_prompt)), prompt)
}

/// Runs one reviewer as a custom Claude agent. The agent definition carries
/// `SYSTEM_PROMPT.md` as its system prompt. Two calls with the same `agents_file`,
/// `agent`, and `prompt` get identical prompts in separate contexts.
pub fn run_agent(agents_file: &Path, agent: &str, prompt: &str) -> Result<String> {
    let mut command = base_command(None);
    command
        .arg("--agents")
        .arg(agents_file)
        .arg("--agent")
        .arg(agent);
    execute(command, prompt)
}

fn base_command(system_prompt: Option<&str>) -> Command {
    let mut command = Command::new("claude");
    command
        .arg("--print")
        .args(["--output-format", "json"])
        .args(["--model", "opus"])
        .arg("--no-session-persistence")
        .args(["--permission-mode", "dontAsk"])
        .args(["--tools", "Bash,Read,Glob,Grep"])
        .arg("--allowedTools")
        .args([
            "Bash(gh *)",
            "Bash(git *)",
            "Bash(mktemp *)",
            "Bash(rm -rf /tmp/diffy-review-*)",
            "Read",
            "Glob",
            "Grep",
        ]);
    if let Some(system_prompt) = system_prompt {
        command.args(["--system-prompt", system_prompt]);
    }
    command
}

fn execute(mut command: Command, prompt: &str) -> Result<String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err("failed to run `claude`; is Claude Code installed?")?;

    child
        .stdin
        .take()
        .ok_or_else(|| eyre!("could not open claude stdin"))?
        .write_all(prompt.as_bytes())
        .wrap_err("could not write prompt to claude")?;

    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The CLI returns an envelope even on API errors; the detail lives in `result`.
    let mut text = stdout.into_owned();
    let mut is_error = false;
    if let Ok(envelope) = serde_json::from_str::<Value>(&text) {
        is_error = envelope
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(result) = envelope.get("result").and_then(Value::as_str) {
            text = result.to_string();
        }
    }

    if !output.status.success() || is_error {
        let detail = if text.trim().is_empty() {
            stderr.trim()
        } else {
            text.trim()
        };
        bail!("claude failed ({}): {detail}", output.status);
    }
    Ok(text)
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
}
