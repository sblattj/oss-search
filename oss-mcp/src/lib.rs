//! Library surface for `oss-mcp` binary internals.
//!
//! Exists so integration tests (and future library consumers) can reach
//! items such as [`version::VERSION`] that the binary target alone could
//! not expose, and to unit-test the engine-selection policy separately
//! from engine construction (which may create runtimes and load indexes).

pub mod version;

/// Which engine `oss-mcp` will serve, as decided by
/// [`choose_engine`] from the explicit `--engine` flag, the `--live`
/// flag, and whether a hot-set index auto-detects and loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineChoice {
    /// Offline stub fixtures (no hot-set, no network).
    Stub,
    /// Remote backends only.
    Live,
    /// Local hot-set index only.
    Hotset,
    /// Local hot-set first, federated with remote backends.
    Merged,
}

/// Engine selection policy: an explicit `--engine` value wins; otherwise
/// `--live` selects the live engine (merged with the hot-set when one is
/// available) and the default auto-detects the hot-set, falling back to
/// the stub engine.
///
/// `engine_flag` must already be validated as one of `stub`, `live`,
/// `hotset` (see [`parse_engine_flag`]); an unknown value is still handled
/// defensively here.
pub fn choose_engine(
    live: bool,
    engine_flag: Option<&str>,
    hotset_available: bool,
) -> Result<EngineChoice, String> {
    match engine_flag {
        Some("stub") => Ok(EngineChoice::Stub),
        Some("live") => Ok(EngineChoice::Live),
        Some("hotset") => {
            if hotset_available {
                Ok(EngineChoice::Hotset)
            } else {
                Err(
                    "--engine hotset: no loadable hot-set index at <OSS_SEARCH_HOME ?? ~/.cache/oss-search>/index \
                     (run `oss-cli hotset build` first)"
                        .to_string(),
                )
            }
        }
        Some(other) => Err(format!(
            "unknown engine '{other}' (known: stub, live, hotset)"
        )),
        None => Ok(match (live, hotset_available) {
            (false, false) => EngineChoice::Stub,
            (false, true) => EngineChoice::Hotset,
            (true, false) => EngineChoice::Live,
            (true, true) => EngineChoice::Merged,
        }),
    }
}

/// Validate an `--engine` value.
pub fn parse_engine_flag(value: &str) -> Result<(), String> {
    match value {
        "stub" | "live" | "hotset" => Ok(()),
        other => Err(format!(
            "unknown engine '{other}' (known: stub, live, hotset)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_stub_without_hotset() {
        assert_eq!(
            choose_engine(false, None, false).unwrap(),
            EngineChoice::Stub
        );
    }

    #[test]
    fn live_without_hotset_is_live() {
        assert_eq!(
            choose_engine(true, None, false).unwrap(),
            EngineChoice::Live
        );
    }

    #[test]
    fn auto_detect_prefers_hotset() {
        assert_eq!(
            choose_engine(false, None, true).unwrap(),
            EngineChoice::Hotset
        );
    }

    #[test]
    fn live_plus_hotset_merges() {
        assert_eq!(
            choose_engine(true, None, true).unwrap(),
            EngineChoice::Merged
        );
    }

    #[test]
    fn explicit_flag_beats_auto_detection() {
        assert_eq!(
            choose_engine(true, Some("stub"), true).unwrap(),
            EngineChoice::Stub
        );
        assert_eq!(
            choose_engine(false, Some("live"), true).unwrap(),
            EngineChoice::Live
        );
        assert_eq!(
            choose_engine(true, Some("hotset"), true).unwrap(),
            EngineChoice::Hotset
        );
        // Explicit hotset without a loadable index fails loudly instead of
        // silently substituting another engine.
        assert!(choose_engine(false, Some("hotset"), false).is_err());
    }

    #[test]
    fn explicit_hotset_without_index_fails_loudly() {
        let err = choose_engine(false, Some("hotset"), false).unwrap_err();
        assert!(err.contains("oss-cli hotset build"), "err: {err}");
    }

    #[test]
    fn engine_flag_validation() {
        assert!(parse_engine_flag("stub").is_ok());
        assert!(parse_engine_flag("live").is_ok());
        assert!(parse_engine_flag("hotset").is_ok());
        assert!(parse_engine_flag("hot").is_err());
    }
}
