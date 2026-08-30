//! Baseline management commands.

use crate::cli::{BaselineAction, BaselineArgs};
use crate::error::Result;
use crate::store::Store;

pub async fn execute(args: BaselineArgs) -> Result<()> {
    let store = Store::open(args.db.as_deref())?;
    match args.action {
        BaselineAction::Tag { run_id, name } => {
            store.tag_baseline(&run_id, &name)?;
            println!("tagged run {run_id} as baseline '{name}'");
        }
        BaselineAction::Delete { name } => {
            store.delete_baseline(&name)?;
            println!("deleted baseline '{name}'");
        }
        BaselineAction::Show { name } => {
            let run = store.get_baseline(&name)?;
            println!("{}", serde_json::to_string_pretty(&run)?);
        }
    }
    Ok(())
}
