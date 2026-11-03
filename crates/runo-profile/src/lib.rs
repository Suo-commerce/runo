//! Profile and chorus manifest TOML with load-time checks (technical spec §10).
//! A profile holds exactly four things: role, state machine, listening graph, cadence source.

use serde::Deserialize;

/// Failure to load or validate a profile or manifest.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// TOML could not be parsed.
    #[error("parse: {0}")]
    Parse(String),
    /// A load-time check failed; the message names the offending key.
    #[error("check: {0}")]
    Check(String),
}

/// A voice's profile.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Role name, e.g. `lane`, `sentinel`, `herald`, `owner`.
    pub role: String,
    /// Refrain interval, e.g. `"60s"`; `None` for voices that never refrain.
    pub refrain_interval: Option<String>,
    /// Probe interval.
    pub probe_interval: String,
    /// `process`, `host` or `replicated:<k>`.
    pub durability: String,
    /// The state machine.
    pub states: States,
    /// Cadence configuration.
    pub cadence: CadenceConfig,
    /// Roles this voice keeps clocks for.
    pub listen: Listen,
    /// Fenced resources and effects.
    pub fenced: Fenced,
}

/// A role's state machine.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct States {
    /// Initial state.
    pub initial: String,
    /// Terminal states.
    pub terminal: Vec<String>,
    /// Allowed transitions; `*` matches any state.
    pub transitions: Vec<(String, String)>,
}

/// Cadence configuration: `fixed` values per state, or `corpus` with fixed fallbacks.
#[derive(Clone, Debug, Deserialize)]
pub struct CadenceConfig {
    /// `fixed` or `corpus`.
    pub source: String,
    /// Escalation multipliers, e.g. `[1.0, 1.5, 2.0]`.
    pub escalate: Vec<f32>,
    /// Per-state cadence in minutes.
    #[serde(flatten)]
    pub per_state: std::collections::BTreeMap<String, toml::Value>,
}

/// Listening graph.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listen {
    /// Roles to keep clocks for.
    pub to: Vec<String>,
}

/// Fenced resources and external effects.
#[derive(Clone, Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Fenced {
    /// Resources the allocator installs fences on.
    #[serde(default)]
    pub resources: Vec<String>,
    /// External effects deduplicated by `(task, operation_id)`.
    #[serde(default)]
    pub keyed: Vec<String>,
    /// Effects accepted as a risk, named.
    #[serde(default)]
    pub accepted_risk: Vec<String>,
}

/// Parse a profile and run the load-time checks.
///
/// # Errors
/// Returns `ProfileError` naming the offending key.
pub fn load_profile(toml_text: &str) -> Result<Profile, ProfileError> {
    let p: Profile = toml::from_str(toml_text).map_err(|e| ProfileError::Parse(e.to_string()))?;
    check_profile(&p)?;
    Ok(p)
}

fn check_profile(p: &Profile) -> Result<(), ProfileError> {
    let known: std::collections::BTreeSet<&str> = p
        .states
        .transitions
        .iter()
        .flat_map(|(a, b)| [a.as_str(), b.as_str()])
        .chain([p.states.initial.as_str()])
        .chain(p.states.terminal.iter().map(String::as_str))
        .collect();
    if !known.contains(p.states.initial.as_str()) {
        return Err(ProfileError::Check(
            "states.initial is not a declared state".into(),
        ));
    }
    if p.cadence.escalate.len() != 3 {
        return Err(ProfileError::Check(
            "cadence.escalate must have three values".into(),
        ));
    }
    Ok(())
}
