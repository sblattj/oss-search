use serde::Deserialize;

use crate::error::{FacadeError, Result};
use crate::http::{build_url, encode_segment, Http};
use crate::types::{BackendResponse, Capabilities, Filters, Hit, Status};
use crate::BackendClient;

pub const DEPS_DEV_BASE: &str = "https://api.deps.dev";

#[derive(Clone)]
pub struct DepsDevClient {
    http: Http,
    base: String,
}

impl Default for DepsDevClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DepsDevClient {
    pub fn new() -> Self {
        Self::with_base(DEPS_DEV_BASE)
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            http: Http::new(),
            base: base.into(),
        }
    }

    pub async fn package_metadata(
        &self,
        system: &str,
        name: &str,
    ) -> Result<Vec<DepsVersionInfo>> {
        let url = build_url(
            &self.base,
            &format!(
                "/v3/systems/{}/packages/{}",
                encode_segment(&system.to_ascii_lowercase()),
                encode_segment(name)
            ),
            &[],
        )?;
        let resp = self.handle_rate_limit(self.http.get(&url, &[]).await)?;
        if resp.status().as_u16() == 404 {
            return Err(FacadeError::DepsDev(format!(
                "package {system}/{name} not found"
            )));
        }
        if !resp.status().is_success() {
            return Err(FacadeError::DepsDev(format!(
                "package metadata request failed with status {}",
                resp.status()
            )));
        }
        let raw: RawPackage = resp
            .json()
            .await
            .map_err(|e| FacadeError::Decode {
                backend: "deps.dev",
                detail: e.to_string(),
            })?;
        Ok(raw.into_versions())
    }

    pub async fn version_details(
        &self,
        system: &str,
        name: &str,
        version: &str,
    ) -> Result<DepsVersionDetails> {
        let url = build_url(
            &self.base,
            &format!(
                "/v3/systems/{}/packages/{}/versions/{}",
                encode_segment(&system.to_ascii_lowercase()),
                encode_segment(name),
                encode_segment(version)
            ),
            &[],
        )?;
        let resp = self.handle_rate_limit(self.http.get(&url, &[]).await)?;
        if resp.status().as_u16() == 404 {
            return Err(FacadeError::DepsDev(format!(
                "version {system}/{name}@{version} not found"
            )));
        }
        if !resp.status().is_success() {
            return Err(FacadeError::DepsDev(format!(
                "version details request failed with status {}",
                resp.status()
            )));
        }
        let raw: RawVersionDetails = resp
            .json()
            .await
            .map_err(|e| FacadeError::Decode {
                backend: "deps.dev",
                detail: e.to_string(),
            })?;
        Ok(DepsVersionDetails {
            version_key: raw
                .version_key
                .map(|k| format!("{}/{}/{}", k.system, k.name, k.version))
                .unwrap_or_default(),
            is_default: raw.is_default,
            is_deprecated: raw.is_deprecated,
            licenses: raw.licenses,
            advisory_keys: raw
                .advisory_keys
                .into_iter()
                .map(|k| k.id)
                .collect::<Vec<_>>(),
        })
    }

    pub async fn advisory(&self, id: &str) -> Result<DepsAdvisory> {
        let url = build_url(&self.base, &format!("/v3/advisories/{}", encode_segment(id)), &[])?;
        let resp = self.handle_rate_limit(self.http.get(&url, &[]).await)?;
        if resp.status().as_u16() == 404 {
            return Err(FacadeError::DepsDev(format!("advisory {id} not found")));
        }
        if !resp.status().is_success() {
            return Err(FacadeError::DepsDev(format!(
                "advisory request failed with status {}",
                resp.status()
            )));
        }
        let raw: RawAdvisory = resp
            .json()
            .await
            .map_err(|e| FacadeError::Decode {
                backend: "deps.dev",
                detail: e.to_string(),
            })?;
        Ok(DepsAdvisory {
            id: raw.id.unwrap_or_else(|| id.to_string()),
            title: raw.title,
            aliases: raw.aliases,
            cvss3_score: raw.cvss3.and_then(|c| c.score),
            url: raw.url,
        })
    }

    fn handle_rate_limit(
        &self,
        result: Result<reqwest::Response>,
    ) -> Result<reqwest::Response> {
        match result {
            Err(FacadeError::RateLimited { retry_after }) => Err(FacadeError::DepsDev(format!(
                "deps.dev rate limited (429, retry_after={retry_after:?}); cache and back off"
            ))),
            other => other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepsVersionInfo {
    pub system: String,
    pub name: String,
    pub version: String,
    pub is_default: bool,
    pub is_deprecated: bool,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepsVersionDetails {
    pub version_key: String,
    pub is_default: Option<bool>,
    pub is_deprecated: Option<bool>,
    pub licenses: Vec<String>,
    pub advisory_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DepsAdvisory {
    pub id: String,
    pub title: Option<String>,
    pub aliases: Vec<String>,
    pub cvss3_score: Option<f64>,
    pub url: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPackage {
    #[serde(alias = "packageVersions")]
    package_versions: Vec<RawPackageVersion>,
    versions: Vec<RawPackageVersion>,
}

impl RawPackage {
    fn into_versions(self) -> Vec<DepsVersionInfo> {
        let mut all = self.package_versions;
        all.extend(self.versions);
        all.into_iter()
            .map(|v| DepsVersionInfo {
                system: v.version_key.system,
                name: v.version_key.name,
                version: v.version_key.version,
                is_default: v.is_default.unwrap_or(false),
                is_deprecated: v.is_deprecated.unwrap_or(false),
                published_at: v.published_at,
            })
            .collect()
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawPackageVersion {
    version_key: RawVersionKey,
    is_default: Option<bool>,
    is_deprecated: Option<bool>,
    published_at: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawVersionKey {
    system: String,
    name: String,
    version: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawVersionDetails {
    version_key: Option<RawVersionKey>,
    is_default: Option<bool>,
    is_deprecated: Option<bool>,
    licenses: Vec<String>,
    advisory_keys: Vec<RawAdvisoryKey>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawAdvisoryKey {
    id: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawAdvisory {
    id: Option<String>,
    title: Option<String>,
    aliases: Vec<String>,
    cvss3: Option<RawCvss3>,
    url: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCvss3 {
    score: Option<f64>,
}

fn system_for_language(lang: &str) -> Option<&'static str> {
    match lang.to_ascii_lowercase().as_str() {
        "rust" | "cargo" => Some("cargo"),
        "javascript" | "js" | "typescript" | "ts" | "npm" => Some("npm"),
        "python" | "py" | "pypi" => Some("pypi"),
        "go" | "golang" => Some("go"),
        "java" | "jvm" | "maven" => Some("maven"),
        "ruby" | "rubygems" => Some("rubygems"),
        "c#" | "csharp" | "nuget" | ".net" => Some("nuget"),
        _ => None,
    }
}

fn parse_system_query(query: &str) -> (String, String) {
    if let Some((system, name)) = query.split_once(':')
        && !system.is_empty()
        && !name.is_empty()
    {
        return (system.to_ascii_lowercase(), name.to_string());
    }
    ("npm".to_string(), query.to_string())
}

#[async_trait::async_trait]
impl BackendClient for DepsDevClient {
    async fn search_code(&self, query: &str, filters: &Filters) -> Result<BackendResponse> {
        let start = tokio::time::Instant::now();
        let (system, name) = parse_system_query(query);
        let system = filters
            .language
            .as_deref()
            .and_then(system_for_language)
            .map(str::to_string)
            .unwrap_or(system);
        let versions = match self.package_metadata(&system, &name).await {
            Ok(versions) => versions,
            Err(FacadeError::DepsDev(d)) if d.contains("not found") => Vec::new(),
            Err(e) => return Err(e),
        };
        let limit = filters.limit.unwrap_or(10).min(100) as usize;
        let shown: Vec<Hit> = versions
            .iter()
            .take(limit)
            .map(|v| {
                let mut snippet = format!("{}/{}", v.system, v.name);
                if v.is_default {
                    snippet.push_str(" (default)");
                }
                if v.is_deprecated {
                    snippet.push_str(" [deprecated]");
                }
                if let Some(published) = &v.published_at {
                    snippet.push_str(&format!(" published {published}"));
                }
                Hit {
                    backend: "deps.dev",
                    repo: None,
                    path: None,
                    url: None,
                    line_ranges: Vec::new(),
                    snippet: format!("{snippet} @ {}", v.version),
                    license: None,
                    score: None,
                }
            })
            .collect();
        let total = versions.len() as u64;
        let has_more = (shown.len() as u64) < total;
        let mut warnings = Vec::new();
        if has_more {
            warnings.push(format!(
                "deps.dev returned {total} versions; showing {}; this is an exact-name package lookup, not a code search",
                shown.len()
            ));
        }
        Ok(BackendResponse {
            hits: shown,
            total: Some(total),
            has_more,
            partial: false,
            status: Status::Ok,
            warnings,
            elapsed: start.elapsed(),
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "deps.dev",
            regex: false,
            language_filter: false,
            repo_filter: false,
            org_filter: false,
            path_filter: false,
            max_results: Some(1),
            rate_limit_per_minute: None,
            auth_required: false,
            line_numbers: false,
            notes: &[
                "exact package/version/advisory lookup across npm, cargo, go, maven, pypi, nuget, rubygems",
                "search_code treats the query as an exact package name (system:name or bare name defaults to npm)",
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn maps_languages_to_systems() {
        assert_eq!(system_for_language("Rust"), Some("cargo"));
        assert_eq!(system_for_language("typescript"), Some("npm"));
        assert_eq!(system_for_language("COBOL"), None);
    }

    #[test]
    fn parses_system_qualified_queries() {
        assert_eq!(
            parse_system_query("cargo:serde"),
            ("cargo".to_string(), "serde".to_string())
        );
        assert_eq!(
            parse_system_query("@scope/pkg"),
            ("npm".to_string(), "@scope/pkg".to_string())
        );
    }

    #[tokio::test]
    async fn package_metadata_parses_and_tolerates_both_shapes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/systems/npm/packages/react"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "packageVersions": [{
                    "versionKey": { "system": "NPM", "name": "react", "version": "19.0.0" },
                    "isDefault": true,
                    "publishedAt": "2024-12-05T00:00:00Z"
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v3/systems/cargo/packages/serde"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "versions": [{
                    "versionKey": { "system": "CARGO", "name": "serde", "version": "1.0.210" },
                    "isDefault": true
                }]
            })))
            .mount(&server)
            .await;
        let client = DepsDevClient::with_base(server.uri());
        let versions = client.package_metadata("npm", "react").await.unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "19.0.0");
        assert!(versions[0].is_default);
        let versions = client.package_metadata("cargo", "serde").await.unwrap();
        assert_eq!(versions[0].system, "CARGO");
    }

    #[tokio::test]
    async fn version_details_and_advisories_parse() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/systems/npm/packages/react/versions/19.0.0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "versionKey": { "system": "NPM", "name": "react", "version": "19.0.0" },
                "isDefault": true,
                "licenses": ["MIT"],
                "advisoryKeys": [{ "id": "GHSA-xxxx-yyyy-zzzz" }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v3/advisories/GHSA-xxxx-yyyy-zzzz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "GHSA-xxxx-yyyy-zzzz",
                "title": "React XSS",
                "aliases": ["CVE-2024-1234"],
                "cvss3": { "score": 7.5, "vector": "AV:N/AC:L" },
                "url": "https://osv.dev/vulnerability/GHSA-xxxx-yyyy-zzzz"
            })))
            .mount(&server)
            .await;
        let client = DepsDevClient::with_base(server.uri());
        let details = client
            .version_details("npm", "react", "19.0.0")
            .await
            .unwrap();
        assert_eq!(details.licenses, vec!["MIT"]);
        assert_eq!(details.advisory_keys, vec!["GHSA-xxxx-yyyy-zzzz"]);
        let advisory = client.advisory("GHSA-xxxx-yyyy-zzzz").await.unwrap();
        assert_eq!(advisory.cvss3_score, Some(7.5));
        assert_eq!(advisory.aliases, vec!["CVE-2024-1234"]);
    }

    #[tokio::test]
    async fn retries_5xx_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/systems/npm/packages/react"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "packageVersions": [{
                    "versionKey": { "system": "NPM", "name": "react", "version": "19.0.0" }
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v3/systems/npm/packages/react"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        let client = DepsDevClient::with_base(server.uri());
        let versions = client.package_metadata("npm", "react").await.unwrap();
        assert_eq!(versions.len(), 1);
    }

    #[tokio::test]
    async fn search_code_unknown_package_is_ok_with_zero_hits() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/systems/npm/packages/nope-missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = DepsDevClient::with_base(server.uri());
        let resp = client
            .search_code("nope-missing", &Filters::default())
            .await
            .unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert!(resp.hits.is_empty());
    }
}
