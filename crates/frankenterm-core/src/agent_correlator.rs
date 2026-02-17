//! Agent session correlation and metadata capture for checkpoint snapshots.
//!
//! Detects which AI coding agent (Claude Code, Codex, Gemini, etc.) is running
//! in each pane and captures metadata for crash-resilient session persistence.
//!
//! # Detection sources (priority order)
//!
//! 1. **Pattern detection context**: Uses existing `DetectionContext.agent_type`
//!    populated by the pattern engine from terminal output matching.
//! 2. **Pane title**: Checks for agent name keywords in the WezTerm pane title.
//! 3. **Process name**: Checks the foreground process name if available.
//!
//! # State tracking
//!
//! Agent state is inferred from the most recent pattern detection rule_id:
//! - `*:banner` → "starting"
//! - `*:tool_use` → "working"
//! - `*:compaction` → "working"
//! - `*:rate_limited` or `*:usage.reached` → "rate_limited"
//! - `*:approval_needed` → "waiting_approval"
//! - `*:cost_summary` or `*:session.end` → "idle"
//!
//! If no recent detection exists, the state defaults to "active".

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, trace, warn};

use crate::events::Event;
use crate::patterns::{AgentType, Detection};
use crate::session_pane_state::AgentMetadata;
use crate::wezterm::PaneInfo;

/// Maximum age of a detection to consider for state inference.
const STATE_DETECTION_MAX_AGE: Duration = Duration::from_secs(300); // 5 minutes

// =============================================================================
// AgentCorrelator
// =============================================================================

/// Correlates pane observations with AI agent identities and states.
///
/// Maintains per-pane tracking of detected agents and their last known states.
/// Called by the checkpoint engine to populate `AgentMetadata` in pane snapshots.
#[derive(Debug)]
pub struct AgentCorrelator {
    /// Per-pane agent tracking state.
    pane_agents: HashMap<u64, PaneAgentState>,
    /// Installed agents discovered via filesystem probes (feature-gated source).
    installed_agents: HashMap<String, InstalledAgentInventoryEntry>,
}

/// Tracked agent state for a single pane.
#[derive(Debug, Clone)]
struct PaneAgentState {
    /// Detected agent type.
    agent_type: AgentType,
    /// Canonical slug used to align runtime state with installation probes.
    slug: String,
    /// How the agent was detected.
    source: DetectionSource,
    /// Agent session ID if extracted from patterns.
    session_id: Option<String>,
    /// Last inferred state (e.g., "working", "idle").
    last_state: String,
    /// When the state was last updated.
    last_state_at: Instant,
    /// Most recent rule_id that updated the state.
    last_rule_id: Option<String>,
    /// Installation metadata when this running agent maps to a detected install.
    installation: Option<AgentInstallationMetadata>,
}

/// How an agent was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionSource {
    /// Detected from filesystem installation probes.
    Filesystem,
    /// Detected from pattern engine output matching.
    PatternEngine,
    /// Detected from pane title keywords.
    PaneTitle,
    /// Detected from foreground process name.
    ProcessName,
}

impl DetectionSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::PatternEngine => "pattern_engine",
            Self::PaneTitle => "pane_title",
            Self::ProcessName => "process_name",
        }
    }
}

/// Installation metadata attached to a running pane-level agent state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInstallationMetadata {
    /// Canonical connector slug from filesystem detection.
    pub slug: String,
    /// Best-effort config root path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    /// Best-effort binary path if discoverable from probe evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    /// Best-effort version string if discoverable from probe evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Installation inventory entry (filesystem side).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledAgentInventoryEntry {
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

/// Running pane-level inventory entry (runtime side).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunningAgentInventoryEntry {
    pub pane_id: u64,
    pub slug: String,
    pub agent_type: String,
    pub detection_source: DetectionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation: Option<AgentInstallationMetadata>,
}

/// Unified inventory view combining installed and running agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentInventory {
    pub installed: Vec<InstalledAgentInventoryEntry>,
    pub running: HashMap<u64, RunningAgentInventoryEntry>,
}

/// Aggregated installed/running classification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInventorySummary {
    pub installed_count: usize,
    pub running_count: usize,
    pub installed_only: Vec<String>,
    pub running_only: Vec<String>,
    pub installed_and_running: Vec<String>,
}

/// Lookup view for a specific slug.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInventoryLookup {
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<InstalledAgentInventoryEntry>,
    pub running: Vec<RunningAgentInventoryEntry>,
}

impl AgentInventory {
    /// Summarize installed/running overlap for operator and robot-mode consumption.
    #[must_use]
    pub fn summary(&self) -> AgentInventorySummary {
        let installed_detected: BTreeSet<String> = self
            .installed
            .iter()
            .filter(|entry| entry.detected)
            .map(|entry| entry.slug.clone())
            .collect();

        let running_slugs: BTreeSet<String> = self
            .running
            .values()
            .map(|entry| entry.slug.clone())
            .collect();

        let installed_and_running: Vec<String> = installed_detected
            .intersection(&running_slugs)
            .cloned()
            .collect();
        let installed_only: Vec<String> = installed_detected
            .difference(&running_slugs)
            .cloned()
            .collect();
        let running_only: Vec<String> = running_slugs
            .difference(&installed_detected)
            .cloned()
            .collect();

        AgentInventorySummary {
            installed_count: installed_detected.len(),
            running_count: self.running.len(),
            installed_only,
            running_only,
            installed_and_running,
        }
    }

    /// Lookup installed/running inventory for a specific slug.
    #[must_use]
    pub fn agent_by_slug(&self, slug: &str) -> Option<AgentInventoryLookup> {
        let normalized = slug.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return None;
        }

        let installed = self
            .installed
            .iter()
            .find(|entry| entry.slug == normalized)
            .cloned();
        let running: Vec<RunningAgentInventoryEntry> = self
            .running
            .values()
            .filter(|entry| entry.slug == normalized)
            .cloned()
            .collect();

        if installed.is_none() && running.is_empty() {
            return None;
        }

        Some(AgentInventoryLookup {
            slug: normalized,
            installed,
            running,
        })
    }
}

impl AgentCorrelator {
    /// Create a new agent correlator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pane_agents: HashMap::new(),
            installed_agents: load_installed_agents_map(),
        }
    }

    /// Create a correlator with explicit installed inventory (test/fixture-friendly).
    #[must_use]
    pub fn with_installed_entries(installed: Vec<InstalledAgentInventoryEntry>) -> Self {
        let installed_agents = installed
            .into_iter()
            .map(|entry| (entry.slug.clone(), entry))
            .collect();
        Self {
            pane_agents: HashMap::new(),
            installed_agents,
        }
    }

    /// Update agent tracking from a batch of pattern detections.
    ///
    /// Called after each detection pass to capture agent identity and state
    /// from newly matched rules.
    pub fn ingest_detections(&mut self, pane_id: u64, detections: &[Detection]) {
        let _ = self.ingest_detections_with_events(pane_id, detections);
    }

    /// Update agent tracking from detections and return lifecycle events.
    ///
    /// Emits `Event::AgentDetected` when a pane first becomes associated with an
    /// agent (or when the associated agent changes). Emits `Event::AgentExited`
    /// for explicit exit-like detection rules.
    pub fn ingest_detections_with_events(
        &mut self,
        pane_id: u64,
        detections: &[Detection],
    ) -> Vec<Event> {
        let mut events = Vec::new();
        for detection in detections {
            if detection.agent_type == AgentType::Wezterm
                || detection.agent_type == AgentType::Unknown
            {
                continue;
            }

            let state = infer_state_from_rule(&detection.rule_id);
            let slug = runtime_slug_for_agent_type(detection.agent_type)
                .map_or_else(|| detection.agent_type.to_string(), str::to_string);
            let session_id = extract_session_id(&detection.extracted);
            let prior = self
                .pane_agents
                .get(&pane_id)
                .map(|entry| (entry.agent_type, entry.source));
            let installation = self.installation_for_slug(pane_id, &slug);

            let entry = self
                .pane_agents
                .entry(pane_id)
                .or_insert_with(|| PaneAgentState {
                    agent_type: detection.agent_type,
                    slug: slug.clone(),
                    source: DetectionSource::PatternEngine,
                    session_id: None,
                    last_state: "active".to_string(),
                    last_state_at: Instant::now(),
                    last_rule_id: None,
                    installation: installation.clone(),
                });

            // Update agent type if detection is more specific
            entry.agent_type = detection.agent_type;
            entry.slug.clone_from(&slug);
            entry.source = DetectionSource::PatternEngine;
            entry.last_state = state.to_string();
            entry.last_state_at = Instant::now();
            entry.last_rule_id = Some(detection.rule_id.clone());
            entry.installation = installation;

            if let Some(sid) = session_id {
                entry.session_id = Some(sid);
            }

            if prior.is_none()
                || prior != Some((detection.agent_type, DetectionSource::PatternEngine))
            {
                events.push(Event::AgentDetected {
                    pane_id,
                    agent_type: entry.agent_type.to_string(),
                    detection_method: DetectionSource::PatternEngine.as_str().to_string(),
                });
            }
            if is_agent_exit_rule(&detection.rule_id) {
                events.push(Event::AgentExited {
                    pane_id,
                    agent_type: entry.agent_type.to_string(),
                    exit_code: extract_exit_code(&detection.extracted),
                });
            }

            trace!(
                pane_id,
                agent = %entry.agent_type,
                state = %entry.last_state,
                rule = %detection.rule_id,
                "Agent state updated from detection"
            );
        }
        events
    }

    /// Update agent tracking from pane info (title and process name).
    ///
    /// Called during checkpoint to ensure all panes have agent detection
    /// even without pattern matches (e.g., new panes not yet processed).
    pub fn update_from_pane_info(&mut self, pane: &PaneInfo) {
        let _ = self.update_from_pane_info_with_event(pane);
    }

    /// Update from pane info and return an optional lifecycle event.
    pub fn update_from_pane_info_with_event(&mut self, pane: &PaneInfo) -> Option<Event> {
        if self.pane_agents.contains_key(&pane.pane_id) {
            return None; // Already detected via patterns — don't downgrade
        }

        // Try title-based detection
        if let Some(agent_type) = detect_agent_from_title(pane.title.as_deref().unwrap_or("")) {
            let slug = runtime_slug_for_agent_type(agent_type)
                .map_or_else(|| agent_type.to_string(), str::to_string);
            self.pane_agents.insert(
                pane.pane_id,
                PaneAgentState {
                    agent_type,
                    slug: slug.clone(),
                    source: DetectionSource::PaneTitle,
                    session_id: None,
                    last_state: "active".to_string(),
                    last_state_at: Instant::now(),
                    last_rule_id: None,
                    installation: self.installation_for_slug(pane.pane_id, &slug),
                },
            );
            debug!(
                pane_id = pane.pane_id,
                agent = %agent_type,
                "Agent detected from pane title"
            );
            return Some(Event::AgentDetected {
                pane_id: pane.pane_id,
                agent_type: agent_type.to_string(),
                detection_method: DetectionSource::PaneTitle.as_str().to_string(),
            });
        }

        // Try process name detection (from foreground_process if available in extras)
        if let Some(process) = pane
            .extra
            .get("foreground_process_name")
            .and_then(|v| v.as_str())
        {
            if let Some(agent_type) = detect_agent_from_process(process) {
                let slug = runtime_slug_for_agent_type(agent_type)
                    .map_or_else(|| agent_type.to_string(), str::to_string);
                self.pane_agents.insert(
                    pane.pane_id,
                    PaneAgentState {
                        agent_type,
                        slug: slug.clone(),
                        source: DetectionSource::ProcessName,
                        session_id: None,
                        last_state: "active".to_string(),
                        last_state_at: Instant::now(),
                        last_rule_id: None,
                        installation: self.installation_for_slug(pane.pane_id, &slug),
                    },
                );
                debug!(
                    pane_id = pane.pane_id,
                    agent = %agent_type,
                    process = %process,
                    "Agent detected from process name"
                );
                return Some(Event::AgentDetected {
                    pane_id: pane.pane_id,
                    agent_type: agent_type.to_string(),
                    detection_method: DetectionSource::ProcessName.as_str().to_string(),
                });
            }
        }
        None
    }

    /// Get agent metadata for a pane, suitable for embedding in a checkpoint snapshot.
    ///
    /// Returns `None` if no agent has been detected for this pane.
    #[must_use]
    pub fn get_metadata(&self, pane_id: u64) -> Option<AgentMetadata> {
        let state = self.pane_agents.get(&pane_id)?;

        // Don't report stale state
        let effective_state = if state.last_state_at.elapsed() > STATE_DETECTION_MAX_AGE {
            "unknown".to_string()
        } else {
            state.last_state.clone()
        };

        Some(AgentMetadata {
            agent_type: state.agent_type.to_string(),
            session_id: state.session_id.clone(),
            state: Some(effective_state),
        })
    }

    /// Build unified installed + running inventory.
    #[must_use]
    pub fn inventory(&self) -> AgentInventory {
        let mut installed: Vec<InstalledAgentInventoryEntry> =
            self.installed_agents.values().cloned().collect();
        installed.sort_by(|a, b| a.slug.cmp(&b.slug));

        let running: HashMap<u64, RunningAgentInventoryEntry> = self
            .pane_agents
            .iter()
            .map(|(pane_id, state)| {
                let effective_state = if state.last_state_at.elapsed() > STATE_DETECTION_MAX_AGE {
                    "unknown".to_string()
                } else {
                    state.last_state.clone()
                };
                (
                    *pane_id,
                    RunningAgentInventoryEntry {
                        pane_id: *pane_id,
                        slug: state.slug.clone(),
                        agent_type: state.agent_type.to_string(),
                        detection_source: state.source,
                        session_id: state.session_id.clone(),
                        state: effective_state,
                        installation: state.installation.clone(),
                    },
                )
            })
            .collect();

        AgentInventory { installed, running }
    }

    /// Remove tracking for a pane (e.g., when pane is closed).
    pub fn remove_pane(&mut self, pane_id: u64) {
        let _ = self.remove_pane_with_event(pane_id, None);
    }

    /// Remove tracking for a pane and return an `AgentExited` event if tracked.
    pub fn remove_pane_with_event(
        &mut self,
        pane_id: u64,
        exit_code: Option<i32>,
    ) -> Option<Event> {
        let state = self.pane_agents.remove(&pane_id)?;
        Some(Event::AgentExited {
            pane_id,
            agent_type: state.agent_type.to_string(),
            exit_code,
        })
    }

    #[must_use]
    fn installation_for_slug(&self, pane_id: u64, slug: &str) -> Option<AgentInstallationMetadata> {
        let Some(entry) = self.installed_agents.get(slug) else {
            if !self.installed_agents.is_empty() {
                warn!(
                    pane_id,
                    slug,
                    "Agent running but not found on filesystem; possibly remote/containerized"
                );
            }
            return None;
        };

        let config_path = entry.root_paths.first().cloned();
        let binary_path = infer_binary_path(&entry.evidence);
        let version = infer_version(&entry.evidence);
        Some(AgentInstallationMetadata {
            slug: entry.slug.clone(),
            config_path,
            binary_path,
            version,
        })
    }

    /// Get the number of panes with detected agents.
    #[must_use]
    pub fn tracked_pane_count(&self) -> usize {
        self.pane_agents.len()
    }
}

impl Default for AgentCorrelator {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether filesystem agent detection support is compiled in.
#[must_use]
pub const fn filesystem_detection_available() -> bool {
    cfg!(feature = "agent-detection")
}

/// Read installed-agent inventory from cache using the filesystem detector when available.
///
/// Returns a user-facing error string when probing fails or the feature is disabled.
pub fn installed_inventory_cached() -> std::result::Result<Vec<InstalledAgentInventoryEntry>, String>
{
    #[cfg(feature = "agent-detection")]
    {
        crate::agent_detection::installed_agent_records_cached()
            .map(convert_records_to_inventory_entries)
            .map_err(|err| err.to_string())
    }

    #[cfg(not(feature = "agent-detection"))]
    {
        Err("agent-detection feature is not enabled".to_string())
    }
}

/// Force-refresh installed-agent inventory by re-running filesystem probes.
///
/// Returns a user-facing error string when probing fails or the feature is disabled.
pub fn installed_inventory_refresh()
-> std::result::Result<Vec<InstalledAgentInventoryEntry>, String> {
    #[cfg(feature = "agent-detection")]
    {
        crate::agent_detection::installed_agent_records_refresh()
            .map(convert_records_to_inventory_entries)
            .map_err(|err| err.to_string())
    }

    #[cfg(not(feature = "agent-detection"))]
    {
        Err("agent-detection feature is not enabled".to_string())
    }
}

// =============================================================================
// Detection helpers
// =============================================================================

#[cfg(feature = "agent-detection")]
fn convert_records_to_inventory_entries(
    records: Vec<crate::agent_detection::InstalledAgentRecord>,
) -> Vec<InstalledAgentInventoryEntry> {
    records
        .into_iter()
        .map(|record| InstalledAgentInventoryEntry {
            slug: record.slug,
            detected: record.detected,
            evidence: record.evidence,
            root_paths: record.root_paths,
            config_path: record.config_path,
            binary_path: record.binary_path,
            version: record.version,
        })
        .collect()
}

#[cfg(feature = "agent-detection")]
fn load_installed_agents_map() -> HashMap<String, InstalledAgentInventoryEntry> {
    let entries = match installed_inventory_cached() {
        Ok(entries) => entries,
        Err(err) => {
            warn!(error = %err, "Failed to load installed agent detection cache");
            return HashMap::new();
        }
    };

    let installed_agents: HashMap<String, InstalledAgentInventoryEntry> = entries
        .into_iter()
        .map(|entry| (entry.slug.clone(), entry))
        .collect();
    debug!(
        count = installed_agents.len(),
        "Loaded installed agent inventory records"
    );
    installed_agents
}

#[cfg(not(feature = "agent-detection"))]
fn load_installed_agents_map() -> HashMap<String, InstalledAgentInventoryEntry> {
    HashMap::new()
}

/// Detect agent type from pane title keywords.
fn detect_agent_from_title(title: &str) -> Option<AgentType> {
    let lower = title.to_lowercase();
    if lower.contains("claude") || lower.contains("claude-code") || lower.contains("claude code") {
        return Some(AgentType::ClaudeCode);
    }
    if lower.contains("codex") || lower.contains("openai") {
        return Some(AgentType::Codex);
    }
    if lower.contains("gemini") {
        return Some(AgentType::Gemini);
    }
    None
}

/// Detect agent type from foreground process name.
fn detect_agent_from_process(process: &str) -> Option<AgentType> {
    let lower = process.to_lowercase();
    if lower.contains("claude") {
        return Some(AgentType::ClaudeCode);
    }
    if lower.contains("codex") {
        return Some(AgentType::Codex);
    }
    if lower.contains("gemini") {
        return Some(AgentType::Gemini);
    }
    None
}

/// Infer agent state from a detection rule_id.
fn infer_state_from_rule(rule_id: &str) -> &'static str {
    // Extract the suffix after the last ':'
    let suffix = rule_id.rsplit(':').next().unwrap_or(rule_id);

    match suffix {
        "banner" | "session.start" => "starting",
        "tool_use" | "compaction" => "working",
        "rate_limited" | "usage_reached" | "usage.reached" | "quota_exceeded" => "rate_limited",
        "approval_needed" | "approval_required" => "waiting_approval",
        "cost_summary" | "session.end" | "token_usage" => "idle",
        "auth.api_key_error" | "auth.login_required" | "auth.device_code_prompt" => "auth_error",
        _ if suffix.starts_with("usage.warning") => "active",
        _ => "active",
    }
}

fn runtime_slug_for_agent_type(agent_type: AgentType) -> Option<&'static str> {
    match agent_type {
        AgentType::ClaudeCode => Some("claude"),
        AgentType::Codex => Some("codex"),
        AgentType::Gemini => Some("gemini"),
        AgentType::Wezterm | AgentType::Unknown => None,
    }
}

fn is_agent_exit_rule(rule_id: &str) -> bool {
    let suffix = rule_id.rsplit(':').next().unwrap_or(rule_id);
    matches!(
        suffix,
        "session.end" | "cost_summary" | "exit" | "process.exit"
    )
}

fn extract_exit_code(extracted: &serde_json::Value) -> Option<i32> {
    let value = extracted.get("exit_code")?;
    if let Some(code) = value.as_i64() {
        return i32::try_from(code).ok();
    }
    value.as_str().and_then(|code| code.parse::<i32>().ok())
}

/// Extract a session ID from detection extracted data (JSON).
fn extract_session_id(extracted: &serde_json::Value) -> Option<String> {
    // Try common patterns for session IDs in extracted data
    extracted
        .get("session_id")
        .or_else(|| extracted.get("resume_session_id"))
        .or_else(|| extracted.get("session"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::Severity;

    fn make_detection(rule_id: &str, agent_type: AgentType) -> Detection {
        Detection {
            rule_id: rule_id.to_string(),
            agent_type,
            event_type: "test".to_string(),
            severity: Severity::Info,
            confidence: 0.9,
            extracted: serde_json::json!({}),
            matched_text: String::new(),
            span: (0, 0),
        }
    }

    fn make_detection_with_session(
        rule_id: &str,
        agent_type: AgentType,
        session_id: &str,
    ) -> Detection {
        Detection {
            rule_id: rule_id.to_string(),
            agent_type,
            event_type: "test".to_string(),
            severity: Severity::Info,
            confidence: 0.9,
            extracted: serde_json::json!({"session_id": session_id}),
            matched_text: String::new(),
            span: (0, 0),
        }
    }

    // ---- Agent detection from detections ----

    #[test]
    fn ingest_detection_tracks_agent() {
        let mut correlator = AgentCorrelator::new();
        let detections = vec![make_detection(
            "core.claude_code:banner",
            AgentType::ClaudeCode,
        )];

        correlator.ingest_detections(1, &detections);

        let meta = correlator.get_metadata(1).unwrap();
        assert_eq!(meta.agent_type, "claude_code");
        assert_eq!(meta.state.as_deref(), Some("starting"));
    }

    #[test]
    fn ingest_detection_updates_state() {
        let mut correlator = AgentCorrelator::new();

        correlator.ingest_detections(
            1,
            &[make_detection(
                "core.claude_code:banner",
                AgentType::ClaudeCode,
            )],
        );
        assert_eq!(
            correlator.get_metadata(1).unwrap().state.as_deref(),
            Some("starting")
        );

        correlator.ingest_detections(
            1,
            &[make_detection(
                "core.claude_code:tool_use",
                AgentType::ClaudeCode,
            )],
        );
        assert_eq!(
            correlator.get_metadata(1).unwrap().state.as_deref(),
            Some("working")
        );
    }

    #[test]
    fn ingest_detection_captures_session_id() {
        let mut correlator = AgentCorrelator::new();
        correlator.ingest_detections(
            1,
            &[make_detection_with_session(
                "core.codex:session.resume_hint",
                AgentType::Codex,
                "codex-sess-abc123",
            )],
        );

        let meta = correlator.get_metadata(1).unwrap();
        assert_eq!(meta.session_id.as_deref(), Some("codex-sess-abc123"));
    }

    // ---- Agent detection from pane info ----

    #[test]
    fn update_from_pane_info_detects_claude() {
        let mut correlator = AgentCorrelator::new();
        let pane = PaneInfo {
            pane_id: 5,
            tab_id: 0,
            window_id: 0,
            domain_id: None,
            domain_name: None,
            workspace: None,
            size: None,
            rows: None,
            cols: None,
            title: Some("claude-code ~/project".to_string()),
            cwd: None,
            tty_name: None,
            cursor_x: None,
            cursor_y: None,
            cursor_visibility: None,
            left_col: None,
            top_row: None,
            is_active: true,
            is_zoomed: false,
            extra: std::collections::HashMap::new(),
        };

        correlator.update_from_pane_info(&pane);

        let meta = correlator.get_metadata(5).unwrap();
        assert_eq!(meta.agent_type, "claude_code");
        assert_eq!(meta.state.as_deref(), Some("active"));
    }

    #[test]
    fn update_from_pane_info_does_not_overwrite_pattern_detection() {
        let mut correlator = AgentCorrelator::new();

        // First: detect via patterns (higher priority)
        correlator.ingest_detections(
            5,
            &[make_detection(
                "core.claude_code:tool_use",
                AgentType::ClaudeCode,
            )],
        );

        // Then: pane info update should NOT overwrite
        let pane = PaneInfo {
            pane_id: 5,
            tab_id: 0,
            window_id: 0,
            domain_id: None,
            domain_name: None,
            workspace: None,
            size: None,
            rows: None,
            cols: None,
            title: Some("gemini-cli".to_string()), // Different agent!
            cwd: None,
            tty_name: None,
            cursor_x: None,
            cursor_y: None,
            cursor_visibility: None,
            left_col: None,
            top_row: None,
            is_active: true,
            is_zoomed: false,
            extra: std::collections::HashMap::new(),
        };

        correlator.update_from_pane_info(&pane);

        // Should still be ClaudeCode from patterns, not Gemini from title
        let meta = correlator.get_metadata(5).unwrap();
        assert_eq!(meta.agent_type, "claude_code");
        assert_eq!(meta.state.as_deref(), Some("working"));
    }

    #[test]
    fn no_detection_returns_none() {
        let correlator = AgentCorrelator::new();
        assert!(correlator.get_metadata(99).is_none());
    }

    // ---- State inference from rule_id ----

    #[test]
    fn infer_state_banner() {
        assert_eq!(infer_state_from_rule("core.claude_code:banner"), "starting");
    }

    #[test]
    fn infer_state_tool_use() {
        assert_eq!(
            infer_state_from_rule("core.claude_code:tool_use"),
            "working"
        );
    }

    #[test]
    fn infer_state_rate_limited() {
        assert_eq!(
            infer_state_from_rule("core.codex:usage.reached"),
            "rate_limited"
        );
        assert_eq!(
            infer_state_from_rule("core.gemini:quota_exceeded"),
            "rate_limited"
        );
    }

    #[test]
    fn infer_state_idle() {
        assert_eq!(
            infer_state_from_rule("core.claude_code:cost_summary"),
            "idle"
        );
    }

    #[test]
    fn infer_state_auth_error() {
        assert_eq!(
            infer_state_from_rule("core.codex:auth.device_code_prompt"),
            "auth_error"
        );
    }

    #[test]
    fn infer_state_unknown_defaults_to_active() {
        assert_eq!(
            infer_state_from_rule("core.claude_code:some_new_rule"),
            "active"
        );
    }

    // ---- Title detection ----

    #[test]
    fn detect_agent_from_title_cases() {
        assert_eq!(
            detect_agent_from_title("claude-code ~/project"),
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(
            detect_agent_from_title("Claude Code"),
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(
            detect_agent_from_title("codex --model o4-mini"),
            Some(AgentType::Codex)
        );
        assert_eq!(
            detect_agent_from_title("openai chat"),
            Some(AgentType::Codex)
        );
        assert_eq!(
            detect_agent_from_title("gemini-cli"),
            Some(AgentType::Gemini)
        );
        assert_eq!(detect_agent_from_title("bash"), None);
        assert_eq!(detect_agent_from_title("vim"), None);
    }

    // ---- Process detection ----

    #[test]
    fn detect_agent_from_process_cases() {
        assert_eq!(
            detect_agent_from_process("claude-code"),
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(detect_agent_from_process("codex"), Some(AgentType::Codex));
        assert_eq!(
            detect_agent_from_process("gemini-cli"),
            Some(AgentType::Gemini)
        );
        assert_eq!(detect_agent_from_process("bash"), None);
        assert_eq!(detect_agent_from_process("node"), None);
    }

    // ---- Session ID extraction ----

    #[test]
    fn extract_session_id_from_extracted() {
        let extracted = serde_json::json!({"session_id": "sess-123"});
        assert_eq!(extract_session_id(&extracted), Some("sess-123".to_string()));

        let extracted = serde_json::json!({"resume_session_id": "resume-456"});
        assert_eq!(
            extract_session_id(&extracted),
            Some("resume-456".to_string())
        );

        let extracted = serde_json::json!({"other_field": "value"});
        assert_eq!(extract_session_id(&extracted), None);

        let extracted = serde_json::json!({"session_id": ""});
        assert_eq!(extract_session_id(&extracted), None);
    }

    // ---- Pane removal ----

    #[test]
    fn remove_pane_clears_tracking() {
        let mut correlator = AgentCorrelator::new();
        correlator.ingest_detections(
            1,
            &[make_detection(
                "core.claude_code:banner",
                AgentType::ClaudeCode,
            )],
        );
        assert!(correlator.get_metadata(1).is_some());

        correlator.remove_pane(1);
        assert!(correlator.get_metadata(1).is_none());
    }

    // ---- Tracked count ----

    #[test]
    fn tracked_pane_count() {
        let mut correlator = AgentCorrelator::new();
        assert_eq!(correlator.tracked_pane_count(), 0);

        correlator.ingest_detections(
            1,
            &[make_detection(
                "core.claude_code:banner",
                AgentType::ClaudeCode,
            )],
        );
        correlator.ingest_detections(2, &[make_detection("core.codex:banner", AgentType::Codex)]);
        assert_eq!(correlator.tracked_pane_count(), 2);
    }

    // ---- Ignores Wezterm/Unknown agent types ----

    #[test]
    fn ignores_wezterm_and_unknown_agents() {
        let mut correlator = AgentCorrelator::new();
        correlator.ingest_detections(
            1,
            &[make_detection("core.wezterm:event", AgentType::Wezterm)],
        );
        correlator.ingest_detections(
            2,
            &[make_detection("unknown:something", AgentType::Unknown)],
        );
        assert_eq!(correlator.tracked_pane_count(), 0);
    }

    // ====================================================================
    // detect_agent_from_title edge cases
    // ====================================================================

    #[test]
    fn detect_title_empty() {
        assert_eq!(detect_agent_from_title(""), None);
    }

    #[test]
    fn detect_title_case_insensitive_claude() {
        assert_eq!(
            detect_agent_from_title("CLAUDE-CODE"),
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(
            detect_agent_from_title("Claude Code"),
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(
            detect_agent_from_title("cLaUdE"),
            Some(AgentType::ClaudeCode)
        );
    }

    #[test]
    fn detect_title_case_insensitive_codex() {
        assert_eq!(
            detect_agent_from_title("CODEX --model o4-mini"),
            Some(AgentType::Codex)
        );
        assert_eq!(
            detect_agent_from_title("OpenAI CLI"),
            Some(AgentType::Codex)
        );
    }

    #[test]
    fn detect_title_case_insensitive_gemini() {
        assert_eq!(detect_agent_from_title("GEMINI"), Some(AgentType::Gemini));
        assert_eq!(
            detect_agent_from_title("Gemini Pro"),
            Some(AgentType::Gemini)
        );
    }

    #[test]
    fn detect_title_claude_substring() {
        // "claude" keyword is contained in longer string
        assert_eq!(
            detect_agent_from_title("running claude-code in tmux"),
            Some(AgentType::ClaudeCode)
        );
    }

    #[test]
    fn detect_title_no_match_similar_words() {
        assert_eq!(detect_agent_from_title("cloudy weather app"), None);
        assert_eq!(detect_agent_from_title("codec"), None);
        assert_eq!(detect_agent_from_title("gem"), None);
    }

    #[test]
    fn detect_title_whitespace_only() {
        assert_eq!(detect_agent_from_title("   "), None);
    }

    #[test]
    fn detect_title_with_path_prefix() {
        assert_eq!(
            detect_agent_from_title("/usr/local/bin/claude-code"),
            Some(AgentType::ClaudeCode)
        );
    }

    // ====================================================================
    // detect_agent_from_process edge cases
    // ====================================================================

    #[test]
    fn detect_process_empty() {
        assert_eq!(detect_agent_from_process(""), None);
    }

    #[test]
    fn detect_process_case_insensitive() {
        assert_eq!(
            detect_agent_from_process("Claude-Code"),
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(detect_agent_from_process("CODEX"), Some(AgentType::Codex));
        assert_eq!(
            detect_agent_from_process("GEMINI-CLI"),
            Some(AgentType::Gemini)
        );
    }

    #[test]
    fn detect_process_no_match() {
        assert_eq!(detect_agent_from_process("vim"), None);
        assert_eq!(detect_agent_from_process("zsh"), None);
        assert_eq!(detect_agent_from_process("python3"), None);
    }

    #[test]
    fn detect_process_with_path() {
        assert_eq!(
            detect_agent_from_process("/home/user/.local/bin/claude"),
            Some(AgentType::ClaudeCode)
        );
    }

    // ====================================================================
    // infer_state_from_rule comprehensive branch coverage
    // ====================================================================

    #[test]
    fn infer_state_session_start() {
        assert_eq!(
            infer_state_from_rule("core.claude_code:session.start"),
            "starting"
        );
    }

    #[test]
    fn infer_state_compaction() {
        assert_eq!(infer_state_from_rule("core.codex:compaction"), "working");
    }

    #[test]
    fn infer_state_rate_limited_variants() {
        assert_eq!(
            infer_state_from_rule("core.codex:rate_limited"),
            "rate_limited"
        );
        assert_eq!(
            infer_state_from_rule("core.codex:usage_reached"),
            "rate_limited"
        );
        assert_eq!(
            infer_state_from_rule("core.gemini:quota_exceeded"),
            "rate_limited"
        );
    }

    #[test]
    fn infer_state_waiting_approval_variants() {
        assert_eq!(
            infer_state_from_rule("core.claude_code:approval_needed"),
            "waiting_approval"
        );
        assert_eq!(
            infer_state_from_rule("core.claude_code:approval_required"),
            "waiting_approval"
        );
    }

    #[test]
    fn infer_state_idle_variants() {
        assert_eq!(infer_state_from_rule("core.codex:session.end"), "idle");
        assert_eq!(
            infer_state_from_rule("core.claude_code:token_usage"),
            "idle"
        );
    }

    #[test]
    fn infer_state_auth_error_variants() {
        assert_eq!(
            infer_state_from_rule("core.codex:auth.api_key_error"),
            "auth_error"
        );
        assert_eq!(
            infer_state_from_rule("core.gemini:auth.login_required"),
            "auth_error"
        );
    }

    #[test]
    fn infer_state_usage_warning_prefix() {
        assert_eq!(
            infer_state_from_rule("core.codex:usage.warning.90pct"),
            "active"
        );
    }

    #[test]
    fn infer_state_no_colon_defaults_active() {
        assert_eq!(infer_state_from_rule("no_colon_rule_id"), "active");
    }

    #[test]
    fn infer_state_empty_string() {
        assert_eq!(infer_state_from_rule(""), "active");
    }

    #[test]
    fn infer_state_multiple_colons_uses_last() {
        // rsplit(':').next() gets the part after the last colon
        assert_eq!(infer_state_from_rule("a:b:c:banner"), "starting");
    }

    // ====================================================================
    // extract_session_id edge cases
    // ====================================================================

    #[test]
    fn extract_session_id_from_session_key() {
        let data = serde_json::json!({"session": "s-789"});
        assert_eq!(extract_session_id(&data), Some("s-789".to_string()));
    }

    #[test]
    fn extract_session_id_priority_order() {
        // "session_id" has priority over "resume_session_id" and "session"
        let data = serde_json::json!({
            "session_id": "first",
            "resume_session_id": "second",
            "session": "third"
        });
        assert_eq!(extract_session_id(&data), Some("first".to_string()));
    }

    #[test]
    fn extract_session_id_falls_to_resume() {
        let data = serde_json::json!({"resume_session_id": "resume-x"});
        assert_eq!(extract_session_id(&data), Some("resume-x".to_string()));
    }

    #[test]
    fn extract_session_id_null_value() {
        let data = serde_json::json!({"session_id": null});
        assert_eq!(extract_session_id(&data), None);
    }

    #[test]
    fn extract_session_id_numeric_value() {
        // as_str() returns None for numbers
        let data = serde_json::json!({"session_id": 42});
        assert_eq!(extract_session_id(&data), None);
    }

    #[test]
    fn extract_session_id_empty_object() {
        let data = serde_json::json!({});
        assert_eq!(extract_session_id(&data), None);
    }

    #[test]
    fn extract_session_id_array_value() {
        let data = serde_json::json!({"session_id": ["a", "b"]});
        assert_eq!(extract_session_id(&data), None);
    }

    // ====================================================================
    // DetectionSource serde tests
    // ====================================================================

    #[test]
    fn detection_source_serde_roundtrip() {
        for src in [
            DetectionSource::Filesystem,
            DetectionSource::PatternEngine,
            DetectionSource::PaneTitle,
            DetectionSource::ProcessName,
        ] {
            let json = serde_json::to_string(&src).unwrap();
            let back: DetectionSource = serde_json::from_str(&json).unwrap();
            assert_eq!(src, back);
        }
    }

    #[test]
    fn detection_source_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&DetectionSource::Filesystem).unwrap(),
            "\"filesystem\""
        );
        assert_eq!(
            serde_json::to_string(&DetectionSource::PatternEngine).unwrap(),
            "\"pattern_engine\""
        );
        assert_eq!(
            serde_json::to_string(&DetectionSource::PaneTitle).unwrap(),
            "\"pane_title\""
        );
        assert_eq!(
            serde_json::to_string(&DetectionSource::ProcessName).unwrap(),
            "\"process_name\""
        );
    }

    #[test]
    fn detection_source_debug() {
        let dbg = format!("{:?}", DetectionSource::PatternEngine);
        assert!(dbg.contains("PatternEngine"));
    }

    #[test]
    fn detection_source_copy() {
        let s = DetectionSource::PaneTitle;
        let s2 = s;
        assert_eq!(s, s2);
    }

    #[test]
    fn detection_source_as_str() {
        assert_eq!(DetectionSource::Filesystem.as_str(), "filesystem");
        assert_eq!(DetectionSource::PatternEngine.as_str(), "pattern_engine");
        assert_eq!(DetectionSource::PaneTitle.as_str(), "pane_title");
        assert_eq!(DetectionSource::ProcessName.as_str(), "process_name");
    }

    // ====================================================================
    // AgentCorrelator additional behavior tests
    // ====================================================================

    #[test]
    fn correlator_default_is_new() {
        let c = AgentCorrelator::default();
        assert_eq!(c.tracked_pane_count(), 0);
    }

    #[test]
    fn correlator_debug() {
        let c = AgentCorrelator::new();
        let dbg = format!("{c:?}");
        assert!(dbg.contains("AgentCorrelator"));
    }

    #[test]
    fn remove_nonexistent_pane_is_noop() {
        let mut c = AgentCorrelator::new();
        c.remove_pane(999); // should not panic
        assert_eq!(c.tracked_pane_count(), 0);
    }

    #[test]
    fn multiple_panes_tracked_independently() {
        let mut c = AgentCorrelator::new();
        c.ingest_detections(
            1,
            &[make_detection(
                "core.claude_code:banner",
                AgentType::ClaudeCode,
            )],
        );
        c.ingest_detections(
            2,
            &[make_detection("core.codex:tool_use", AgentType::Codex)],
        );
        c.ingest_detections(
            3,
            &[make_detection(
                "core.gemini:rate_limited",
                AgentType::Gemini,
            )],
        );

        assert_eq!(c.tracked_pane_count(), 3);
        assert_eq!(c.get_metadata(1).unwrap().agent_type, "claude_code");
        assert_eq!(
            c.get_metadata(1).unwrap().state.as_deref(),
            Some("starting")
        );
        assert_eq!(c.get_metadata(2).unwrap().agent_type, "codex");
        assert_eq!(c.get_metadata(2).unwrap().state.as_deref(), Some("working"));
        assert_eq!(c.get_metadata(3).unwrap().agent_type, "gemini");
        assert_eq!(
            c.get_metadata(3).unwrap().state.as_deref(),
            Some("rate_limited")
        );
    }

    #[test]
    fn ingest_empty_detections_is_noop() {
        let mut c = AgentCorrelator::new();
        c.ingest_detections(1, &[]);
        assert_eq!(c.tracked_pane_count(), 0);
    }

    #[test]
    fn ingest_multiple_detections_uses_last() {
        let mut c = AgentCorrelator::new();
        c.ingest_detections(
            1,
            &[
                make_detection("core.claude_code:banner", AgentType::ClaudeCode),
                make_detection("core.claude_code:tool_use", AgentType::ClaudeCode),
                make_detection("core.claude_code:cost_summary", AgentType::ClaudeCode),
            ],
        );
        // Last detection wins for state
        assert_eq!(c.get_metadata(1).unwrap().state.as_deref(), Some("idle"));
    }

    #[test]
    fn session_id_preserved_across_updates() {
        let mut c = AgentCorrelator::new();
        c.ingest_detections(
            1,
            &[make_detection_with_session(
                "core.codex:banner",
                AgentType::Codex,
                "sess-original",
            )],
        );
        // Second detection without session_id should not clear it
        c.ingest_detections(
            1,
            &[make_detection("core.codex:tool_use", AgentType::Codex)],
        );
        // session_id comes from extract_session_id which returns None for empty extracted
        // But the code only updates session_id when Some is returned
        // So the original should be preserved
        let meta = c.get_metadata(1).unwrap();
        assert_eq!(meta.session_id.as_deref(), Some("sess-original"));
    }

    #[test]
    fn session_id_updated_when_new_provided() {
        let mut c = AgentCorrelator::new();
        c.ingest_detections(
            1,
            &[make_detection_with_session(
                "core.codex:banner",
                AgentType::Codex,
                "sess-1",
            )],
        );
        c.ingest_detections(
            1,
            &[make_detection_with_session(
                "core.codex:tool_use",
                AgentType::Codex,
                "sess-2",
            )],
        );
        assert_eq!(
            c.get_metadata(1).unwrap().session_id.as_deref(),
            Some("sess-2")
        );
    }

    #[test]
    fn agent_type_updated_on_new_detection() {
        let mut c = AgentCorrelator::new();
        c.ingest_detections(1, &[make_detection("core.codex:banner", AgentType::Codex)]);
        assert_eq!(c.get_metadata(1).unwrap().agent_type, "codex");

        // Different agent type on same pane
        c.ingest_detections(
            1,
            &[make_detection(
                "core.claude_code:banner",
                AgentType::ClaudeCode,
            )],
        );
        assert_eq!(c.get_metadata(1).unwrap().agent_type, "claude_code");
    }

    #[test]
    fn remove_pane_then_redetect() {
        let mut c = AgentCorrelator::new();
        c.ingest_detections(1, &[make_detection("core.codex:banner", AgentType::Codex)]);
        c.remove_pane(1);
        assert!(c.get_metadata(1).is_none());

        // Re-add
        c.ingest_detections(
            1,
            &[make_detection("core.gemini:banner", AgentType::Gemini)],
        );
        assert_eq!(c.get_metadata(1).unwrap().agent_type, "gemini");
    }

    #[test]
    fn ingest_detections_with_events_emits_agent_detected() {
        let mut c = AgentCorrelator::new();
        let events = c.ingest_detections_with_events(
            11,
            &[make_detection("core.codex:banner", AgentType::Codex)],
        );
        assert!(events.iter().any(|event| matches!(
            event,
            Event::AgentDetected {
                pane_id,
                agent_type,
                detection_method
            } if *pane_id == 11 && agent_type == "codex" && detection_method == "pattern_engine"
        )));
    }

    #[test]
    fn test_event_bus_agent_detected() {
        ingest_detections_with_events_emits_agent_detected();
    }

    #[test]
    fn remove_pane_with_event_emits_agent_exited() {
        let mut c = AgentCorrelator::new();
        c.ingest_detections(7, &[make_detection("core.codex:banner", AgentType::Codex)]);
        let event = c.remove_pane_with_event(7, Some(0));
        assert!(matches!(
            event,
            Some(Event::AgentExited {
                pane_id: 7,
                ref agent_type,
                exit_code: Some(0),
            }) if agent_type == "codex"
        ));
    }

    #[test]
    fn test_event_bus_agent_exited() {
        remove_pane_with_event_emits_agent_exited();
    }

    #[test]
    fn inventory_summary_installed_only_and_running() {
        let mut c = AgentCorrelator::with_installed_entries(vec![
            InstalledAgentInventoryEntry {
                slug: "codex".to_string(),
                detected: true,
                evidence: vec![],
                root_paths: vec!["/tmp/codex".to_string()],
                config_path: Some("/tmp/codex".to_string()),
                binary_path: None,
                version: None,
            },
            InstalledAgentInventoryEntry {
                slug: "gemini".to_string(),
                detected: true,
                evidence: vec![],
                root_paths: vec!["/tmp/gemini".to_string()],
                config_path: Some("/tmp/gemini".to_string()),
                binary_path: None,
                version: None,
            },
        ]);
        c.ingest_detections(1, &[make_detection("core.codex:banner", AgentType::Codex)]);

        let inventory = c.inventory();
        let summary = inventory.summary();
        assert_eq!(summary.running_count, 1);
        assert_eq!(summary.installed_count, 2);
        assert_eq!(summary.installed_and_running, vec!["codex".to_string()]);
        assert_eq!(summary.installed_only, vec!["gemini".to_string()]);
        assert_eq!(summary.running_only, Vec::<String>::new());
    }

    #[test]
    fn inventory_agent_by_slug_returns_lookup() {
        let mut c = AgentCorrelator::with_installed_entries(vec![InstalledAgentInventoryEntry {
            slug: "codex".to_string(),
            detected: true,
            evidence: vec!["default root exists: /tmp/codex".to_string()],
            root_paths: vec!["/tmp/codex".to_string()],
            config_path: Some("/tmp/codex".to_string()),
            binary_path: None,
            version: None,
        }]);
        c.ingest_detections(2, &[make_detection("core.codex:banner", AgentType::Codex)]);

        let inventory = c.inventory();
        let lookup = inventory.agent_by_slug("codex").expect("codex lookup");
        assert!(lookup.installed.is_some());
        assert_eq!(lookup.running.len(), 1);
        assert_eq!(lookup.running[0].pane_id, 2);
    }

    #[test]
    fn installed_metadata_missing_does_not_crash() {
        let mut c = AgentCorrelator::with_installed_entries(vec![]);
        c.ingest_detections(
            3,
            &[make_detection(
                "core.claude_code:banner",
                AgentType::ClaudeCode,
            )],
        );
        let inventory = c.inventory();
        let running = inventory.running.get(&3).expect("running pane");
        assert!(running.installation.is_none());
    }
}
