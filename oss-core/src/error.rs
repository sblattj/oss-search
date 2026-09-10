use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    UnknownField {
        field: String,
        known: Vec<String>,
        input: Value,
    },
    MissingField {
        field: String,
        known: Vec<String>,
        input: Value,
    },
    InvalidValue {
        field: String,
        message: String,
        input: Value,
        correction: String,
    },
    UnknownTool {
        tool: String,
        known: Vec<String>,
    },
    NotFound {
        kind: &'static str,
        name: String,
        input: Value,
        correction: String,
    },
    Engine {
        message: String,
    },
}

impl ToolError {
    pub fn code(&self) -> &'static str {
        match self {
            ToolError::UnknownField { .. } => "unknown_field",
            ToolError::MissingField { .. } => "missing_field",
            ToolError::InvalidValue { .. } => "invalid_value",
            ToolError::UnknownTool { .. } => "unknown_tool",
            ToolError::NotFound { .. } => "not_found",
            ToolError::Engine { .. } => "engine_error",
        }
    }

    pub fn message(&self) -> String {
        match self {
            ToolError::UnknownField { field, known, .. } => format!(
                "unknown field `{field}` for this tool; known fields are {}",
                known
                    .iter()
                    .map(|f| format!("`{f}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ToolError::MissingField { field, known, .. } => format!(
                "missing required field `{field}`; known fields are {}",
                known
                    .iter()
                    .map(|f| format!("`{f}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ToolError::InvalidValue { message, .. } => message.clone(),
            ToolError::UnknownTool { tool, known } => format!(
                "unknown tool `{tool}`; known tools are {}",
                known.join(", ")
            ),
            ToolError::NotFound { kind, name, .. } => format!("{kind} `{name}` not found"),
            ToolError::Engine { message } => format!("engine failure: {message}"),
        }
    }

    pub fn input_echo(&self) -> Value {
        match self {
            ToolError::UnknownField { input, .. }
            | ToolError::MissingField { input, .. }
            | ToolError::InvalidValue { input, .. }
            | ToolError::NotFound { input, .. } => input.clone(),
            ToolError::UnknownTool { .. } | ToolError::Engine { .. } => Value::Null,
        }
    }

    pub fn known_fields(&self) -> Value {
        match self {
            ToolError::UnknownField { known, .. } | ToolError::MissingField { known, .. } => {
                serde_json::to_value(known).unwrap_or(Value::Null)
            }
            _ => Value::Null,
        }
    }

    pub fn correction(&self) -> Value {
        let s: String = match self {
            ToolError::UnknownField { field, .. } => format!(
                "remove `{field}` or rename it to one of the known fields; see oss_guide for the query dialect"
            ),
            ToolError::MissingField { field, .. } => format!(
                "add required field `{field}`; example: {example}",
                example = example_for(field)
            ),
            ToolError::InvalidValue { correction, .. } => correction.clone(),
            ToolError::UnknownTool { known, .. } => format!(
                "call one of: {}; oss_guide explains which tool answers which question",
                known.join(", ")
            ),
            ToolError::NotFound { correction, .. } => correction.clone(),
            ToolError::Engine { .. } => {
                "retry the call; if it persists, narrow the query or see oss_guide".to_string()
            }
        };
        Value::String(s)
    }

    pub fn to_json(&self) -> Value {
        let mut v = serde_json::json!({
            "error": {
                "code": self.code(),
                "message": self.message(),
                "input_echo": self.input_echo(),
                "known_fields": self.known_fields(),
                "correction": self.correction(),
            }
        });
        // Error responses must meet the same output-hygiene bar as success
        // responses: attacker-controlled input is echoed in message/
        // input_echo, so strip control chars and escape bidi overrides here
        // too. This is the only path errors take to the wire.
        crate::shaping::sanitize_value(&mut v, &mut crate::shaping::SanitizeReport::default());
        v
    }
}

fn example_for(field: &str) -> &'static str {
    match field {
        "query" => "{\"query\": \"rust connection pool\"}",
        "probes" => "{\"probes\": [\"spawn_worker\", \"/early[Cc]loses/\"]}",
        "repo" => "{\"repo\": \"ferrum/poolite\"}",
        "path" => "{\"repo\": \"ferrum/poolite\", \"path\": \"src/pool.rs\"}",
        "pattern" => "{\"repo\": \"gabe/hashopen\", \"pattern\": \"early close\"}",
        _ => "{}",
    }
}
