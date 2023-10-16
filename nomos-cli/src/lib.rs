pub mod cmds;

pub(crate) mod da;
use clap::Parser;
use cmds::Command;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    pub fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        self.command.run()
    }

    pub fn command(&self) -> &Command {
        &self.command
    }
}
