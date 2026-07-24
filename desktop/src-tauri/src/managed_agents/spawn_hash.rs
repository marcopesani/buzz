//! Spawn-time config hash for the restart-required badge.
//!
//! [`spawn_config_hash`] digests the *effective spawned values* — what a
//! process launch of `record` would actually receive — so the UI can compare
//! a running process's hash (stamped on [`super::ManagedAgentProcess`] at
//! spawn) against a recomputation from current disk state and show a
//! "restart required" badge only when a restart would change what runs.
//!
//! Scope rules (decided in #centralize-personas-and-agents, revised in PR
//! #1602 review):
//! - Inputs mirror what a start would actually run: the start/restore paths
//!   re-snapshot the linked persona's prompt/model/provider/env onto the
//!   record immediately before spawning (`start_local_agent_with_preflight`,
//!   `restore_managed_agents_on_launch`), so persona edits to those fields DO
//!   apply on a plain restart and are hashed via the same prospective
//!   re-snapshot. Harness command, args/mcp, env layering, and the record
//!   fields the spawn env writes read are hashed as spawn resolves them.
//! - The relay URL is hashed in resolved form (`effective_agent_relay_url`):
//!   every record spawns against the active workspace relay (legacy per-record
//!   pins are ignored), so a workspace relay change means a restart would
//!   change what runs.
//! - Channel membership is not an input: agents pick up channel changes live
//!   (#1468), never via restart.
//! - Wallet presence is a spawn-identity bit: `BUZZ_NWC_URI` is injected from
//!   the keyring at launch, so provision/unprovision must flip the hash and
//!   the existing `needs_restart` badge. Callers pass a bool resolved from the
//!   keyring (never the raw NWC URI — presence only, no recoverable secret).
//!
//! The hash never crosses a process or persistence boundary, so
//! `DefaultHasher` (not stable across Rust releases) is sufficient.

use std::hash::{DefaultHasher, Hash, Hasher};

use super::{
    known_acp_runtime, normalize_agent_args,
    persona_events::apply_persona_snapshot,
    resolve_effective_agent_env,
    types::{AgentDefinition, ManagedAgentRecord, TeamRecord},
    GlobalAgentConfig,
};

/// The prompt a spawn would actually deliver: `Some("")` collapses to `None`
/// because an empty `BUZZ_ACP_SYSTEM_PROMPT` is no prompt.
///
/// The single source of truth for the spawn env write AND the config hash.
pub(crate) fn effective_spawn_prompt(record: &ManagedAgentRecord) -> Option<String> {
    record
        .system_prompt
        .clone()
        .filter(|prompt| !prompt.is_empty())
}

/// Resolve the current instructions for this instance's deployment-time team binding.
/// A deleted team deliberately degrades to no team section.
pub(crate) fn effective_team_instructions(
    record: &ManagedAgentRecord,
    teams: &[TeamRecord],
) -> Option<String> {
    teams
        .iter()
        .find(|team| Some(team.id.as_str()) == record.team_id.as_deref())
        .and_then(|team| team.instructions.as_deref())
        .map(str::trim)
        .filter(|instructions| !instructions.is_empty())
        .map(str::to_string)
}

/// Digest the effective spawn configuration of `record` under the current
/// `personas`, resolving a blank record relay against `workspace_relay`.
///
/// Pure — no `AppHandle`, no disk, no keyring. Callers resolve
/// `wallet_provisioned` from a single spawn observation
/// ([`crate::wallet::observe_agent_nwc_for_spawn`]) and pass the bit; the raw
/// NWC URI never enters the hasher.
pub(crate) fn spawn_config_hash(
    record: &ManagedAgentRecord,
    personas: &[AgentDefinition],
    teams: &[TeamRecord],
    workspace_relay: &str,
    global: &GlobalAgentConfig,
    wallet_provisioned: bool,
) -> u64 {
    // Prospective re-snapshot: apply the same `apply_persona_snapshot` the
    // start/restore paths run right before spawning, so the hash covers what a
    // restart would actually run. Idempotent, so the spawn-time stamp
    // (post-snapshot record) and later recomputes (persisted record) agree
    // when nothing changed. The persona env itself reaches the hash through
    // `resolve_effective_agent_env` below; `persona_source_version` is set on
    // the clone but is not a hash input.
    let mut record = record.clone();
    if let Some(persona_id) = record.persona_id.clone() {
        if let Some(persona) = personas.iter().find(|p| p.id == persona_id) {
            apply_persona_snapshot(&mut record, persona);
        }
    }
    let record = &record;

    let effective_command = crate::managed_agents::record_agent_command(record, personas);
    let runtime_meta = known_acp_runtime(&effective_command);
    let effective = resolve_effective_agent_env(record, personas, runtime_meta, global);

    let mut hasher = DefaultHasher::new();

    // Harness identity and derivations (live-persona-resolved, like spawn).
    record.acp_command.hash(&mut hasher);
    effective_command.hash(&mut hasher);
    normalize_agent_args(&effective_command, record.agent_args.clone()).hash(&mut hasher);
    runtime_meta
        .and_then(|r| r.mcp_command)
        .unwrap_or("")
        .hash(&mut hasher);

    // Effective env layering (baked floor → runtime metadata → user env).
    // BTreeMap iteration is ordered, so this is deterministic.
    effective.env.hash(&mut hasher);

    // Record fields the spawn env writes read directly. The relay is hashed
    // resolved: every record spawns on the workspace relay (legacy pins
    // ignored), so a workspace relay change must trip the badge.
    crate::relay::effective_agent_relay_url(&record.relay_url, workspace_relay).hash(&mut hasher);
    // Prompt and runtime-layered team instructions use the same resolver as spawn.
    effective_spawn_prompt(record).hash(&mut hasher);
    effective_team_instructions(record, teams).hash(&mut hasher);
    record.model.hash(&mut hasher);
    record.provider.hash(&mut hasher);
    record.auth_tag.hash(&mut hasher);
    record.respond_to.as_str().hash(&mut hasher);
    // The allowlist is hashed as the env receives it: spawn sets
    // BUZZ_ACP_RESPOND_TO_ALLOWLIST only in allowlist mode, and normalized
    // (trim/lowercase/dedup via `validate_respond_to_allowlist`) — so edits
    // that don't survive normalization, or edits while another mode is
    // active, must not badge. A list spawn would reject hashes raw: the
    // stamped hash comes from a successful spawn, so any invalid edit
    // correctly compares unequal.
    if record.respond_to == super::types::RespondTo::Allowlist {
        super::types::validate_respond_to_allowlist(&record.respond_to_allowlist)
            .unwrap_or_else(|_| record.respond_to_allowlist.clone())
            .hash(&mut hasher);
    }
    record.idle_timeout_seconds.hash(&mut hasher);
    record.max_turn_duration_seconds.hash(&mut hasher);
    record.parallelism.hash(&mut hasher);

    // Receive-only NWC presence (keyring `agent-nwc:{pubkey}` → `BUZZ_NWC_URI`).
    // Bool only — never the URI. Provision/unprovision flips needs_restart.
    wallet_provisioned.hash(&mut hasher);

    hasher.finish()
}

/// Whether a stamped spawn hash has drifted from current effective config.
///
/// `wallet_provisioned` is `Ok(bit)` from a successful keyring observe, or
/// `Err(_)` when the keyring is unavailable. On `Err`, drift is non-flapping:
/// badge only if the stamp matches **neither** presence polarity (i.e. some
/// non-wallet input changed). Transient secret-store failures must not flip
/// `needs_restart` by themselves.
pub(crate) fn spawn_config_has_drifted(
    stamped_hash: u64,
    record: &ManagedAgentRecord,
    personas: &[AgentDefinition],
    teams: &[TeamRecord],
    workspace_relay: &str,
    global: &GlobalAgentConfig,
    wallet_provisioned: Result<bool, String>,
) -> bool {
    match wallet_provisioned {
        Ok(bit) => {
            stamped_hash != spawn_config_hash(record, personas, teams, workspace_relay, global, bit)
        }
        Err(_) => {
            let with_wallet =
                spawn_config_hash(record, personas, teams, workspace_relay, global, true);
            let without_wallet =
                spawn_config_hash(record, personas, teams, workspace_relay, global, false);
            stamped_hash != with_wallet && stamped_hash != without_wallet
        }
    }
}

#[cfg(test)]
mod tests;
