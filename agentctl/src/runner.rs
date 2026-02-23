use anyhow::Result;

use crate::run_dir::RunPaths;
use crate::work_unit::WorkUnit;

pub fn execute(_wu: &WorkUnit, _paths: &RunPaths) -> Result<()> {
    // Placeholder: future implementation will spawn codex exec, capture events, etc.
    Ok(())
}
