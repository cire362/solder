//! Reviews changes before they are pushed: the model reads the outgoing
//! diff and reports problems through a tool, so they come back as data
//! (file, line, how serious, what) rather than prose to parse.

use crate::{
    provider::Endpoint,
    tools::{self, StepRequest, ToolSpec, Turn},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Wrong behavior, a crash, data loss, a security hole.
    Bug,
    /// Likely trouble: an edge case, a race, a missing check.
    Risk,
    /// Worth a look: unclear code, leftovers, small slips.
    Note,
}

impl Severity {
    fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "bug" | "error" | "critical" | "high" => Severity::Bug,
            "risk" | "warning" | "medium" => Severity::Risk,
            _ => Severity::Note,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Severity::Bug => "bug",
            Severity::Risk => "risk",
            Severity::Note => "note",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub file: String,
    /// 1-based, in the new version of the file; 0 when not given.
    pub line: u32,
    pub severity: Severity,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Review {
    pub findings: Vec<Finding>,
    /// What the model said besides its findings.
    pub note: String,
}

const SYSTEM: &str = "You review code changes before they are pushed. Read the diff and report \
real problems only: bugs, crashes, security holes, data loss, broken edge cases, leftover debug \
code or secrets. Do not report style or taste. Call report_findings exactly once, with an empty \
list when nothing is wrong. Line numbers refer to the new version of each file.";

pub fn tool() -> ToolSpec {
    ToolSpec {
        name: "report_findings".into(),
        description: "Report the problems found in the diff; an empty list when there are none."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "findings": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "file": { "type": "string" },
                            "line": { "type": "integer" },
                            "severity": { "type": "string", "enum": ["bug", "risk", "note"] },
                            "message": { "type": "string", "description": "One or two sentences." },
                        },
                        "required": ["file", "severity", "message"],
                    },
                },
            },
            "required": ["findings"],
        }),
    }
}

/// Findings from the tool's input, most serious first.
pub fn findings(input: &serde_json::Value) -> Vec<Finding> {
    let mut out: Vec<Finding> = input["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let message = f["message"].as_str()?.trim().to_string();
            (!message.is_empty()).then(|| Finding {
                file: f["file"].as_str().unwrap_or_default().trim().to_string(),
                line: f["line"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32,
                severity: Severity::parse(f["severity"].as_str().unwrap_or("")),
                message,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });
    out
}

pub fn request(model: String, summary: &str, diff: &str) -> StepRequest {
    StepRequest {
        model,
        system: SYSTEM.into(),
        turns: vec![Turn::User(format!("{summary}\n\n```diff\n{diff}\n```"))],
        tools: vec![tool()],
        max_tokens: 4096,
    }
}

/// Asks the model for its review of `diff`.
pub async fn review(
    endpoint: Endpoint,
    model: String,
    summary: String,
    diff: String,
) -> Result<Review, String> {
    let step = tools::step(endpoint, request(model, &summary, &diff)).await?;
    let mut found = Vec::new();
    let mut reported = false;
    for call in &step.calls {
        if call.name == "report_findings" {
            reported = true;
            found.extend(findings(&call.input));
        }
    }
    if !reported && step.text.is_empty() {
        return Err("The model gave no review".into());
    }
    Ok(Review {
        findings: found,
        note: step.text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_findings_most_serious_first() {
        let input = serde_json::json!({ "findings": [
            { "file": "b.rs", "line": 3, "severity": "note", "message": "Leftover println." },
            { "file": "a.rs", "line": 10, "severity": "BUG", "message": " Divides by zero. " },
            { "file": "a.rs", "severity": "warning", "message": "Unchecked index." },
            { "file": "c.rs", "severity": "bug", "message": "" },
        ]});
        let f = findings(&input);
        assert_eq!(f.len(), 3);
        assert_eq!(
            f[0],
            Finding {
                file: "a.rs".into(),
                line: 10,
                severity: Severity::Bug,
                message: "Divides by zero.".into()
            }
        );
        assert_eq!((f[1].severity, f[1].line), (Severity::Risk, 0));
        assert_eq!(f[2].severity.label(), "note");
        assert!(findings(&serde_json::json!({})).is_empty());
        let req = request("m".into(), "2 commits", "+x");
        assert_eq!(req.tools[0].name, "report_findings");
        assert!(matches!(&req.turns[0], Turn::User(t) if t.contains("```diff\n+x")));
    }
}
