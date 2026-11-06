use std::env;

use clap::Args;
use gir_core::{GirError, Repository};
use gir_tui::run_tui;

#[derive(Debug, Args)]
pub struct TuiArgs {}

pub fn run(_args: TuiArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;
    run_tui(repo)
}
