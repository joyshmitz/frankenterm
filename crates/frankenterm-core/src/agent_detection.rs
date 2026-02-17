//! Feature-gated adapter for filesystem-based coding-agent detection.
//!
//! This module wraps `franken-agent-detection` with:
//! - a process-wide `OnceLock` cache for startup probes,
//! - conversion helpers for inventory consumers, and
//! - stable record shapes used by `agent_correlator`.

use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use tracing::debug;

pub use fad::{
    AgentDetectError, AgentDetectOptions, AgentDetectRootOverride, InstalledAgentDetectionEntry,
    InstalledAgentDetectionReport, InstalledAgentDetectionSummary,
};
use franken_agent_detection as fad;

/// Canonical connector slugs supported by `franken-agent-detection` v0.1.0.
pub const KNOWN_AGENT_SLUGS: &[&str] = &[
    "claude",
    "cline",
    "codex",
    "cursor",
    "factory",
    "gemini",
    "github-copilot",
    "opencode",
    "windsurf",
];

/// Flattened inventory record used by higher-level correlation logic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledAgentRecord {
    pub slug: String,
    pub detected: bool,
    pub evidence: Vec<String>,
    pub root_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

static INSTALLED_AGENT_REPORT_CACHE: OnceLock<RwLock<Arc<InstalledAgentDetectionReport>>> =
    OnceLock::new();

/// Run detection with explicit options (fixture/test-friendly).
///
/// # Errors
/// Returns `AgentDetectError` when option validation fails.
pub fn detect_installed_agents_with_options(
    opts: &AgentDetectOptions,
) -> Result<InstalledAgentDetectionReport, AgentDetectError> {
    fad::detect_installed_agents(opts)
}

/// Detect installed agents with a process-wide cache.
///
/// The first successful call seeds a `OnceLock`; subsequent calls are zero-I/O.
///
/// # Errors
/// Returns `AgentDetectError` if detection fails before cache initialization.
pub fn detect_installed_agents_cached()
-> Result<Arc<InstalledAgentDetectionReport>, AgentDetectError> {
    if let Some(lock) = INSTALLED_AGENT_REPORT_CACHE.get() {
        let guard = lock
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        return Ok(Arc::clone(&guard));
    }

    let report = Arc::new(fad::detect_installed_agents(&AgentDetectOptions::default())?);
    match INSTALLED_AGENT_REPORT_CACHE.set(RwLock::new(Arc::clone(&report))) {
        Ok(()) => {
            debug!(
                detected = report.summary.detected_count,
                total = report.summary.total_count,
                "Initialized installed-agent detection cache"
            );
            Ok(report)
        }
        Err(_already_set) => {
            let lock = INSTALLED_AGENT_REPORT_CACHE
                .get()
                .expect("installed-agent report cache pre-initialized");
            let guard = lock
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Ok(Arc::clone(&guard))
        }
    }
}

/// Force a fresh filesystem probe and replace the process cache.
///
/// This allows long-running watcher/robot processes to re-detect installed
/// agents without restarting.
///
/// # Errors
/// Returns `AgentDetectError` if the probe fails.
pub fn detect_installed_agents_refresh()
-> Result<Arc<InstalledAgentDetectionReport>, AgentDetectError> {
    let report = Arc::new(fad::detect_installed_agents(&AgentDetectOptions::default())?);
    let lock = INSTALLED_AGENT_REPORT_CACHE.get_or_init(|| RwLock::new(Arc::clone(&report)));
    {
        let mut guard = lock
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Arc::clone(&report);
    }

    debug!(
        detected = report.summary.detected_count,
        total = report.summary.total_count,
        "Refreshed installed-agent detection cache"
    );

    Ok(report)
}

/// Convert a detection report to flattened inventory records.
#[must_use]
pub fn installed_agent_records_from_report(
    report: &InstalledAgentDetectionReport,
) -> Vec<InstalledAgentRecord> {
    report
        .installed_agents
        .iter()
        .map(record_from_entry)
        .collect()
}

/// Read cached detection and return flattened inventory records.
///
/// # Errors
/// Returns `AgentDetectError` if detection fails before cache initialization.
pub fn installed_agent_records_cached() -> Result<Vec<InstalledAgentRecord>, AgentDetectError> {
    let report = detect_installed_agents_cached()?;
    Ok(installed_agent_records_from_report(report.as_ref()))
}

/// Force-refresh detection cache and return flattened inventory records.
///
/// # Errors
/// Returns `AgentDetectError` if probing fails.
pub fn installed_agent_records_refresh() -> Result<Vec<InstalledAgentRecord>, AgentDetectError> {
    let report = detect_installed_agents_refresh()?;
    Ok(installed_agent_records_from_report(report.as_ref()))
}

fn record_from_entry(entry: &InstalledAgentDetectionEntry) -> InstalledAgentRecord {
    InstalledAgentRecord {
        slug: entry.slug.clone(),
        detected: entry.detected,
        evidence: entry.evidence.clone(),
        root_paths: entry.root_paths.clone(),
        config_path: entry.root_paths.first().cloned(),
        binary_path: infer_binary_path(&entry.evidence),
        version: infer_version(&entry.evidence),
    }
}

fn infer_binary_path(evidence: &[String]) -> Option<String> {
    evidence.iter().find_map(|line| {
        line.strip_prefix("binary exists:")
            .or_else(|| line.strip_prefix("binary:"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn infer_version(evidence: &[String]) -> Option<String> {
    evidence.iter().find_map(|line| {
        line.strip_prefix("version:")
            .or_else(|| line.strip_prefix("version="))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn known_slugs() -> Vec<String> {
        KNOWN_AGENT_SLUGS
            .iter()
            .map(|slug| (*slug).to_string())
            .collect()
    }

    #[test]
    fn startup_detection_all_known_connectors_detected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut overrides = Vec::new();
        for slug in KNOWN_AGENT_SLUGS {
            let root = tmp.path().join(slug);
            std::fs::create_dir_all(&root).expect("create connector root");
            overrides.push(AgentDetectRootOverride {
                slug: (*slug).to_string(),
                root,
            });
        }

        let report = detect_installed_agents_with_options(&AgentDetectOptions {
            only_connectors: Some(known_slugs()),
            include_undetected: true,
            root_overrides: overrides,
        })
        .expect("detect");

        assert_eq!(report.summary.total_count, KNOWN_AGENT_SLUGS.len());
        assert_eq!(report.summary.detected_count, KNOWN_AGENT_SLUGS.len());
        assert_eq!(report.installed_agents.len(), KNOWN_AGENT_SLUGS.len());
        assert!(report.installed_agents.iter().all(|entry| entry.detected));
    }

    #[test]
    fn startup_detection_none_detected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let overrides: Vec<AgentDetectRootOverride> = KNOWN_AGENT_SLUGS
            .iter()
            .map(|slug| AgentDetectRootOverride {
                slug: (*slug).to_string(),
                root: tmp.path().join(format!("missing-{slug}")),
            })
            .collect();

        let report = detect_installed_agents_with_options(&AgentDetectOptions {
            only_connectors: Some(known_slugs()),
            include_undetected: true,
            root_overrides: overrides,
        })
        .expect("detect");

        assert_eq!(report.summary.total_count, KNOWN_AGENT_SLUGS.len());
        assert_eq!(report.summary.detected_count, 0);
        assert!(report.installed_agents.iter().all(|entry| !entry.detected));
    }

    #[test]
    fn startup_detection_partial_detected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut overrides = Vec::new();
        for (idx, slug) in KNOWN_AGENT_SLUGS.iter().enumerate() {
            let root = tmp.path().join(slug);
            if idx < 3 {
                std::fs::create_dir_all(&root).expect("create root");
            }
            overrides.push(AgentDetectRootOverride {
                slug: (*slug).to_string(),
                root,
            });
        }

        let report = detect_installed_agents_with_options(&AgentDetectOptions {
            only_connectors: Some(known_slugs()),
            include_undetected: true,
            root_overrides: overrides,
        })
        .expect("detect");

        assert_eq!(report.summary.total_count, KNOWN_AGENT_SLUGS.len());
        assert_eq!(report.summary.detected_count, 3);
    }

    #[test]
    fn detection_entries_include_evidence() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = detect_installed_agents_with_options(&AgentDetectOptions {
            only_connectors: Some(vec!["codex".to_string()]),
            include_undetected: true,
            root_overrides: vec![AgentDetectRootOverride {
                slug: "codex".to_string(),
                root: tmp.path().join("missing-codex"),
            }],
        })
        .expect("detect");

        let codex = report
            .installed_agents
            .iter()
            .find(|entry| entry.slug == "codex")
            .expect("codex entry");
        assert!(!codex.evidence.is_empty());
    }

    #[test]
    fn detection_caching_oncelock_returns_same_ref() {
        let r1 = detect_installed_agents_cached().expect("cached report");
        let r2 = detect_installed_agents_cached().expect("cached report");
        assert!(Arc::ptr_eq(&r1, &r2));
    }

    #[test]
    fn detection_refresh_replaces_cached_report() {
        let before = detect_installed_agents_cached().expect("cached report");
        let refreshed = detect_installed_agents_refresh().expect("refreshed report");
        assert!(
            !Arc::ptr_eq(&before, &refreshed),
            "refresh should replace cached Arc"
        );

        let after = detect_installed_agents_cached().expect("cached report");
        assert!(
            Arc::ptr_eq(&after, &refreshed),
            "cached report should point to refreshed value"
        );
    }

    #[test]
    fn detection_with_single_override_is_fast() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("codex");
        std::fs::create_dir_all(&root).expect("create root");

        let started = Instant::now();
        let report = detect_installed_agents_with_options(&AgentDetectOptions {
            only_connectors: Some(vec!["codex".to_string()]),
            include_undetected: true,
            root_overrides: vec![AgentDetectRootOverride {
                slug: "codex".to_string(),
                root,
            }],
        })
        .expect("detect");
        let elapsed = started.elapsed();

        assert_eq!(report.summary.total_count, 1);
        assert_eq!(report.summary.detected_count, 1);
        assert!(
            elapsed < Duration::from_millis(50),
            "single-connector detection took {:?}, expected < 50ms",
            elapsed
        );
    }
}
