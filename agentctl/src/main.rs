mod run_dir;
mod run_id;
mod runner;
mod work_unit;

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use run_dir::provision;
use work_unit::WorkUnit;

/// CLI for submitting WorkUnits defined by runfmt/0.1.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// Path to a WorkUnit file (JSON or TOML).
    #[arg(long, value_name = "FILE")]
    spec: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut buf = Vec::new();
    File::open(&cli.spec)
        .with_context(|| format!("unable to open spec {:?}", cli.spec))?
        .read_to_end(&mut buf)?;

    let work_unit: WorkUnit = if cli
        .spec
        .extension()
        .map(|ext| ext == "toml")
        .unwrap_or(false)
    {
        toml::from_str(std::str::from_utf8(&buf)?).context("failed to parse WorkUnit TOML")?
    } else {
        serde_json::from_slice(&buf).context("failed to parse WorkUnit JSON")?
    };

    if work_unit.version != "runfmt/0.1" {
        anyhow::bail!("unsupported runfmt version: {}", work_unit.version);
    }

    let run_id = work_unit.id.clone().unwrap_or_else(|| run_id::new_run_id());

    let paths = provision(&run_id, work_unit.target.workspace_mode_str())?;

    println!("run {} staged at {:?}", run_id, paths.run_dir);

    Ok(())
}
