mod binding_config;
mod git_remote;
mod git_transport;
mod interop;
mod native_import;
mod native_project;
mod native_source;
mod project;
mod project_config;
mod project_import;
mod project_migration;
mod project_preserve;
mod project_provenance;
mod projection;
mod projection_history;
mod ref_names;
mod remote_refs;
mod source_repo;

use jj_cli::cli_util::CliRunner;
use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::CommandError;
use jj_cli::ui::Ui;

#[derive(clap::Parser, Clone, Debug)]
enum JjoshCommand {
    /// Register, inspect, and import operation-owned subprojects.
    Project(project::Args),
    /// Work with bidirectional Josh history projections.
    Projection(projection::Args),
}

async fn run_jjosh_command(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    command: JjoshCommand,
) -> Result<(), CommandError> {
    match command {
        JjoshCommand::Project(args) => project::run(ui, command_helper, args).await,
        JjoshCommand::Projection(args) => projection::run(ui, command_helper, args).await,
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
