use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
pub struct WorkUnit {
    pub version: String,
    pub id: Option<String>,
    pub lineage: Option<Lineage>,
    pub kind: String,
    pub target: Target,
    pub agent: Agent,
    pub env: Env,
    pub authority: Authority,
    pub tools: Tools,
    pub budgets: Budgets,
    pub acceptance: Acceptance,
    pub outputs: Outputs,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Lineage {
    pub workflow_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    pub agent_id: String,
}

#[derive(Debug, Deserialize)]
pub struct Target {
    pub repo: String,
    pub base_ref: String,
    pub subdir: Option<String>,
    #[serde(default = "default_workspace_mode")]
    pub workspace_mode: WorkspaceMode,
}

impl Target {
    pub fn branch_slug(&self, run_id: &str) -> String {
        // Use a slash-free, git-safe branch name to avoid ref namespace collisions.
        let run = sanitize_ref_component(run_id, "run");
        let slug = sanitize_ref_component(self.subdir.as_deref().unwrap_or("default"), "default");
        let name = format!("agentctl-run-{run}-{slug}");
        truncate_with_hash(name, BRANCH_MAX_LEN)
    }
}

const BRANCH_MAX_LEN: usize = 200;

fn sanitize_ref_component(raw: &str, default: &str) -> String {
    let mut out = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    out = out
        .trim_matches(|c: char| c == '-' || c == '_' || c == '.')
        .to_string();
    if out.is_empty() {
        return default.to_string();
    }
    if out.starts_with('-') {
        out = format!("x{out}");
    }
    while out.contains("..") {
        out = out.replace("..", ".");
    }
    if out.ends_with(".lock") {
        out.push_str("-x");
    }
    out
}

fn truncate_with_hash(value: String, max_len: usize) -> String {
    if value.len() <= max_len {
        return value;
    }
    let hash = short_hash(&value);
    let keep = max_len.saturating_sub(hash.len() + 1);
    let mut prefix = value[..keep]
        .trim_end_matches(|c: char| c == '-' || c == '_' || c == '.')
        .to_string();
    if prefix.is_empty() {
        prefix = "ref".to_string();
    }
    format!("{prefix}-{hash}")
}

fn short_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let full = hex::encode(hasher.finalize());
    full[..12].to_string()
}

#[derive(Debug, Deserialize)]
pub struct Agent {
    pub driver: String,
    pub model_hint: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub context_files: Vec<String>,
    pub personality: Option<String>,
    pub resume_session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Env {
    #[serde(default = "default_env_profile")]
    pub profile: EnvProfile,
    #[serde(default)]
    pub setup: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Authority {
    pub mode: u8,
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Deserialize)]
pub struct Capability {
    pub name: String,
    pub scope: Option<String>,
    pub ttl_seconds: Option<u64>,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct Tools {
    pub mcp_profile: String,
    #[serde(default = "default_command_policy")]
    pub command_policy: CommandPolicy,
    #[serde(default = "default_network_policy")]
    pub network: NetworkPolicy,
}

#[derive(Debug, Deserialize)]
pub struct Budgets {
    pub wall_seconds: u64,
    pub max_tool_calls: Option<u64>,
    pub max_commands: Option<u64>,
    pub max_bytes_written: Option<u64>,
    pub max_diff_lines: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct Acceptance {
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub receipts_required: bool,
}

#[derive(Debug, Deserialize)]
pub struct Outputs {
    #[serde(default = "default_true")]
    pub want_patch: bool,
    #[serde(default = "default_true")]
    pub want_commits: bool,
    #[serde(default = "default_true")]
    pub want_handoff: bool,
    #[serde(default)]
    pub push_branch: bool,
    #[serde(default)]
    pub open_pr: bool,
}

#[derive(Debug, Deserialize, Copy, Clone, PartialEq, Eq)]
pub enum WorkspaceMode {
    #[serde(rename = "worktree")]
    Worktree,
    #[serde(rename = "clone")]
    Clone,
    #[serde(rename = "scratch")]
    Scratch,
}

impl WorkspaceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceMode::Worktree => "worktree",
            WorkspaceMode::Clone => "clone",
            WorkspaceMode::Scratch => "scratch",
        }
    }
}

fn default_workspace_mode() -> WorkspaceMode {
    WorkspaceMode::Worktree
}

#[derive(Debug, Deserialize)]
pub enum EnvProfile {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "devcontainer")]
    Devcontainer,
    #[serde(rename = "nix")]
    Nix,
    #[serde(rename = "mise")]
    Mise,
    #[serde(rename = "native")]
    Native,
}

fn default_env_profile() -> EnvProfile {
    EnvProfile::Auto
}

#[derive(Debug, Deserialize, Copy, Clone, PartialEq, Eq)]
pub enum CommandPolicy {
    #[serde(rename = "deny")]
    Deny,
    #[serde(rename = "safe-default")]
    SafeDefault,
    #[serde(rename = "allow-listed")]
    AllowListed,
    #[serde(rename = "full")]
    Full,
}

fn default_command_policy() -> CommandPolicy {
    CommandPolicy::SafeDefault
}

#[derive(Debug, Deserialize)]
pub enum NetworkPolicy {
    #[serde(rename = "deny")]
    Deny,
    #[serde(rename = "egress-limited")]
    EgressLimited,
    #[serde(rename = "allow")]
    Allow,
}

fn default_network_policy() -> NetworkPolicy {
    NetworkPolicy::Deny
}

fn default_true() -> bool {
    true
}
