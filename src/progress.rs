//! Live output for agent runs.
//!
//! A run makes many slow model calls and edits files you care about, so it
//! narrates itself as it goes. Everything goes to stderr, leaving stdout for the
//! result.

use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::Value;
use std::io::{IsTerminal, Write};

pub struct Reporter {
    enabled: bool,
    colour: bool,
}

impl Reporter {
    pub fn new() -> Self {
        Self {
            enabled: true,
            colour: std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    /// Prints nothing.
    pub fn silent() -> Self {
        Self {
            enabled: false,
            colour: false,
        }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.colour {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn line(&self, text: &str) {
        let mut stderr = std::io::stderr();
        let _ = writeln!(stderr, "{text}");
        let _ = stderr.flush();
    }

    pub fn started(
        &self,
        action: &str,
        current: usize,
        total: usize,
        revision: &str,
        message: &str,
    ) {
        if !self.enabled {
            return;
        }
        self.line(&format!(
            "{} {revision} {message} ({current}/{total})",
            self.paint("1;36", action),
        ));
    }

    /// Printed before the call, since that is where the seconds go.
    pub fn thinking(&self, turn: usize) {
        if !self.enabled {
            return;
        }
        self.line(&format!(
            "\n{} {}",
            self.paint("2", &format!("[{turn}]")),
            self.paint("2", "thinking…")
        ));
    }

    pub fn model_text(&self, text: &str) {
        if !self.enabled || text.trim().is_empty() {
            return;
        }
        for line in text.trim().lines() {
            self.line(&format!("    {}", self.paint("2;3", line)));
        }
    }

    pub fn tool_call(&self, call: &CallToolRequestParams) {
        if !self.enabled {
            return;
        }
        let arguments = call
            .arguments
            .as_ref()
            .map(Clone::clone)
            .map(Value::Object)
            .unwrap_or(Value::Null);
        self.line(&format!(
            "  {} {}{}",
            self.paint("1;34", "→"),
            self.paint("1", &call.name.to_string()),
            self.paint(
                "36",
                &format!(
                    " {}",
                    serde_json::to_string(&arguments)
                        .unwrap_or_else(|error| format!("error: {error:#}"))
                )
            )
        ));
    }

    pub fn tool_result(&self, result: &CallToolResult) {
        if !self.enabled {
            return;
        }
        let output = if let Some(ref structured) = result.structured_content {
            serde_json::to_string_pretty(structured)
                .unwrap_or_else(|error| format!("error: {error:#}"))
        } else {
            let blocks: Vec<_> = result
                .content
                .iter()
                .filter_map(|c| c.as_text().map(|text| text.text.clone()))
                .collect();
            blocks.join("")
        };
        // Errors print in full: watching the agent correct itself is the most
        // useful thing on screen when a run misbehaves.
        let (code, lines) = match result.is_error {
            Some(true) => ("1;31", output.trim().lines().collect::<Vec<_>>()),
            _ => ("2", output.trim().lines().take(3).collect::<Vec<_>>()),
        };
        let total = output.trim().lines().count();
        for line in &lines {
            self.line(&format!("    {}", self.paint(code, &truncate(line, 100))));
        }
        if total > lines.len() {
            self.line(&format!(
                "    {}",
                self.paint("2", &format!("… {} more lines", total - lines.len()))
            ));
        }
    }

    pub fn finished(&self, summary: &str) {
        if !self.enabled || summary.trim().is_empty() {
            return;
        }
        self.line(&format!(
            "  {} {}\n",
            self.paint("1;32", "✓"),
            self.paint("32", summary)
        ));
    }

    pub fn warn(&self, text: &str) {
        if !self.enabled {
            return;
        }
        self.line(&format!("  {}", self.paint("33", text)));
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    format!("{}…", text.chars().take(limit).collect::<String>())
}
