//! `oss_repo_profile` over live backends: ecosyste.ms repository metadata
//! merged with deps.dev package data (versions/advisory posture when an npm
//! package matches the repo name).

use oss_core::EngineOutput;
use oss_core::BackendStatus;
use oss_core::ToolError;
use oss_core::RepoProfileParams;
use oss_facade::{DepsVersionInfo, FacadeError};
use serde_json::{json, Value};

use crate::status;
use crate::util::{activity, days_since, license_class, split_repo, star_tier};
use crate::Inner;

pub(crate) async fn run(inner: &Inner, params: &RepoProfileParams) -> Result<EngineOutput, ToolError> {
    let input = serde_json::to_value(params).unwrap_or(Value::Null);
    let (owner, name) = split_repo(&params.repo).ok_or_else(|| ToolError::InvalidValue {
        field: "repo".into(),
        message: format!("repo `{}` is not in owner/name form", params.repo),
        input: input.clone(),
        correction: "use owner/name form, e.g. \"tokio-rs/tokio\"; call oss_search_repos if unsure".into(),
    })?;
    let lookup_url = format!("https://github.com/{}/{}", owner, name);

    let meta = match inner.ecosystems.repository_lookup(&lookup_url).await {
        Ok(Some(meta)) => meta,
        Ok(None) => {
            return Err(ToolError::NotFound {
                kind: "repo",
                name: params.repo.clone(),
                input: input.clone(),
                correction: "call oss_search_repos to find the exact owner/name first".into(),
            });
        }
        Err(e) => {
            let statuses = vec![
                status::unavailable("ecosyste.ms", e.to_string()),
                status::unavailable("deps.dev", "skipped: repository metadata unavailable"),
            ];
            inner.record("oss_repo_profile", statuses);
            return Ok(EngineOutput {
                results: Vec::new(),
                total: 0,
                has_more: false,
                partial: true,
                next_cursor: None,
            });
        }
    };

    // deps.dev by exact repo-name package guess (npm system; not-found is a
    // normal "no matching package", not an error).
    let deps_versions = inner.deps_dev.package_metadata("npm", name).await;
    let deps_dev_value: Value;
    let deps_status: BackendStatus = match &deps_versions {
        Ok(versions) if !versions.is_empty() => {
            deps_dev_value = deps_json(versions);
            status::ok("deps.dev")
        }
        Ok(_) => {
            deps_dev_value = Value::String(format!("no npm package named `{name}` tracked by deps.dev"));
            status::ok_with("deps.dev", "exact-name lookup found no npm package")
        }
        Err(FacadeError::DepsDev(d)) if d.contains("not found") => {
            deps_dev_value = Value::String(format!("no npm package named `{name}` tracked by deps.dev"));
            status::ok_with("deps.dev", "exact-name lookup found no npm package")
        }
        Err(e) => {
            deps_dev_value = Value::String("deps.dev unavailable for this profile".to_string());
            status::degraded("deps.dev", e.to_string())
        }
    };

    let stars = meta.stargazers_count.unwrap_or(0);
    let days = meta.pushed_at.as_deref().and_then(days_since);
    let license = meta.license.clone().map(|l| l.to_ascii_uppercase());
    let mut row = json!({
        "repo": meta.full_name.clone().unwrap_or_else(|| params.repo.clone()),
        "what": meta.description,
        "stars": stars,
        "star_tier": star_tier(stars),
        "forks": meta.forks_count.unwrap_or(0),
        "open_issues": meta.open_issues_count,
        "license": license,
        "license_class": license.as_deref().map(license_class),
        "archived": meta.archived.unwrap_or(false),
        "language": meta.language,
        "topics": meta.topics,
        "last_push": meta.pushed_at,
        "days_since_activity": days,
        "activity": days.map(|d| activity(u64::from(d))),
        "homepage": meta.homepage,
        "url": meta.html_url.clone().or_else(|| meta.repository_url.clone()),
        "readme_path": meta.metadata.files.readme,
        "deps_dev": deps_dev_value,
        "sources": ["ecosyste.ms", "deps.dev"],
    });
    if params.deep_docs {
        let deep = match inner
            .ecosystems
            .package_metadata("npmjs.org", name)
            .await
        {
            Ok(Some(pkg)) => json!({
                "latest_release": pkg.latest_release_number,
                "latest_release_published_at": pkg.latest_release_published_at,
                "versions_count": pkg.versions_count,
                "dependent_packages": pkg.dependent_packages_count,
                "dependent_repos": pkg.dependent_repos_count,
                "source": format!(
                    "https://packages.ecosyste.ms/api/v1/registries/npmjs.org/packages/{name}"
                ),
            }),
            _ => json!(format!(
                "no ecosyste.ms package record for `{name}`; deep docs unavailable"
            )),
        };
        row["deep_docs"] = deep;
    }

    inner.record(
        "oss_repo_profile",
        vec![status::ok("ecosyste.ms"), deps_status],
    );
    Ok(EngineOutput::complete(vec![row]))
}

fn deps_json(versions: &[DepsVersionInfo]) -> Value {
    let default_version = versions.iter().find(|v| v.is_default);
    let deprecated = versions.iter().any(|v| v.is_deprecated);
    json!({
        "system": default_version.map(|v| v.system.clone()).unwrap_or_default(),
        "name": default_version.map(|v| v.name.clone()).unwrap_or_default(),
        "versions_tracked": versions.len(),
        "default_version": default_version.map(|v| v.version.clone()),
        "default_published_at": default_version.and_then(|v| v.published_at.clone()),
        "any_deprecated": deprecated,
    })
}
