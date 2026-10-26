//! `dorman-ingest`: load KNOCK batches and publish daily sets.
//!
//!     dorman-ingest migrate
//!     dorman-ingest load batch.json
//!     dorman-ingest daily [--day 2026-10-28]     # default: tomorrow (UTC)

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use chrono::{Days, NaiveDate, Utc};
use clap::{Parser, Subcommand};
use dorman_core::{pick_daily_set, Batch, DailyPlan};

#[derive(Parser)]
#[command(about = "Load KNOCK batches and publish daily sets")]
struct Cli {
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Apply database migrations.
    Migrate,
    /// Load a batch.json from KNOCK's export_game_batch.py.
    Load { path: PathBuf },
    /// Pick and publish the daily set for a day.
    Daily {
        #[arg(long)]
        day: Option<NaiveDate>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let pool = dorman_db::connect(&cli.database_url)
        .await
        .context("connecting to Postgres")?;
    match cli.command {
        Command::Migrate => {
            dorman_db::migrate(&pool).await?;
            println!("Migrations applied.");
        }
        Command::Load { path } => {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let batch =
                Batch::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
            let mut added = 0;
            for item in &batch.items {
                if dorman_db::insert_item(&pool, &batch.batch, &batch.intents, item).await? {
                    added += 1;
                }
            }
            println!(
                "Batch {}: {added} new items, {} already loaded.",
                batch.batch,
                batch.items.len() - added
            );
        }
        Command::Daily { day } => {
            let day = day.unwrap_or_else(|| Utc::now().date_naive() + Days::new(1));
            let yesterday = day - Days::new(1);
            let pools = dorman_db::daily_pools(&pool, yesterday).await?;
            let plan = DailyPlan::default();
            let keys = pick_daily_set(
                &day.to_string(),
                plan,
                &pools.new,
                &pools.gold,
                &pools.carry_over,
            );
            if keys.is_empty() {
                bail!("no items to publish for {day}; load a batch first");
            }
            dorman_db::set_daily(&pool, day, &keys).await?;
            println!(
                "Published {} items for {day} (pools: {} new, {} gold, {} carry-over).",
                keys.len(),
                pools.new.len(),
                pools.gold.len(),
                pools.carry_over.len()
            );
            if keys.len() < plan.total() {
                println!(
                    "Warning: short of the planned {}; load more data.",
                    plan.total()
                );
            }
        }
    }
    Ok(())
}
