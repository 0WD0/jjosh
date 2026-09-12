mod git_remote;
mod git_transport;
mod interop;
mod link;
mod link_fetch;
mod link_metadata;
mod link_refs;
mod native;
mod native_bundle;
mod native_import;
mod native_project;
mod native_source;
mod projection;
mod ref_names;
mod transplant;

use jj_cli::cli_util::CliRunner;
use jj_cli::cli_util::CommandHelper;
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
        .name("jjosh")
        .version(env!("CARGO_PKG_VERSION"))
        .add_git_remote_extension(Box::new(git_remote::Extension))
        .add_subcommand(run_jjosh_command)
        .run()
        .into()
}
