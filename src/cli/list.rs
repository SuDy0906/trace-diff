//! `trace-diff list` command.

use crate::cli::ListArgs;
use crate::error::Result;
use crate::store::Store;

pub async fn execute(args: ListArgs) -> Result<()> {
    let store = Store::open(args.db.as_deref())?;

    if !args.baselines_only {
        println!("=== Recent runs ===");
        for run in store.list_runs(20)? {
            println!(
                "  {}  {}  {}  l7={} hops={}",
                run.created_at.to_rfc3339(),
                &run.id[..8.min(run.id.len())],
                run.target,
                run.l7.is_some(),
                run.trace.as_ref().map(|t| t.hops.len()).unwrap_or(0),
            );
        }
        println!();
    }

    println!("=== Baselines ===");
    let baselines = store.list_baselines()?;
    if baselines.is_empty() {
        println!("  (none)");
    } else {
        for b in baselines {
            println!(
                "  {:<24}  run={}  target={}  {}",
                b.name,
                &b.run_id[..8.min(b.run_id.len())],
                b.target,
                b.created_at.to_rfc3339(),
            );
        }
    }
    Ok(())
}
