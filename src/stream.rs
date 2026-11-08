//! Turning Claude Code's `--output-format stream-json` into transcript
//! entries and bookkeeping.

use serde_json::Value;

use crate::model::{Entry, EntryKind, one_line};

#[derive(Debug)]
pub enum Update {
    Session {
        id: String,
        model: Option<String>,
    },
    Entry(Entry),
    Result {
        text: String,
        is_error: bool,
        cost_usd: f64,
        turns: u32,
        tokens_in: u64,
        tokens_cached: u64,
        tokens_out: u64,
        session_id: Option<String>,
    },
}

pub fn parse(line: &str) -> Vec<Update> {
    let line = line.trim();
    if line.is_empty() {
        return vec![];
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        // Not JSON: a warning or banner the CLI printed. Worth seeing.
        return vec![Update::Entry(Entry::new(EntryKind::Info, line))];
    };
    match v["type"].as_str() {
        Some("system") => match v["subtype"].as_str() {
            Some("init") => v["session_id"]
                .as_str()
                .map(|id| Update::Session {
                    id: id.to_string(),
                    model: v["model"].as_str().map(str::to_string),
                })
                .into_iter()
                .collect(),
            Some("compact_boundary") => {
                vec![Update::Entry(Entry::new(EntryKind::Info, "context compacted"))]
            }
            _ => vec![],
        },
        Some("assistant") => blocks(&v)
            .iter()
            .filter_map(|block| match block["type"].as_str() {
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or("").trim();
                    (!text.is_empty()).then(|| Entry::new(EntryKind::Text, text))
                }
                Some("tool_use") => {
                    let name = block["name"].as_str().unwrap_or("tool");
                    Some(Entry::new(
                        EntryKind::Tool,
                        format!("{name}  {}", tool_summary(name, &block["input"])),
                    ))
                }
                _ => None,
            })
            .map(Update::Entry)
            .collect(),
        Some("user") => blocks(&v)
            .iter()
            .filter(|b| b["type"] == "tool_result")
            .map(|block| {
                let text = result_text(&block["content"]);
                let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
                let mut preview = one_line(&text, 160);
                if preview.is_empty() {
                    preview = "(no output)".into();
                }
                if lines > 1 {
                    preview.push_str(&format!("  (+{} lines)", lines - 1));
                }
                let kind = if block["is_error"].as_bool().unwrap_or(false) {
                    EntryKind::ToolErr
                } else {
                    EntryKind::ToolOk
                };
                Update::Entry(Entry::new(kind, preview))
            })
            .collect(),
        Some("result") => {
            let usage = &v["usage"];
            let n = |k: &str| usage[k].as_u64().unwrap_or(0);
            let is_error = v["is_error"].as_bool().unwrap_or(false)
                || v["subtype"].as_str().is_some_and(|s| s != "success");
            let text = v["result"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v["subtype"].as_str().unwrap_or("finished").replace('_', " "));
            vec![Update::Result {
                text,
                is_error,
                cost_usd: v["total_cost_usd"].as_f64().unwrap_or(0.0),
                turns: v["num_turns"].as_u64().unwrap_or(0) as u32,
                tokens_in: n("input_tokens")
                    + n("cache_read_input_tokens")
                    + n("cache_creation_input_tokens"),
                tokens_cached: n("cache_read_input_tokens"),
                tokens_out: n("output_tokens"),
                session_id: v["session_id"].as_str().map(str::to_string),
            }]
        }
        _ => vec![],
    }
}

fn blocks(v: &Value) -> Vec<Value> {
    v["message"]["content"].as_array().cloned().unwrap_or_default()
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The one argument that says what a tool call is doing.
fn tool_summary(name: &str, input: &Value) -> String {
    let field = |k: &str| input[k].as_str().map(|s| one_line(s, 140));
    let summary = match name {
        "Bash" => field("command"),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            field("file_path").or_else(|| field("notebook_path"))
        }
        "Grep" | "Glob" => field("pattern"),
        "Task" | "Agent" => field("description"),
        "WebFetch" => field("url"),
        "WebSearch" => field("query"),
        "TodoWrite" => input["todos"]
            .as_array()
            .map(|t| format!("{} todos", t.len())),
        _ => None,
    };
    summary.unwrap_or_else(|| {
        let compact = input.to_string();
        one_line(&compact, 120)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_turn() {
        let init = r#"{"type":"system","subtype":"init","session_id":"abc","model":"claude-x"}"#;
        assert!(matches!(&parse(init)[..], [Update::Session { id, .. }] if id == "abc"));

        let tool = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking."},{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#;
        let updates = parse(tool);
        assert_eq!(updates.len(), 2);
        assert!(matches!(&updates[1], Update::Entry(e) if e.kind == EntryKind::Tool && e.text == "Bash  cargo test"));

        let result = r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":[{"type":"text","text":"boom\nline2"}]}]}}"#;
        assert!(matches!(&parse(result)[0], Update::Entry(e) if e.kind == EntryKind::ToolErr && e.text == "boom  (+1 lines)"));

        let done = r#"{"type":"result","subtype":"success","is_error":false,"result":"Done.","total_cost_usd":0.12,"num_turns":3,"session_id":"abc","usage":{"input_tokens":10,"cache_read_input_tokens":90,"output_tokens":5}}"#;
        match &parse(done)[0] {
            Update::Result { text, is_error, cost_usd, turns, tokens_in, tokens_cached, tokens_out, .. } => {
                assert_eq!(text, "Done.");
                assert!(!is_error);
                assert_eq!((*cost_usd, *turns, *tokens_in, *tokens_out), (0.12, 3, 100, 5));
                assert_eq!(*tokens_cached, 90);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn non_json_lines_are_kept() {
        assert!(matches!(&parse("warning: something")[0], Update::Entry(e) if e.kind == EntryKind::Info));
        assert!(parse("  ").is_empty());
    }
}
