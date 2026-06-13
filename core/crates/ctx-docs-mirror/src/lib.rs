mod cli;
mod config;
mod crawler;
mod http;
mod output;
mod plan;
mod repo;
mod sitemap;

use std::fs;

use anyhow::{anyhow, Context, Result};
use clap::Parser;

pub async fn run() -> Result<()> {
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Commands::Plan { config } => {
            let cfg = config::load_config(&config)?;
            let client = http::http_client()?;
            let plan = plan::resolve_plan(&client, &cfg).await?;
            let manifest = plan::manifest_from_plan(&cfg, &plan);
            let txt = serde_json::to_string_pretty(&manifest)?;
            println!("{txt}");
        }
        cli::Commands::Mirror { config, out, clean } => {
            let cfg = config::load_config(&config)?;
            let client = http::http_client()?;
            let plan = plan::resolve_plan(&client, &cfg).await?;
            let out_dir = out
                .or_else(|| std::env::var_os("CTX_DOCS_OUTPUT_DIR").map(std::path::PathBuf::from))
                .ok_or_else(|| {
                    anyhow!("output directory required (--out or CTX_DOCS_OUTPUT_DIR)")
                })?;
            if clean {
                output::clean_output_dir(&out_dir)?;
            }
            fs::create_dir_all(&out_dir)?;
            output::execute_plan(&client, &cfg, &plan, &out_dir).await?;
            let manifest = plan::manifest_from_plan(&cfg, &plan);
            let manifest_path = out_dir.join("manifest.json");
            fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)
                .with_context(|| format!("writing {}", manifest_path.display()))?;
        }
    }
    Ok(())
}
