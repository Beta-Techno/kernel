use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct WorkUnit {
    pub version: String,
    pub id: Option<String>,
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

#[derive(Debug, Deserialize)]
pub struct Target {
    pub repo: String,
    pub base_ref: String,
    pub subdir: Option<String>,
    #[serde(default = "default_workspace_mode")]
    pub workspace_mode: WorkspaceMode,
}

impl Target {
    pub fn workspace_mode_str(&self) -> &'static str {
        match self.workspace_mode {
            WorkspaceMode::Worktree => "worktree",
            WorkspaceMode::Clone => "clone",
            WorkspaceMode::Scratch => "scratch",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Agent {
    pub driver: String,
    pub model_hint: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub context_files: Vec<String>,
    pub personality: Option<String>,
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

#[derive(Debug, Deserialize)]
pub enum WorkspaceMode {
    #[serde(rename = "worktree")]
    Worktree,
    #[serde(rename = "clone")]
    Clone,
    #[serde(rename = "scratch")]
    Scratch,
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

#[derive(Debug, Deserialize)]
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
