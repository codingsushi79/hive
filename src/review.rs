//! Code review: what a reviewer is told, and how its verdict is read back.
//!
//! A reviewer is an ordinary agent pointed at another agent's directory with
//! read-only tools. It ends its reply with a JSON verdict, which becomes
//! structured findings the author can be handed in one keystroke.

use serde::Deserialize;

use crate::model::Finding;

/// Tools a reviewer may use. Anything that could modify files is absent, and
/// headless mode denies whatever isn't listed.
pub const REVIEWER_TOOLS: &str = "Read,Grep,Glob,Bash(git diff:*),Bash(git log:*),Bash(git show:*),Bash(git status:*)";

/// The reviewer's brief. With `inline`, the diff is in the prompt itself, so
/// a small change can be reviewed without a single tool call.
pub fn prompt(task: &str, base: &str, inline: Option<&str>) -> String {
    let source = match inline {
        Some(diff) => format!(
            "The complete diff is below (it includes uncommitted work). Read surrounding code only \
where the diff alone isn't enough to judge it.\n\n<diff>\n{diff}\n</diff>"
        ),
        None => format!(
            "Run `git diff {base}` to see them (it includes uncommitted work), and read the \
surrounding code wherever you need context."
        ),
    };
    format!(
        "You are a meticulous senior code reviewer. Review the changes in this directory \
relative to commit {base}. {source}

The author was asked to do this:
<task>
{task}
</task>

Look for correctness bugs, security problems, unhandled edge cases, broken or missing tests, \
and whether the change actually accomplishes the task. Do not report style nits or matters of \
taste. Verify each finding against the code before reporting it. Do not modify any files.

Finish your reply with exactly one fenced ```json block and nothing after it:
{{\"verdict\": \"approve\" or \"changes\", \"summary\": \"one or two sentences\", \
\"findings\": [{{\"severity\": \"high\" | \"medium\" | \"low\", \"file\": \"path\", \"line\": 42, \
\"summary\": \"what is wrong and why it matters\"}}]}}
Use \"approve\" with an empty findings list when nothing needs to change."
    )
}

#[derive(Debug, Deserialize)]
struct Verdict {
    verdict: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    findings: Vec<Finding>,
}

pub struct Parsed {
    pub verdict: String,
    pub summary: String,
    pub findings: Vec<Finding>,
}

/// Read the verdict from the reviewer's final message. Takes the last JSON
/// object that parses, fenced or not; anything else is `unparsed`, keeping the
/// reviewer's own words as the summary.
pub fn parse(text: &str) -> Parsed {
    for candidate in candidates(text).into_iter().rev() {
        if let Ok(v) = serde_json::from_str::<Verdict>(&candidate) {
            let verdict = match v.verdict.to_ascii_lowercase().as_str() {
                "approve" | "approved" | "lgtm" => "approve",
                _ => "changes",
            };
            let verdict = if verdict == "changes" && v.findings.is_empty() {
                // Asked for changes but named none: nothing to act on.
                "approve"
            } else {
                verdict
            };
            return Parsed {
                verdict: verdict.into(),
                summary: v.summary,
                findings: v.findings,
            };
        }
    }
    Parsed {
        verdict: "unparsed".into(),
        summary: crate::model::one_line(text, 300),
        findings: vec![],
    }
}

fn candidates(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        let after = &rest[start + 3..];
        let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
        let Some(end) = after[body_start..].find("```") else { break };
        out.push(after[body_start..body_start + end].trim().to_string());
        rest = &after[body_start + end + 3..];
    }
    // An unfenced object: from the last `{"verdict"` to the final brace.
    if let Some(start) = text.rfind("{\"verdict\"").or_else(|| text.rfind("{ \"verdict\""))
        && let Some(end) = text.rfind('}')
        && end > start
    {
        out.push(text[start..=end].to_string());
    }
    out
}

/// Stable fingerprint of a diff, to recognise one that was already reviewed.
pub fn diff_hash(diff: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in diff.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// The message that hands findings back to the author.
pub fn findings_message(reviewer: &str, summary: &str, findings: &[Finding]) -> String {
    let mut msg = format!("Code review from {reviewer} requested changes.");
    if !summary.is_empty() {
        msg.push_str(&format!("\n\nSummary: {summary}"));
    }
    msg.push_str("\n\nFindings:");
    for f in findings {
        let at = match (f.file.is_empty(), f.line) {
            (true, _) => String::new(),
            (false, Some(line)) => format!(" {}:{line}", f.file),
            (false, None) => format!(" {}", f.file),
        };
        msg.push_str(&format!("\n- [{}]{at} — {}", f.severity, f.summary));
    }
    msg.push_str(
        "\n\nAddress each finding. If you believe one is wrong, say why instead of changing the code.",
    );
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fenced_verdict() {
        let text = "I looked.\n```json\n{\"verdict\":\"changes\",\"summary\":\"s\",\"findings\":[{\"severity\":\"high\",\"file\":\"a.rs\",\"line\":3,\"summary\":\"off by one\"}]}\n```";
        let p = parse(text);
        assert_eq!(p.verdict, "changes");
        assert_eq!(p.findings.len(), 1);
        assert_eq!(p.findings[0].line, Some(3));
    }

    #[test]
    fn reads_bare_verdict_and_normalises() {
        let p = parse("All good. {\"verdict\": \"LGTM\", \"findings\": []}");
        assert_eq!(p.verdict, "approve");
        let p = parse("{\"verdict\":\"changes\",\"findings\":[]}");
        assert_eq!(p.verdict, "approve");
    }

    #[test]
    fn keeps_prose_when_unparsable() {
        let p = parse("Looks fine to me overall.");
        assert_eq!(p.verdict, "unparsed");
        assert_eq!(p.summary, "Looks fine to me overall.");
    }

    #[test]
    fn prefers_last_block() {
        let text = "```json\n{\"verdict\":\"approve\"}\n```\nwait\n```json\n{\"verdict\":\"changes\",\"findings\":[{\"severity\":\"low\",\"summary\":\"x\"}]}\n```";
        assert_eq!(parse(text).verdict, "changes");
    }

    #[test]
    fn message_lists_findings() {
        let f = vec![Finding {
            severity: "high".into(),
            file: "src/a.rs".into(),
            line: Some(9),
            summary: "panics on empty input".into(),
        }];
        let m = findings_message("r1", "", &f);
        assert!(m.contains("- [high] src/a.rs:9 — panics on empty input"));
    }
}
