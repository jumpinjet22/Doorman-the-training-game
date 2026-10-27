//! `dorman-ingest`: load batches and publish daily sets.
//!
//!     dorman-ingest migrate
//!     dorman-ingest load batch.json
//!     dorman-ingest review <batch>               # print every item for a skim
//!     dorman-ingest approve <batch>              # let it into daily sets
//!     dorman-ingest daily [--day 2026-10-28]     # default: tomorrow (UTC)
//!     dorman-ingest clear-handle <handle>        # take a reported name away
//!
//! A loaded batch is pending until approved; only approved batches feed
//! daily sets.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use chrono::{Days, NaiveDate, Utc};
use clap::{Parser, Subcommand};
use dorman_core::{pick_daily_set, Batch, BatchItem, DailyPlan};
use dorman_db::StoredItem;

#[derive(Parser)]
#[command(about = "Load batches and publish daily sets")]
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
    /// Load a dorman-batch JSON file. It starts pending.
    Load { path: PathBuf },
    /// Print every item in a loaded batch, for a person to skim.
    Review { batch: String },
    /// Make a reviewed batch eligible for daily sets.
    Approve { batch: String },
    /// Take a leaderboard name away from whoever has it (any case), for a
    /// reported or offensive name. The player keeps playing.
    ClearHandle { handle: String },
    /// Pick and publish the daily set for a day.
    Daily {
        #[arg(long)]
        day: Option<NaiveDate>,
        /// Retire a gold item after this many daily sets.
        #[arg(long, env = "DORMAN_GOLD_SHOWINGS", default_value_t = dorman_db::GOLD_SHOWINGS)]
        gold_showings: i64,
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
            if dorman_db::batch_status(&pool, &batch.batch)
                .await?
                .as_deref()
                == Some("pending")
            {
                println!(
                    "It's pending review: `dorman-ingest review {0}`, then `approve {0}`.",
                    batch.batch
                );
            }
        }
        Command::Review { batch } => {
            let Some(status) = dorman_db::batch_status(&pool, &batch).await? else {
                bail!("no batch called {batch:?} is loaded");
            };
            let items = dorman_db::batch_items(&pool, &batch).await?;
            print!("{}", review(&batch, &status, &items));
        }
        Command::Approve { batch } => {
            if dorman_db::approve_batch(&pool, &batch, Utc::now()).await? {
                println!("Approved {batch}; its items can now go into daily sets.");
            } else {
                match dorman_db::batch_status(&pool, &batch).await? {
                    Some(_) => println!("{batch} was already approved."),
                    None => bail!("no batch called {batch:?} is loaded"),
                }
            }
        }
        Command::ClearHandle { handle } => {
            if dorman_db::clear_handle(&pool, &handle).await? {
                println!("Cleared {handle}; that player is off the leaderboard until they pick a new name.");
            } else {
                bail!("nobody has the name {handle:?}");
            }
        }
        Command::Daily { day, gold_showings } => {
            let day = day.unwrap_or_else(|| Utc::now().date_naive() + Days::new(1));
            let yesterday = day - Days::new(1);
            let pools = dorman_db::daily_pools_retiring(&pool, yesterday, gold_showings).await?;
            let plan = DailyPlan::default();
            let keys = pick_daily_set(
                &day.to_string(),
                plan,
                &pools.new,
                &pools.gold,
                &pools.carry_over,
            );
            if keys.is_empty() {
                bail!("no items to publish for {day}; load and approve a batch first");
            }
            dorman_db::set_daily(&pool, day, &keys).await?;
            println!(
                "Published {} items for {day} (pools: {} new, {} gold, {} carry-over).",
                keys.len(),
                pools.new.len(),
                pools.gold.len(),
                pools.carry_over.len()
            );
            if let Some(warning) = gold_warning(pools.gold.len(), plan.gold) {
                println!("{warning}");
            }
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

/// A warning when gold is running out: retired gold doesn't come back, so a
/// pool that won't last the week needs a batch with known answers loaded.
fn gold_warning(available: usize, per_day: usize) -> Option<String> {
    let week = per_day * 7;
    (available < week).then(|| {
        format!(
            "Warning: only {available} gold items left in rotation (a week takes {week}); \
             load and approve a batch with known answers."
        )
    })
}

/// Everything a reviewer needs to skim a batch: each item as a player would
/// see it, plus the gold answer, if there is one.
fn review(batch: &str, status: &str, items: &[StoredItem]) -> String {
    let mut out = format!("Batch {batch}: {} items, {status}.\n", items.len());
    for (n, stored) in items.iter().enumerate() {
        let (mode, text, scene, generator, gold) = match &stored.item {
            BatchItem::Intent(i) => (
                "intent",
                &i.text,
                &i.scene,
                &i.intended_intent,
                &i.gold_answer,
            ),
            BatchItem::Pair(p) => (
                "pair",
                &p.text,
                &p.scene,
                &p.intended_intent,
                &p.gold_answer,
            ),
        };
        out += &format!("\n[{}/{}] {mode} {}\n", n + 1, items.len(), stored.item_key);
        out += &format!("  visitor: \u{201c}{text}\u{201d}\n");
        if let Some(scene) = scene {
            out += &format!("  scene:   {scene}\n");
        }
        out += &format!("  intent:  {generator}\n");
        match &stored.item {
            BatchItem::Intent(_) => {
                out += &format!("  options: {}\n", stored.options.join(", "));
            }
            BatchItem::Pair(pair) => {
                for (reply, letter) in pair.replies.iter().zip(["A", "B"]) {
                    out += &format!("  reply {letter}: \u{201c}{}\u{201d}\n", reply.text);
                }
            }
        }
        if let Some(gold) = gold {
            out += &format!("  gold:    {gold}\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../fixtures/batch.example.json");

    #[test]
    fn a_short_gold_pool_warns_before_it_runs_out() {
        assert_eq!(gold_warning(21, 3), None);
        let warning = gold_warning(20, 3).unwrap();
        assert!(warning.contains("only 20 gold items left"));
        assert!(gold_warning(0, 3).is_some());
    }

    #[test]
    fn a_review_shows_every_item_in_full() {
        let batch = Batch::parse(EXAMPLE).unwrap();
        let items: Vec<StoredItem> = batch
            .items
            .iter()
            .map(|item| StoredItem {
                id: uuid::Uuid::nil(),
                item_key: item.item_key().into(),
                item: item.clone(),
                options: Vec::new(),
            })
            .collect();
        let text = review(&batch.batch, "pending", &items);
        assert!(text.starts_with(&format!(
            "Batch {}: {} items, pending.",
            batch.batch,
            items.len()
        )));
        for item in &batch.items {
            assert!(text.contains(item.item_key()));
            let line = match item {
                BatchItem::Intent(i) => &i.text,
                BatchItem::Pair(p) => &p.text,
            };
            assert!(text.contains(line.as_str()), "missing {line}");
            if let BatchItem::Pair(pair) = item {
                assert!(pair.replies.iter().all(|r| text.contains(&r.text)));
            }
        }
    }
}
