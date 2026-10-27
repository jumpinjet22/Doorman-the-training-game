//! `dorman-ingest`: load batches and publish daily sets.
//!
//!     dorman-ingest migrate
//!     dorman-ingest load batch.json
//!     dorman-ingest review <batch>               # print every item for a skim
//!     dorman-ingest approve <batch>              # let it into daily sets
//!     dorman-ingest daily [--day 2026-10-28]     # default: tomorrow (UTC)
//!     dorman-ingest clear-handle <handle>        # take a reported name away
//!     dorman-ingest export <dir>                 # training data: intent + DPO
//!
//! A loaded batch is pending until approved; only approved batches feed
//! daily sets, and only they are exported.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{Days, NaiveDate, Utc};
use clap::{Parser, Subcommand};
use dorman_core::export::{self, Crowd, Row, Skip, EXPORT_FORMAT, EXPORT_VERSION};
use dorman_core::{pick_daily_set, Batch, BatchItem, DailyPlan};
use dorman_db::{ExportItem, StoredItem};
use serde_json::json;
use sha2::{Digest, Sha256};

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
    /// Write training data from settled and gold items to a directory:
    /// intent.jsonl, dpo.jsonl and manifest.json. Holds gold answers: never
    /// serve it or commit it.
    Export { dir: PathBuf },
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
        Command::Export { dir } => {
            let items = dorman_db::export_items(&pool).await?;
            let scored_through = dorman_db::latest_scoring_day(&pool).await?;
            let dataset = dataset(&items);
            let manifest = write_export(&dir, &dataset, scored_through, Utc::now())?;
            println!(
                "Exported {} intent and {} DPO rows to {} (skipped: {}).",
                dataset.intent_rows,
                dataset.dpo_rows,
                dir.display(),
                manifest["skipped"]
            );
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

/// The export's files, rendered, and what was left out.
#[derive(Debug, Default)]
struct Dataset {
    intent: String,
    dpo: String,
    intent_rows: usize,
    dpo_rows: usize,
    open: usize,
    disputed: usize,
    both_bad: usize,
}

/// Every exportable item as a JSON line in its file.
fn dataset(items: &[ExportItem]) -> Dataset {
    let mut out = Dataset::default();
    for entry in items {
        let crowd = entry.verdict.as_ref().map(|v| Crowd {
            status: &v.status,
            label: v.label.as_deref(),
            split: &v.split,
        });
        match export::row(&entry.item.item, &entry.batch, crowd, entry.votes) {
            Ok(Row::Intent(row)) => {
                out.intent += &json_line(&row);
                out.intent_rows += 1;
            }
            Ok(Row::Dpo(row)) => {
                out.dpo += &json_line(&row);
                out.dpo_rows += 1;
            }
            Err(Skip::Open) => out.open += 1,
            Err(Skip::Disputed) => out.disputed += 1,
            Err(Skip::BothBad) => out.both_bad += 1,
        }
    }
    out
}

/// A row as one line of JSON.
fn json_line<T: serde::Serialize>(row: &T) -> String {
    let mut text = serde_json::to_string(row).expect("rows always serialise");
    text.push('\n');
    text
}

/// Writes `name` in `dir` through a temporary file and a rename, so a job
/// reading the directory never sees half a file. Returns its SHA-256.
fn write_atomic(dir: &Path, name: &str, text: &str) -> Result<String> {
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join(name)).with_context(|| format!("renaming {}", tmp.display()))?;
    Ok(Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Writes the dataset and its manifest (last, so a manifest always describes
/// files already in place). Returns the manifest.
fn write_export(
    dir: &Path,
    dataset: &Dataset,
    scored_through: Option<NaiveDate>,
    now: chrono::DateTime<Utc>,
) -> Result<serde_json::Value> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let intent = write_atomic(dir, "intent.jsonl", &dataset.intent)?;
    let dpo = write_atomic(dir, "dpo.jsonl", &dataset.dpo)?;
    let manifest = json!({
        "format": EXPORT_FORMAT,
        "version": EXPORT_VERSION,
        "generated_at": now.to_rfc3339(),
        "scored_through": scored_through,
        "files": {
            "intent.jsonl": { "rows": dataset.intent_rows, "sha256": intent },
            "dpo.jsonl": { "rows": dataset.dpo_rows, "sha256": dpo },
        },
        "skipped": {
            "open": dataset.open,
            "disputed": dataset.disputed,
            "both_bad": dataset.both_bad,
        },
    });
    let mut text = serde_json::to_string_pretty(&manifest)?;
    text.push('\n');
    write_atomic(dir, "manifest.json", &text)?;
    Ok(manifest)
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

    fn stored(item: &BatchItem) -> StoredItem {
        StoredItem {
            id: uuid::Uuid::nil(),
            item_key: item.item_key().into(),
            item: item.clone(),
            options: Vec::new(),
        }
    }

    /// Every example item, settled on its gold answer or generator label
    /// (the first reply for pairs), except one left open and one disputed.
    fn scored_example() -> Vec<ExportItem> {
        let batch = Batch::parse(EXAMPLE).unwrap();
        batch
            .items
            .iter()
            .enumerate()
            .map(|(n, item)| {
                let label = match item {
                    BatchItem::Intent(i) => i.intended_intent.clone(),
                    BatchItem::Pair(p) => p.replies[0].item_key.clone(),
                };
                let status = match n {
                    1 => "open",
                    2 => "disputed",
                    _ => "settled",
                };
                ExportItem {
                    item: stored(item),
                    batch: batch.batch.clone(),
                    verdict: Some(dorman_db::Verdict {
                        status: status.into(),
                        label: Some(label.clone()),
                        split: vec![(label, 3.0)],
                    }),
                    votes: 5,
                }
            })
            .collect()
    }

    #[test]
    fn the_dataset_puts_each_item_in_its_file_or_counts_why_not() {
        let items = scored_example();
        let data = dataset(&items);
        let gold = |n: usize| items[n].item.item.gold_answer().is_some();
        // Items 1 and 2 are skipped unless gold, which is always exported.
        let skipped = [1, 2].iter().filter(|&&n| !gold(n)).count();
        assert_eq!(data.intent_rows + data.dpo_rows, items.len() - skipped);
        assert_eq!(data.open + data.disputed, skipped);
        assert_eq!(data.intent.lines().count(), data.intent_rows);
        assert_eq!(data.dpo.lines().count(), data.dpo_rows);
        assert!(data.dpo_rows > 0, "the example has settled pairs");
        for line in data.intent.lines().chain(data.dpo.lines()) {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(row["votes"], 5);
        }
        for line in data.dpo.lines() {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_ne!(row["chosen"], row["rejected"]);
            assert!(row["prompt"].as_str().unwrap().contains("Visitor:"));
        }
    }

    #[test]
    fn an_export_writes_its_files_and_a_manifest_that_matches_them() {
        let dir = std::env::temp_dir().join(format!("dorman-export-{}", std::process::id()));
        let data = dataset(&scored_example());
        let now = Utc::now();
        let manifest = write_export(&dir, &data, Some(NaiveDate::MIN), now).unwrap();
        // Again over the top, as the nightly job would.
        write_export(&dir, &data, None, now).unwrap();

        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(on_disk["format"], EXPORT_FORMAT);
        assert_eq!(on_disk["scored_through"], serde_json::Value::Null);
        assert_eq!(manifest["files"], on_disk["files"]);
        for (name, rows) in [
            ("intent.jsonl", data.intent_rows),
            ("dpo.jsonl", data.dpo_rows),
        ] {
            let text = std::fs::read_to_string(dir.join(name)).unwrap();
            let sha: String = Sha256::digest(text.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(on_disk["files"][name]["sha256"], sha.as_str());
            assert_eq!(on_disk["files"][name]["rows"], rows);
        }
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
