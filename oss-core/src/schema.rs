use serde_json::{json, Value};

pub const TOOL_NAMES: &[&str] = &[
    "oss_search_repos",
    "oss_search_code",
    "oss_repo_profile",
    "oss_repo_tree",
    "oss_fetch_file",
    "oss_fetch_docs",
    "oss_guide",
];

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

pub fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "oss_search_repos",
            description: "Find open-source repositories by topic, name, or description. Returns a ranked shortlist with pre-computed fitness evidence (stars, license class, last release, archived flag, one-line what-it-is, one-line the-catch) — not a link dump. Lead with this for 'is there a tool for X' questions; use oss_search_code when the capability is an implementation detail no README advertises.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Topic or name search. Multi-phrasings allowed, ';' separated — unioned like two sweeps." },
                    "topic": { "type": "string", "description": "Exact topic tag (kebab-case), e.g. 'temporal-io'." },
                    "language": { "type": "string", "description": "Language constraint filter, e.g. 'TypeScript'." },
                    "min_stars": { "type": "integer", "minimum": 0 },
                    "sort": { "type": "string", "enum": ["fitness", "stars", "updated"], "default": "fitness", "description": "'fitness' = activity >= license-compat >= maturity >= stars; star-sort alone buries newcomers." },
                    "limit": { "type": "integer", "default": 10, "maximum": 30 },
                    "cursor": { "type": "string", "description": "Opaque cursor from a previous response's next_cursor." },
                    "fields": { "type": "array", "items": { "type": "string", "enum": ["what", "catch", "license", "recency", "stars", "urls"] }, "description": "Subset to return per row. Omitted fields still count toward total." },
                    "response_format": { "type": "string", "enum": ["concise", "detailed"], "default": "concise", "description": "concise = narration rows; detailed adds follow-up-call fields (catch, recency details, urls)." }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "oss_search_code",
            description: "Find repos by distinctive code idioms, not marketing. Fires 2-5 orthogonal literal or /regex/ probes, aggregates hits PER REPO, and ranks by probe breadth (repos hitting several independent idioms outrank any star count). Use for: implementations of an API idiom, error string, or config key; finding peers of a known-good repo (steal its identifiers as probes). Do NOT use to test whether a category exists — probing your own idioms finds only clones; lead with oss_search_repos instead.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "probes": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 5, "description": "Distinctive code fingerprints: function names, error strings, config keys, endpoints. '/.../' = regex (grep.app + Sourcegraph only). Search for code, not concepts: 'special_closes' not 'market calendars'; uniqueness beats correctness." },
                    "language": { "type": "string" },
                    "path": { "type": "string", "description": "Path substring filter, e.g. 'src/'." },
                    "repo_filter": { "type": "string", "description": "Restrict to org ('vercel/') or one repo ('owner/name')." },
                    "mode": { "type": "string", "enum": ["repos", "snippets"], "default": "repos", "description": "'repos' = per-repo aggregation with probe attribution; 'snippets' = match-centered snippets with path:Lx-Ly locators." },
                    "snippets_per_repo": { "type": "integer", "default": 2, "maximum": 5 },
                    "limit": { "type": "integer", "default": 15, "maximum": 40 },
                    "exclude_known": { "type": "string", "description": "Local corpus dir; marks repos already owned." },
                    "cursor": { "type": "string" },
                    "response_format": { "type": "string", "enum": ["concise", "detailed"], "default": "concise", "description": "concise = aggregation rows only; detailed adds sample evidence and snippet lines." }
                },
                "required": ["probes"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "oss_repo_profile",
            description: "Pre-digested brief for ONE known repo: metadata, computed health verdicts, license compatibility class, README digest (<=1200 chars), top-level architecture from the file tree, release cadence, runtime-support evidence. Use instead of 5+ raw metadata calls; use oss_fetch_docs / oss_fetch_file to drill in.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "owner/name; redirects re-resolved automatically on first 404." },
                    "deep_docs": { "type": "boolean", "default": false, "description": "Also digest llms.txt / llms-full.txt if published." }
                },
                "required": ["repo"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "oss_repo_tree",
            description: "Whole-repo file list in ONE call, path-filtered. ALWAYS call this before fetching any file whose path you are guessing — a 404 from a guessed path proves nothing. Response declares the total file count and returns up to `limit` paths with truncated/has_more set beyond.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string" },
                    "ref": { "type": "string", "description": "Branch/tag/sha; default = default branch." },
                    "path_prefix": { "type": "string", "description": "e.g. 'docs/' — only entries under this prefix." },
                    "glob": { "type": "string", "description": "Filename glob, e.g. '*.md'." },
                    "limit": { "type": "integer", "default": 200, "maximum": 500 }
                },
                "required": ["repo"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "oss_fetch_file",
            description: "Read one file's exact contents — byte-exact, never via a summarizer for opaque tokens, paths, or config keys. Prefer a line_range to keep payloads small; large files return the range plus total_lines.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string" },
                    "path": { "type": "string" },
                    "ref": { "type": "string" },
                    "line_range": { "type": "object", "properties": { "start": { "type": "integer", "minimum": 1 }, "end": { "type": "integer" } }, "required": ["start", "end"], "description": "1-based, inclusive; default = first 100 lines.", "additionalProperties": false }
                },
                "required": ["repo", "path"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "oss_fetch_docs",
            description: "Fetch a project's docs the reliable way: resolve llms.txt / llms-full.txt aggregates, then return search hits WITH their 'Source: <url>' breadcrumbs so each hit maps back to its page. Falls back to raw docs/*.md and flags redirect-stub pages. One aggregate fetch replaces a dozen lossy page fetches.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string" },
                    "pattern": { "type": "string", "description": "Text or /regex/ to extract; returns matching lines + breadcrumbs." },
                    "docs_host": { "type": "string", "description": "Override when docs live off-repo (e.g. learn.example.com)." }
                },
                "required": ["repo", "pattern"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "oss_guide",
            description: "Returns the probe-writing and query-syntax playbook on demand: good vs bad probes, the backend regex support matrix, rate-limit traps, license-class meanings, and which tool answers which question (discovery vs vetting vs drill-down). Call once when unsure which oss_ tool to use.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
    ]
}

pub fn schema_for(name: &str) -> Option<Value> {
    tool_specs()
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| s.input_schema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{
        FetchDocsParams, FetchFileParams, GuideParams, RepoProfileParams, RepoTreeParams,
        SearchCodeParams, SearchReposParams, ToolParams,
    };

    #[test]
    fn seven_tools_in_deterministic_order() {
        let specs = tool_specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name).collect();
        assert_eq!(names, TOOL_NAMES.to_vec());
    }

    #[test]
    fn schemas_declare_additional_properties_false() {
        for spec in tool_specs() {
            assert_eq!(
                spec.input_schema["additionalProperties"], false,
                "{}",
                spec.name
            );
        }
    }

    #[test]
    fn schema_properties_match_param_struct_fields() {
        let cases: Vec<(&str, Vec<&str>)> = vec![
            ("oss_search_repos", SearchReposParams::known_fields().to_vec()),
            ("oss_search_code", SearchCodeParams::known_fields().to_vec()),
            ("oss_repo_profile", RepoProfileParams::known_fields().to_vec()),
            ("oss_repo_tree", RepoTreeParams::known_fields().to_vec()),
            ("oss_fetch_file", FetchFileParams::known_fields().to_vec()),
            ("oss_fetch_docs", FetchDocsParams::known_fields().to_vec()),
            ("oss_guide", GuideParams::known_fields().to_vec()),
        ];
        for (name, fields) in cases {
            let schema = schema_for(name).unwrap();
            let props: Vec<String> = schema["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            let mut expected: Vec<String> = fields.into_iter().map(|f| f.to_string()).collect();
            expected.sort();
            let mut actual = props;
            actual.sort();
            assert_eq!(actual, expected, "schema drift for {name}");
        }
    }

    #[test]
    fn required_fields_declared() {
        assert_eq!(
            schema_for("oss_search_repos").unwrap()["required"],
            json!(["query"])
        );
        assert_eq!(
            schema_for("oss_search_code").unwrap()["required"],
            json!(["probes"])
        );
        assert_eq!(
            schema_for("oss_repo_profile").unwrap()["required"],
            json!(["repo"])
        );
        assert_eq!(
            schema_for("oss_repo_tree").unwrap()["required"],
            json!(["repo"])
        );
        assert_eq!(
            schema_for("oss_fetch_file").unwrap()["required"],
            json!(["repo", "path"])
        );
        assert_eq!(
            schema_for("oss_fetch_docs").unwrap()["required"],
            json!(["repo", "pattern"])
        );
    }
}
