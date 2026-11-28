pub mod app;
pub mod cli;
pub mod completion;
pub mod config;
pub mod envvault;
pub mod history;
pub mod ime;
pub mod input;
pub mod json;
pub mod knowledge;
pub mod ledger;
pub mod models;
pub mod pathpick;
pub mod refs;
pub mod select;
pub mod session;
pub mod skills;
pub mod sys;
pub mod tools;
pub mod ui;
pub mod updater;

#[cfg(test)]
mod tests;

fn main() -> anyhow::Result<()> {
    cli::run(std::env::args().skip(1).collect())
}
