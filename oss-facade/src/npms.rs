use serde::Deserialize;

use crate::error::{FacadeError, Result};
use crate::http::{build_url, encode_segment, Http};
use crate::types::{BackendResponse, Capabilities, Filters, Hit, Status};
use crate::BackendClient;

pub const NPMS_BASE: &str = "https://api.npms.io";
pub const NPMS_MAX_SIZE: u32 = 250;
pub const NPMS_MAX_FROM: u32 = 10_000;
const STALENESS_WARNING_DAYS: i64 = 90;

#[derive(Clone)]
pub struct NpmsClient {
    http: Http,
    base: String,
}

impl Default for NpmsClient {
    fn default() -> Self {
        Self::new()
    }
}

impl NpmsClient {
    pub fn new() -> Self {
        Self::with_base(NPMS_BASE)
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            http: Http::new(),
            base: base.into(),
        }
    }

    pub async fn package_scores(&self, name: &str) -> Result<NpmsPackageScore> {
        let url = build_url(
            &self.base,
            &format!("/v2/package/{}", encode_segment(name)),
            &[],
        )?;
        let resp = match self.http.get(&url, &[]).await {
            Ok(resp) => resp,
            Err(FacadeError::RateLimited { retry_after }) => {
                return Err(FacadeError::Npms(format!(
                    "npms.io rate limited (429, retry_after={retry_after:?})"
                )));
            }
            Err(e) => return Err(e),
        };
        if resp.status().as_u16() == 404 {
            return Err(FacadeError::Npms(format!("package {name} not found")));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(FacadeError::Npms(format!(
                "package score request failed with status {status}: {}",
                &body[..body.len().min(200)]
            )));
        }
        let raw: RawPackageScore = resp
            .json()
            .await
            .map_err(|e| FacadeError::Decode {
                backend: "npms.io",
                detail: e.to_string(),
            })?;
        let mut warnings = Vec::new();
        if let Some(analyzed_at) = &raw.analyzed_at {
            if let Some(days) = days_old(analyzed_at) {
                if days > STALENESS_WARNING_DAYS {
                    warnings.push(format!(
                        "npms.io analysis of {name} is {days} days old (analyzedAt {analyzed_at}); scores are a cold-start prior and may be stale"
                    ));
                }
            } else {
                warnings.push(format!(
                    "could not parse npms.io analyzedAt timestamp {analyzed_at:?}"
                ));
            }
        } else {
            warnings.push(format!(
                "npms.io response for {name} has no analyzedAt field; scores may be stale"
            ));
        }
        let metadata = raw.collected.and_then(|c| c.metadata);
        let name = metadata
            .as_ref()
            .and_then(|m| m.name.clone())
            .unwrap_or_else(|| name.to_string());
        let version = metadata.and_then(|m| m.version);
        Ok(NpmsPackageScore {
            name,
            version,
            analyzed_at: raw.analyzed_at,
            final_score: raw.score.as_ref().and_then(|s| s.final_score),
            quality: raw
                .score
                .as_ref()
                .and_then(|s| s.detail.as_ref())
                .and_then(|d| d.quality),
            popularity: raw
                .score
                .as_ref()
                .and_then(|s| s.detail.as_ref())
                .and_then(|d| d.popularity),
            maintenance: raw
                .score
                .as_ref()
                .and_then(|s| s.detail.as_ref())
                .and_then(|d| d.maintenance),
            warnings,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NpmsPackageScore {
    pub name: String,
    pub version: Option<String>,
    pub analyzed_at: Option<String>,
    pub final_score: Option<f64>,
    pub quality: Option<f64>,
    pub popularity: Option<f64>,
    pub maintenance: Option<f64>,
    pub warnings: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSearchResponse {
    total: Option<u64>,
    results: Vec<RawSearchResult>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawSearchResult {
    package: RawPackageSummary,
    score: Option<RawScore>,
    search_score: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPackageSummary {
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
    date: Option<String>,
    links: Option<RawLinks>,
    license: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawLinks {
    npm: Option<String>,
    repository: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawScore {
    #[serde(rename = "final")]
    final_score: Option<f64>,
    detail: Option<RawScoreDetail>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawScoreDetail {
    quality: Option<f64>,
    popularity: Option<f64>,
    maintenance: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawPackageScore {
    analyzed_at: Option<String>,
    collected: Option<RawCollected>,
    score: Option<RawScore>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCollected {
    metadata: Option<RawMetadata>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMetadata {
    name: Option<String>,
    version: Option<String>,
}

fn days_old(rfc3339: &str) -> Option<i64> {
    let date = rfc3339.split_once('T').map(|(d, _)| d)?;
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let analyzed_days = days_from_civil(y, m, d);
    let today_days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64
        / 86_400;
    Some(today_days - analyzed_days)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[async_trait::async_trait]
impl BackendClient for NpmsClient {
    async fn search_code(&self, query: &str, filters: &Filters) -> Result<BackendResponse> {
        let start = tokio::time::Instant::now();
        let mut warnings = Vec::new();
        let requested_size = filters.limit.unwrap_or(25);
        let size = requested_size.min(NPMS_MAX_SIZE);
        if size < requested_size {
            warnings.push(format!(
                "npms.io caps size at {NPMS_MAX_SIZE}; requested {requested_size}"
            ));
        }
        let requested_from = filters.offset.unwrap_or(0);
        let from = requested_from.min(NPMS_MAX_FROM);
        if from < requested_from {
            warnings.push(format!(
                "npms.io caps from at {NPMS_MAX_FROM}; requested {requested_from}"
            ));
        }
        let url = build_url(
            &self.base,
            "/v2/search",
            &[
                ("q", query.to_string()),
                ("size", size.to_string()),
                ("from", from.to_string()),
            ],
        )?;
        let resp = match self.http.get(&url, &[]).await {
            Ok(resp) => resp,
            Err(FacadeError::RateLimited { retry_after }) => {
                return Ok(BackendResponse::unavailable(
                    format!("npms.io rate limited (429, retry_after={retry_after:?})"),
                    start.elapsed(),
                ));
            }
            Err(e) => return Err(e),
        };
        if resp.status().as_u16() == 404 {
            return Ok(BackendResponse::unavailable(
                "npms.io returned 404 for the search endpoint",
                start.elapsed(),
            ));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(FacadeError::Npms(format!(
                "search failed with status {status}: {}",
                &body[..body.len().min(200)]
            )));
        }
        let parsed: RawSearchResponse = resp
            .json()
            .await
            .map_err(|e| FacadeError::Decode {
                backend: "npms.io",
                detail: e.to_string(),
            })?;
        let hits: Vec<Hit> = parsed
            .results
            .into_iter()
            .map(|r| Hit {
                backend: "npms.io",
                repo: r.package.name.clone(),
                path: None,
                url: r
                    .package
                    .links
                    .as_ref()
                    .and_then(|l| l.npm.clone())
                    .or_else(|| {
                        r.package
                            .links
                            .as_ref()
                            .and_then(|l| l.repository.clone())
                    }),
                line_ranges: Vec::new(),
                snippet: r.package.description.clone().unwrap_or_default(),
                license: r.package.license,
                score: r
                    .score
                    .as_ref()
                    .and_then(|s| s.final_score)
                    .or(r.search_score),
            })
            .collect();
        let total = parsed.total;
        let shown = from as u64 + hits.len() as u64;
        let has_more = total.is_some_and(|t| shown < t);
        warnings.push(
            "npms.io scores can be months or years stale (analyzedAt is irregularly refreshed); treat them as a cold-start prior"
                .to_string(),
        );
        Ok(BackendResponse {
            hits,
            total,
            has_more,
            partial: false,
            status: Status::Ok,
            warnings,
            elapsed: start.elapsed(),
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "npms.io",
            regex: false,
            language_filter: false,
            repo_filter: false,
            org_filter: false,
            path_filter: false,
            max_results: Some(NPMS_MAX_FROM as u64 + NPMS_MAX_SIZE as u64),
            rate_limit_per_minute: None,
            auth_required: false,
            line_numbers: false,
            notes: &[
                "npm package-name search plus quality/popularity/maintenance scores",
                "size max 250, from max 10000",
                "scores are precomputed and can be years stale; use analyzedAt from package_scores to judge freshness",
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
    fn computes_days_old() {
        let old = days_old("2023-01-13T10:00:00.000Z").unwrap();
        assert!(old > 365, "got {old}");
        assert!(days_old("not-a-date").is_none());
    }

    #[tokio::test]
    async fn search_parses_results_and_totals() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total": 57,
                "results": [{
                    "package": {
                        "name": "react",
                        "version": "19.0.0",
                        "description": "React is a JavaScript library",
                        "links": {
                            "npm": "https://www.npmjs.com/package/react",
                            "repository": "https://github.com/facebook/react"
                        },
                        "license": "MIT"
                    },
                    "score": {
                        "final": 0.78,
                        "detail": { "quality": 0.85, "popularity": 0.99, "maintenance": 0.7 }
                    },
                    "searchScore": 100.5
                }]
            })))
            .mount(&server)
            .await;
        let client = NpmsClient::with_base(server.uri());
        let resp = client.search_code("react", &Filters::default()).await.unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert_eq!(resp.total, Some(57));
        assert!(resp.has_more);
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].repo.as_deref(), Some("react"));
        assert_eq!(resp.hits[0].license.as_deref(), Some("MIT"));
        assert_eq!(resp.hits[0].score, Some(0.78));
        assert_eq!(resp.hits[0].backend, "npms.io");
        assert!(resp
            .warnings
            .iter()
            .any(|w| w.contains("cold-start prior")));
    }

    #[tokio::test]
    async fn clamps_and_warns_on_caps() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total": 0, "results": []
            })))
            .mount(&server)
            .await;
        let client = NpmsClient::with_base(server.uri());
        let resp = client
            .search_code(
                "react",
                &Filters {
                    limit: Some(500),
                    offset: Some(20000),
                    ..Filters::default()
                },
            )
            .await
            .unwrap();
        assert!(resp.warnings.iter().any(|w| w.contains("caps size at 250")));
        assert!(resp
            .warnings
            .iter()
            .any(|w| w.contains("caps from at 10000")));
        let requests = server.received_requests().await.unwrap();
        let url = requests[0].url.to_string();
        assert!(url.contains("size=250"));
        assert!(url.contains("from=10000"));
    }

    #[tokio::test]
    async fn rate_limit_is_unavailable_not_empty_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/search"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let client = NpmsClient::with_base(server.uri());
        let resp = client.search_code("react", &Filters::default()).await.unwrap();
        assert!(matches!(resp.status, Status::Unavailable { .. }));
        assert!(resp.hits.is_empty());
    }

    #[tokio::test]
    async fn package_scores_flags_stale_analysis() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/package/react"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "analyzedAt": "2023-01-13T08:00:00.000Z",
                "collected": { "metadata": { "name": "react", "version": "18.2.0" } },
                "score": {
                    "final": 0.74,
                    "detail": { "quality": 0.8, "popularity": 0.99, "maintenance": 0.6 }
                }
            })))
            .mount(&server)
            .await;
        let client = NpmsClient::with_base(server.uri());
        let scores = client.package_scores("react").await.unwrap();
        assert_eq!(scores.final_score, Some(0.74));
        assert_eq!(scores.quality, Some(0.8));
        assert_eq!(scores.maintenance, Some(0.6));
        assert!(!scores.warnings.is_empty());
        assert!(scores.warnings[0].contains("days old"));
    }
}
