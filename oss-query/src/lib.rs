use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum CaseMode {
    Sensitive,
    #[default]
    Insensitive,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ContentScope {
    #[default]
    Any,
    Definition,
    Test,
    Comment,
    StringLiteral,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum RepoScope {
    HotSet,
    Remote,
    #[default]
    Both,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PatternKind {
    #[default]
    Literal,
    Regex,
}


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub pattern: String,
    #[serde(default)]
    pub kind: PatternKind,
    #[serde(default)]
    pub case: CaseMode,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub exclude_repos: Vec<String>,
    #[serde(default)]
    pub langs: Vec<String>,
    #[serde(default)]
    pub exclude_langs: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    #[serde(default)]
    pub symbols: Vec<String>,
    #[serde(default)]
    pub content_scope: ContentScope,
    #[serde(default)]
    pub repo_scope: RepoScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    LocalIndex,
    GrepApp,
    GitHub,
    Sourcegraph,
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Backend::LocalIndex => "local_index",
            Backend::GrepApp => "grep_app",
            Backend::GitHub => "github",
            Backend::Sourcegraph => "sourcegraph",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackendPlan {
    pub backend: Backend,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryPlan {
    pub backends: Vec<BackendPlan>,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum QueryError {
    #[error("parse error at position {position} in `{input}`: {message}. Example: `retry repo:tokio-rs/tokio lang:rust`")]
    ParseError {
        input: String,
        position: usize,
        message: String,
        suggestion: String,
    },
    #[error("unknown field `{field}`; known fields: {}", known_fields.join(", "))]
    UnknownField {
        field: String,
        known_fields: Vec<String>,
    },
    #[error("invalid regex `{pattern}`: {error}. Only RE2-style syntax is supported (no backreferences or look-around)")]
    RegexError { pattern: String, error: String },
    #[error("pattern is not indexable: {reason}. Add a literal substring of at least 3 characters (e.g. `retry.*backoff`)")]
    NotIndexable { reason: String },
}

const KNOWN_TEXT_KEYS: &[&str] = &[
    "repo", "-repo", "lang", "-lang", "path", "-path", "symbol", "case", "scope", "in", "type",
];

pub const KNOWN_JSON_FIELDS: &[&str] = &[
    "pattern",
    "kind",
    "case",
    "repos",
    "exclude_repos",
    "langs",
    "exclude_langs",
    "paths",
    "exclude_paths",
    "symbols",
    "content_scope",
    "repo_scope",
];

impl Query {
    pub fn literal(pattern: impl Into<String>) -> Self {
        Query {
            pattern: pattern.into(),
            ..Default::default()
        }
    }

    pub fn validate(&self) -> Result<(), QueryError> {
        if self.pattern.is_empty() {
            return Err(QueryError::ParseError {
                input: self.pattern.clone(),
                position: 0,
                message: "pattern must not be empty".into(),
                suggestion: "provide a literal or regex pattern".into(),
            });
        }
        if self.kind == PatternKind::Regex {
            let anchored = if self.case == CaseMode::Insensitive {
                format!("(?i){}", self.pattern)
            } else {
                self.pattern.clone()
            };
            regex::Regex::new(&anchored).map_err(|e| QueryError::RegexError {
                pattern: self.pattern.clone(),
                error: e.to_string(),
            })?;
            if !literal_trigrams_ok(&self.pattern) {
                return Err(QueryError::NotIndexable {
                    reason: format!(
                        "regex `{}` has no literal substring of 3+ characters for trigram planning",
                        self.pattern
                    ),
                });
            }
        }
        Ok(())
    }

    pub fn to_text(&self) -> String {
        let mut parts = vec![self.pattern.clone()];
        if self.kind == PatternKind::Regex {
            parts.push("type:regex".into());
        }
        if self.case == CaseMode::Sensitive {
            parts.push("case:yes".into());
        }
        for r in &self.repos {
            parts.push(format!("repo:{r}"));
        }
        for r in &self.exclude_repos {
            parts.push(format!("-repo:{r}"));
        }
        for l in &self.langs {
            parts.push(format!("lang:{l}"));
        }
        for l in &self.exclude_langs {
            parts.push(format!("-lang:{l}"));
        }
        for p in &self.paths {
            parts.push(format!("path:{p}"));
        }
        for p in &self.exclude_paths {
            parts.push(format!("-path:{p}"));
        }
        for s in &self.symbols {
            parts.push(format!("symbol:{s}"));
        }
        match self.content_scope {
            ContentScope::Any => {}
            ContentScope::Definition => parts.push("scope:def".into()),
            ContentScope::Test => parts.push("scope:test".into()),
            ContentScope::Comment => parts.push("scope:comment".into()),
            ContentScope::StringLiteral => parts.push("scope:string".into()),
        }
        match self.repo_scope {
            RepoScope::Both => {}
            RepoScope::HotSet => parts.push("in:hotset".into()),
            RepoScope::Remote => parts.push("in:remote".into()),
        }
        parts.join(" ")
    }

    pub fn from_text(input: &str) -> Result<Query, QueryError> {
        let mut q = Query::default();
        let mut pattern_parts: Vec<String> = Vec::new();
        for (i, tok) in input.split_whitespace().enumerate() {
            if let Some((key, value)) = split_filter(tok) {
                let known: Vec<String> = KNOWN_TEXT_KEYS.iter().map(|s| s.to_string()).collect();
                match key.as_str() {
                    "repo" => q.repos.extend(split_values(&value)),
                    "-repo" => q.exclude_repos.extend(split_values(&value)),
                    "lang" => q.langs.extend(split_values(&value)),
                    "-lang" => q.exclude_langs.extend(split_values(&value)),
                    "path" => q.paths.extend(split_values(&value)),
                    "-path" => q.exclude_paths.extend(split_values(&value)),
                    "symbol" => q.symbols.extend(split_values(&value)),
                    "case" => {
                        q.case = match value.as_str() {
                            "yes" | "sensitive" | "true" => CaseMode::Sensitive,
                            "no" | "insensitive" | "false" => CaseMode::Insensitive,
                            _ => {
                                return Err(QueryError::ParseError {
                                    input: input.into(),
                                    position: i,
                                    message: format!("invalid case value `{value}`"),
                                    suggestion: "use case:yes or case:no".into(),
                                })
                            }
                        }
                    }
                    "scope" => {
                        q.content_scope = match value.as_str() {
                            "def" | "definition" => ContentScope::Definition,
                            "test" => ContentScope::Test,
                            "comment" => ContentScope::Comment,
                            "string" => ContentScope::StringLiteral,
                            "any" => ContentScope::Any,
                            _ => {
                                return Err(QueryError::ParseError {
                                    input: input.into(),
                                    position: i,
                                    message: format!("invalid scope value `{value}`"),
                                    suggestion: "use scope:def|test|comment|string|any".into(),
                                })
                            }
                        }
                    }
                    "in" => {
                        q.repo_scope = match value.as_str() {
                            "hotset" | "local" => RepoScope::HotSet,
                            "remote" => RepoScope::Remote,
                            "both" => RepoScope::Both,
                            _ => {
                                return Err(QueryError::ParseError {
                                    input: input.into(),
                                    position: i,
                                    message: format!("invalid in value `{value}`"),
                                    suggestion: "use in:hotset|remote|both".into(),
                                })
                            }
                        }
                    }
                    "type" => {
                        q.kind = match value.as_str() {
                            "regex" => PatternKind::Regex,
                            "literal" => PatternKind::Literal,
                            _ => {
                                return Err(QueryError::ParseError {
                                    input: input.into(),
                                    position: i,
                                    message: format!("invalid type value `{value}`"),
                                    suggestion: "use type:literal or type:regex".into(),
                                })
                            }
                        }
                    }
                    other => {
                        return Err(QueryError::UnknownField {
                            field: other.to_string(),
                            known_fields: known,
                        })
                    }
                }
            } else {
                pattern_parts.push(tok.to_string());
            }
        }
        q.pattern = pattern_parts.join(" ");
        Ok(q)
    }

    pub fn from_json(bytes: &str) -> Result<Query, QueryError> {
        let v: serde_json::Value =
            serde_json::from_str(bytes).map_err(|e| QueryError::ParseError {
                input: bytes.chars().take(200).collect(),
                position: 0,
                message: format!("invalid JSON: {e}"),
                suggestion: "pass a JSON object with a `pattern` string field".into(),
            })?;
        if let Some(obj) = v.as_object() {
            for k in obj.keys() {
                if !KNOWN_JSON_FIELDS.contains(&k.as_str()) {
                    return Err(QueryError::UnknownField {
                        field: k.clone(),
                        known_fields: KNOWN_JSON_FIELDS.iter().map(|s| s.to_string()).collect(),
                    });
                }
            }
        }
        serde_json::from_value(v).map_err(|e| QueryError::ParseError {
            input: bytes.chars().take(200).collect(),
            position: 0,
            message: e.to_string(),
            suggestion: "check field types: lists of strings for repos/langs/paths/symbols".into(),
        })
    }

    pub fn plan(&self) -> Result<QueryPlan, QueryError> {
        self.validate()?;
        let mut backends = Vec::new();
        if matches!(self.repo_scope, RepoScope::HotSet | RepoScope::Both) {
            backends.push(BackendPlan {
                backend: Backend::LocalIndex,
                warnings: vec![],
            });
        }
        if matches!(self.repo_scope, RepoScope::Remote | RepoScope::Both) {
            let mut grep_warnings = vec![];
            if !self.symbols.is_empty() {
                grep_warnings
                    .push("symbol filter not supported on grep.app; results unfiltered".into());
            }
            if self.content_scope != ContentScope::Any {
                grep_warnings
                    .push("content scope not supported on grep.app; results unfiltered".into());
            }
            if self.kind == PatternKind::Regex {
                grep_warnings
                    .push("grep.app applies substring semantics; regex verified client-side".into());
            }
            backends.push(BackendPlan {
                backend: Backend::GrepApp,
                warnings: grep_warnings,
            });

            let mut gh_warnings = vec![
                "github legacy API strips punctuation; no regex support server-side".to_string(),
            ];
            if self.kind == PatternKind::Regex {
                gh_warnings
                    .push("regex will be degraded to literal terms on github".into());
            }
            backends.push(BackendPlan {
                backend: Backend::GitHub,
                warnings: gh_warnings,
            });

            backends.push(BackendPlan {
                backend: Backend::Sourcegraph,
                warnings: vec![],
            });
        }
        Ok(QueryPlan { backends })
    }
}

fn split_filter(tok: &str) -> Option<(String, String)> {
    if let Some(rest) = tok.strip_prefix("-repo:") {
        return Some(("-repo".into(), rest.into()));
    }
    if let Some(rest) = tok.strip_prefix("-lang:") {
        return Some(("-lang".into(), rest.into()));
    }
    if let Some(rest) = tok.strip_prefix("-path:") {
        return Some(("-path".into(), rest.into()));
    }
    let idx = tok.find(':')?;
    let (key, value) = tok.split_at(idx);
    let value = &value[1..];
    let _ = KNOWN_TEXT_KEYS;
    Some((key.to_string(), value.to_string()))
}

fn split_values(value: &str) -> Vec<String> {
    value
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

pub fn literal_trigrams_ok(pattern: &str) -> bool {
    let mut run = 0usize;
    let mut chars = pattern.chars().peekable();
    let mut max_run = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(&next) = chars.peek() {
                    chars.next();
                    match next {
                        'd' | 's' | 'w' | 'b' | 'D' | 'S' | 'W' | 'B' | 'n' | 't' | 'r' | '0' => {
                            max_run = max_run.max(run);
                            run = 0;
                        }
                        _ => {
                            run += 1;
                        }
                    }
                }
            }
            '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' => {
                if c == '[' {
                    let mut depth = 1;
                    while let Some(c2) = chars.next() {
                        match c2 {
                            '[' => depth += 1,
                            ']' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            '\\' => {
                                chars.next();
                            }
                            _ => {}
                        }
                    }
                }
                max_run = max_run.max(run);
                run = 0;
            }
            _ => {
                run += 1;
            }
        }
    }
    max_run = max_run.max(run);
    max_run >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(q: Query) {
        let text = q.to_text();
        let back = Query::from_text(&text).expect(&text);
        assert_eq!(q, back, "round trip via `{text}`");
    }

    #[test]
    fn roundtrip_bare_pattern() {
        rt(Query::literal("retry"));
    }

    #[test]
    fn roundtrip_all_filters() {
        rt(Query {
            pattern: "backoff".into(),
            kind: PatternKind::Regex,
            case: CaseMode::Sensitive,
            repos: vec!["tokio-rs/tokio".into()],
            exclude_repos: vec!["foo/bar".into()],
            langs: vec!["rust".into()],
            exclude_langs: vec!["go".into()],
            paths: vec!["src/".into()],
            exclude_paths: vec!["tests/".into()],
            symbols: vec!["spawn".into()],
            content_scope: ContentScope::Definition,
            repo_scope: RepoScope::HotSet,
        });
    }

    #[test]
    fn roundtrip_each_scope() {
        for s in [
            ContentScope::Any,
            ContentScope::Definition,
            ContentScope::Test,
            ContentScope::Comment,
            ContentScope::StringLiteral,
        ] {
            rt(Query {
                pattern: "x".into(),
                content_scope: s,
                ..Default::default()
            });
        }
    }

    #[test]
    fn roundtrip_repo_scope() {
        for s in [RepoScope::Both, RepoScope::HotSet, RepoScope::Remote] {
            rt(Query {
                pattern: "x".into(),
                repo_scope: s,
                ..Default::default()
            });
        }
    }

    #[test]
    fn roundtrip_case_modes() {
        rt(Query {
            pattern: "x".into(),
            case: CaseMode::Sensitive,
            ..Default::default()
        });
        rt(Query {
            pattern: "x".into(),
            case: CaseMode::Insensitive,
            ..Default::default()
        });
    }

    #[test]
    fn roundtrip_multi_value_commas() {
        rt(Query {
            pattern: "x".into(),
            langs: vec!["rust".into(), "go".into(), "python".into()],
            ..Default::default()
        });
    }

    #[test]
    fn roundtrip_regex_kind() {
        rt(Query {
            pattern: "foo.*bar".into(),
            kind: PatternKind::Regex,
            ..Default::default()
        });
    }

    #[test]
    fn roundtrip_multi_word_pattern() {
        rt(Query::literal("hello world"));
    }

    #[test]
    fn roundtrip_excludes_only() {
        rt(Query {
            pattern: "x".into(),
            exclude_repos: vec!["a/b".into()],
            exclude_langs: vec!["c".into()],
            exclude_paths: vec!["gen/".into()],
            ..Default::default()
        });
    }

    #[test]
    fn roundtrip_symbols() {
        rt(Query {
            pattern: "x".into(),
            symbols: vec!["main".into(), "init".into()],
            ..Default::default()
        });
    }

    #[test]
    fn parse_text_basic() {
        let q = Query::from_text("retry repo:tokio-rs/tokio lang:rust").unwrap();
        assert_eq!(q.pattern, "retry");
        assert_eq!(q.repos, vec!["tokio-rs/tokio"]);
        assert_eq!(q.langs, vec!["rust"]);
    }

    #[test]
    fn parse_text_negations() {
        let q = Query::from_text("retry -repo:bad/repo -lang:java -path:target/").unwrap();
        assert_eq!(q.exclude_repos, vec!["bad/repo"]);
        assert_eq!(q.exclude_langs, vec!["java"]);
        assert_eq!(q.exclude_paths, vec!["target/"]);
    }

    #[test]
    fn parse_text_case_and_scope() {
        let q = Query::from_text("foo case:yes scope:def in:hotset type:regex").unwrap();
        assert_eq!(q.case, CaseMode::Sensitive);
        assert_eq!(q.content_scope, ContentScope::Definition);
        assert_eq!(q.repo_scope, RepoScope::HotSet);
        assert_eq!(q.kind, PatternKind::Regex);
    }

    #[test]
    fn parse_unknown_key_is_typed_error() {
        let err = Query::from_text("retry branch:main").unwrap_err();
        match err {
            QueryError::UnknownField { field, known_fields } => {
                assert_eq!(field, "branch");
                assert!(known_fields.contains(&"repo".to_string()));
            }
            other => panic!("expected UnknownField, got {other:?}"),
        }
    }

    #[test]
    fn parse_invalid_case_value() {
        let err = Query::from_text("x case:maybe").unwrap_err();
        assert!(matches!(err, QueryError::ParseError { .. }));
    }

    #[test]
    fn parse_invalid_scope_value() {
        assert!(Query::from_text("x scope:banana").is_err());
    }

    #[test]
    fn parse_invalid_in_value() {
        assert!(Query::from_text("x in:nowhere").is_err());
    }

    #[test]
    fn parse_invalid_type_value() {
        assert!(Query::from_text("x type:fuzzy").is_err());
    }

    #[test]
    fn json_roundtrip() {
        let q = Query {
            pattern: "retry".into(),
            langs: vec!["rust".into()],
            ..Default::default()
        };
        let s = serde_json::to_string(&q).unwrap();
        let back = Query::from_json(&s).unwrap();
        assert_eq!(q, back);
    }

    #[test]
    fn json_unknown_field() {
        let err = Query::from_json(r#"{"pattern":"x","brnach":"main"}"#).unwrap_err();
        match err {
            QueryError::UnknownField { field, .. } => assert_eq!(field, "brnach"),
            other => panic!("expected UnknownField, got {other:?}"),
        }
    }

    #[test]
    fn json_bad_json() {
        assert!(matches!(
            Query::from_json("{not json"),
            Err(QueryError::ParseError { .. })
        ));
    }

    #[test]
    fn validate_empty_pattern() {
        assert!(matches!(
            Query::literal("").validate(),
            Err(QueryError::ParseError { .. })
        ));
    }

    #[test]
    fn validate_bad_regex() {
        let q = Query {
            pattern: "foo(".into(),
            kind: PatternKind::Regex,
            ..Default::default()
        };
        assert!(matches!(
            q.validate(),
            Err(QueryError::RegexError { .. })
        ));
    }

    #[test]
    fn validate_backreference_rejected() {
        let q = Query {
            pattern: r"(a)\1bc".into(),
            kind: PatternKind::Regex,
            ..Default::default()
        };
        assert!(matches!(
            q.validate(),
            Err(QueryError::RegexError { .. })
        ));
    }

    #[test]
    fn validate_regex_without_literal_not_indexable() {
        let q = Query {
            pattern: "[a-z]+".into(),
            kind: PatternKind::Regex,
            ..Default::default()
        };
        assert!(matches!(
            q.validate(),
            Err(QueryError::NotIndexable { .. })
        ));
    }

    #[test]
    fn validate_regex_with_literal_ok() {
        let q = Query {
            pattern: "retry.*backoff".into(),
            kind: PatternKind::Regex,
            ..Default::default()
        };
        assert!(q.validate().is_ok());
    }

    #[test]
    fn trigrams_ok_cases() {
        assert!(literal_trigrams_ok("abc"));
        assert!(literal_trigrams_ok("foo.*"));
        assert!(!literal_trigrams_ok(".*"));
        assert!(!literal_trigrams_ok("[a-z]+"));
        assert!(!literal_trigrams_ok("a.*b"));
        assert!(literal_trigrams_ok("a.*bcd"));
        assert!(!literal_trigrams_ok(r"\d\d\d"));
        assert!(literal_trigrams_ok(r"foo\.bar"));
    }

    #[test]
    fn plan_both_scope_all_backends() {
        let q = Query::literal("retry");
        let plan = q.plan().unwrap();
        let names: Vec<Backend> = plan.backends.iter().map(|b| b.backend).collect();
        assert_eq!(
            names,
            vec![
                Backend::LocalIndex,
                Backend::GrepApp,
                Backend::GitHub,
                Backend::Sourcegraph
            ]
        );
    }

    #[test]
    fn plan_hotset_only_local() {
        let q = Query {
            pattern: "x".into(),
            repo_scope: RepoScope::HotSet,
            ..Default::default()
        };
        let plan = q.plan().unwrap();
        assert_eq!(plan.backends.len(), 1);
        assert_eq!(plan.backends[0].backend, Backend::LocalIndex);
    }

    #[test]
    fn plan_remote_excludes_local() {
        let q = Query {
            pattern: "x".into(),
            repo_scope: RepoScope::Remote,
            ..Default::default()
        };
        let plan = q.plan().unwrap();
        assert!(!plan.backends.iter().any(|b| b.backend == Backend::LocalIndex));
        assert_eq!(plan.backends.len(), 3);
    }

    #[test]
    fn plan_symbol_filter_warns_on_grepapp() {
        let q = Query {
            pattern: "x".into(),
            symbols: vec!["main".into()],
            ..Default::default()
        };
        let plan = q.plan().unwrap();
        let grep = plan
            .backends
            .iter()
            .find(|b| b.backend == Backend::GrepApp)
            .unwrap();
        assert!(grep.warnings.iter().any(|w| w.contains("symbol")));
    }

    #[test]
    fn plan_regex_warns_on_github() {
        let q = Query {
            pattern: "foo.*bar".into(),
            kind: PatternKind::Regex,
            ..Default::default()
        };
        let plan = q.plan().unwrap();
        let gh = plan
            .backends
            .iter()
            .find(|b| b.backend == Backend::GitHub)
            .unwrap();
        assert!(!gh.warnings.is_empty());
    }

    #[test]
    fn error_display_echoes_input() {
        let err = QueryError::UnknownField {
            field: "brnach".into(),
            known_fields: vec!["repo".into()],
        };
        let s = err.to_string();
        assert!(s.contains("brnach"));
        assert!(s.contains("repo"));
    }

    #[test]
    fn text_serialization_stable() {
        let text = "retry case:yes repo:a/b lang:rust scope:test in:remote";
        let q = Query::from_text(text).unwrap();
        assert_eq!(q.to_text(), text);
    }
}
