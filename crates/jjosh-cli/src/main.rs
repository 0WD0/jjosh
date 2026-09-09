mod interop;
mod link;
mod link_metadata;
mod link_refs;
mod native;
mod native_bundle;
mod native_import;
mod projection;
mod transplant;

use jj_cli::cli_util::{CliRunner, CommandHelper};
use jj_cli::command_error::CommandError;
use jj_cli::ui::Ui;

#[derive(clap::Parser, Clone, Debug)]
enum JjoshCommand {
    /// Work with bidirectional Josh history projections.
    Projection(projection::Args),
    /// Compose and publish external repositories through native Josh links.
    Link(link::Args),
    /// Transport and relocate recorded native Jujutsu states.
    Native(native::Args),
}

async fn run_jjosh_command(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    command: JjoshCommand,
) -> Result<(), CommandError> {
    match command {
        JjoshCommand::Projection(args) => projection::run(ui, command_helper, args).await,
        JjoshCommand::Link(args) => link::run(ui, command_helper, args).await,
        JjoshCommand::Native(args) => native::run(ui, command_helper, args).await,
    }
}

fn main() -> std::process::ExitCode {
    CliRunner::init()
        .add_extra_config(link_refs::default_config())
        .name("jjosh")
        .version(env!("CARGO_PKG_VERSION"))
        .add_subcommand(run_jjosh_command)
        .run()
        .into()
}
