//! Status helpers shared by the tool modules.

use oss_core::{BackendState, BackendStatus};
use oss_facade::{BackendResponse, Status};

pub(crate) fn ok(backend: &str) -> BackendStatus {
    BackendStatus {
        backend: backend.to_string(),
        status: BackendState::Ok,
        detail: None,
    }
}

pub(crate) fn ok_with(backend: &str, detail: impl Into<String>) -> BackendStatus {
    BackendStatus {
        backend: backend.to_string(),
        status: BackendState::Ok,
        detail: Some(detail.into()),
    }
}

pub(crate) fn degraded(backend: &str, detail: impl Into<String>) -> BackendStatus {
    BackendStatus {
        backend: backend.to_string(),
        status: BackendState::Degraded,
        detail: Some(detail.into()),
    }
}

pub(crate) fn unavailable(backend: &str, detail: impl Into<String>) -> BackendStatus {
    BackendStatus {
        backend: backend.to_string(),
        status: BackendState::Error,
        detail: Some(detail.into()),
    }
}

/// Aggregate one backend's per-probe responses into a single status:
/// all Ok -> Ok; all Unavailable -> Error; mixed -> Degraded.
pub(crate) fn aggregate(backend: &str, responses: &[&BackendResponse]) -> BackendStatus {
    let mut ok_n = 0usize;
    let mut unavail_n = 0usize;
    let mut degraded_n = 0usize;
    let mut first_reason: Option<String> = None;
    for r in responses {
        match &r.status {
            Status::Ok => ok_n += 1,
            Status::Degraded { reason } => {
                degraded_n += 1;
                first_reason.get_or_insert_with(|| reason.clone());
            }
            Status::Unavailable { reason } => {
                unavail_n += 1;
                first_reason.get_or_insert_with(|| reason.clone());
            }
        }
    }
    let total = ok_n + degraded_n + unavail_n;
    if total == 0 {
        return unavailable(backend, "no probes dispatched");
    }
    if unavail_n == total {
        return unavailable(
            backend,
            format!(
                "all {total} probe(s) unavailable: {}",
                first_reason.unwrap_or_default()
            ),
        );
    }
    if degraded_n == 0 && unavail_n == 0 {
        return ok(backend);
    }
    degraded(
        backend,
        format!(
            "{unavail_n} unavailable, {degraded_n} degraded of {total} probe(s): {}",
            first_reason.unwrap_or_default()
        ),
    )
}
