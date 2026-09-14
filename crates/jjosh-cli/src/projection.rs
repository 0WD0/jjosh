use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use crate::interop::{
    check_projectable_repo_history, commit_as_josh_oid, open_josh_transaction, sha1_git_repo_path,
};
use jj_cli::cli_util::{CommandHelper, RevisionArg, WorkspaceCommandHelper};
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_cli::ui::Ui;
use jj_lib::repo::Repo as _;

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Clone, Debug)]
enum Command {
    /// Preview an arbitrary filter or versioned view without changing recorded state.
    Preview(PreviewArgs),
}

#[derive(clap::Args, Clone, Debug)]
struct OutputArgs {
    /// Emit structured JSON instead of human-readable output.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args, Clone, Debug)]
struct FilterArgs {
    /// Josh filter expression, for example `:/services/api`.
    #[arg(required_unless_present = "view", conflicts_with = "view")]
    filter: Option<String>,

    /// Versioned projection view at this repository-relative path.
    ///
    /// Reads Josh's workspace.josh from each historical version of the path.
    /// This selects a history/layout view, not a Jujutsu working copy.
    #[arg(long, value_name = "REPO_PATH")]
    view: Option<String>,
}

impl FilterArgs {
    fn resolve(&self) -> Result<josh_core::filter::Filter, CommandError> {
        if let Some(value) = &self.view {
            if value.is_empty() || value.contains('\0') {
                return Err(user_error("View paths must not be empty or contain NUL"));
            }
            let mut path = PathBuf::new();
            for component in Path::new(value).components() {
                match component {
                    Component::Normal(name) => path.push(name),
                    Component::CurDir => {}
                    _ => {
                        return Err(user_error(
                            "View path must be repository-relative and cannot contain ..",
                        ));
                    }
                }
            }
            Ok(josh_core::filter::Filter::new().workspace(path))
        } else {
            josh_core::filter::parse(
                self.filter
                    .as_deref()
                    .expect("clap requires a filter or view"),
            )
            .map_err(|err| user_error_with_message("Invalid Josh projection", err))
        }
    }
}

#[derive(clap::Args, Clone, Debug)]
#[command(group(
    clap::ArgGroup::new("selection").required(true).args(["filter", "view"])
))]
struct PreviewArgs {
    /// Josh filter expression applied to the selected recorded revision.
    filter: Option<String>,
    /// Versioned workspace.josh view, relative to the repository root.
    #[arg(long)]
    view: Option<String>,

    /// Revision to project.
    #[arg(short = 'r', long, default_value = "@")]
    revision: RevisionArg,
    /// List files in the resulting view, relative to its root.
    #[arg(long)]
    files: bool,
    /// List commits and parent IDs in the resulting view history.
    #[arg(long)]
    history: bool,
    #[command(flatten)]
    output: OutputArgs,
}

pub(crate) async fn run(
    ui: &mut Ui,
    command: &CommandHelper,
    args: Args,
) -> Result<(), CommandError> {
    match args.command {
        Command::Preview(args) => run_preview(ui, command, args).await,
    }
}

async fn recorded_workspace(
    ui: &Ui,
    command_helper: &CommandHelper,
) -> Result<WorkspaceCommandHelper, CommandError> {
    // The ordinary no-snapshot helper can still merge divergent operation heads.
    // Preview/configuration need the recorded view, not a working-copy mutation.
    let workspace = command_helper.load_workspace()?;
    let loader = workspace.repo_loader();
    let operation = if let Some(op) = &command_helper.global_args().at_operation {
        jj_lib::op_walk::resolve_op_for_load(loader, op).await?
    } else {
        let heads = jj_lib::op_walk::get_current_head_ops(
            loader.op_store(),
            loader.op_heads_store().as_ref(),
        )
        .await?;
        let [operation] = heads.as_slice() else {
            return Err(user_error(
                "Projection requires one recorded operation head; reconcile operations separately or select --at-operation for read-only inspection",
            ));
        };
        operation.clone()
    };
    let repo = loader.load_at(&operation).await?;
    command_helper.for_workable_repo(ui, workspace, repo)
}

fn write_json(ui: &mut Ui, value: &impl serde::Serialize) -> Result<(), CommandError> {
    serde_json::to_writer_pretty(ui.stdout(), value).map_err(user_error)?;
    writeln!(ui.stdout())?;
    Ok(())
}

#[derive(serde::Serialize)]
struct Preview {
    filter: String,
    source_commit: String,
    source_change_id: String,
    projected_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history: Option<Vec<PreviewCommit>>,
}

#[derive(serde::Serialize)]
struct PreviewCommit {
    commit: String,
    parents: Vec<String>,
    description: String,
}

fn preview_files(
    transaction: &josh_core::cache::Transaction,
    head: gix_hash::ObjectId,
) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    if head.is_null() {
        return Ok(files);
    }
    let tree = josh_core::objects::CommitData::read(transaction.odb(), head)?.tree_id()?;
    let mut pending = vec![(String::new(), tree)];
    while let Some((prefix, tree)) = pending.pop() {
        for entry in josh_core::objects::read_tree_entries(transaction.odb(), tree)? {
            let name = std::str::from_utf8(&entry.filename)?;
            let path = format!("{prefix}{name}");
            if entry.mode.is_tree() {
                pending.push((format!("{path}/"), entry.oid));
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn preview_history(
    transaction: &josh_core::cache::Transaction,
    head: gix_hash::ObjectId,
) -> anyhow::Result<Vec<PreviewCommit>> {
    let mut commits = Vec::new();
    if head.is_null() {
        return Ok(commits);
    }
    let mut seen = std::collections::HashSet::new();
    let mut pending = vec![head];
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let commit = josh_core::objects::CommitData::read(transaction.odb(), id)?;
        let parents: Vec<_> = commit.parent_ids().collect();
        pending.extend(parents.iter().rev().copied());
        commits.push(PreviewCommit {
            commit: id.to_string(),
            parents: parents.iter().map(ToString::to_string).collect(),
            description: std::str::from_utf8(commit.message_raw()?)?.to_owned(),
        });
    }
    Ok(commits)
}

async fn run_preview(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: PreviewArgs,
) -> Result<(), CommandError> {
    let workspace_command = recorded_workspace(ui, command_helper).await?;
    jj_lib::git::get_git_backend(workspace_command.repo().store())?.disable_lazy_commit_imports();
    let commit = workspace_command
        .resolve_single_rev(ui, &args.revision)
        .await?;

    check_projectable_repo_history(workspace_command.repo().as_ref(), &commit).await?;

    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let filter = FilterArgs {
        filter: args.filter,
        view: args.view,
    }
    .resolve()?;
    let source_oid = commit_as_josh_oid(&commit)?;

    // Filtering transforms history; the displayed change ID belongs to the source,
    // not a claim that the derived graph preserves every source commit.
    let transaction = open_josh_transaction(&git_repo_path, true)?;
    let projected_oid = josh_core::filter_commit(&transaction, filter, source_oid)
        .map_err(|err| user_error_with_message("Failed to apply the Josh projection", err))?;

    let preview = Preview {
        filter: josh_core::filter::spec(filter),
        source_commit: source_oid.to_string(),
        source_change_id: commit.change_id().to_string(),
        projected_commit: (!projected_oid.is_null()).then(|| projected_oid.to_string()),
        files: args
            .files
            .then(|| preview_files(&transaction, projected_oid))
            .transpose()
            .map_err(user_error)?,
        history: args
            .history
            .then(|| preview_history(&transaction, projected_oid))
            .transpose()
            .map_err(user_error)?,
    };
    if args.output.json {
        write_json(ui, &preview)?;
    } else {
        writeln!(ui.stdout(), "Projection: {}", preview.filter)?;
        writeln!(ui.stdout(), "Source commit: {}", preview.source_commit)?;
        writeln!(
            ui.stdout(),
            "Projected commit: {}",
            preview
                .projected_commit
                .as_deref()
                .unwrap_or("(empty view)")
        )?;
        writeln!(
            ui.stdout(),
            "Source change ID: {}",
            preview.source_change_id
        )?;
        writeln!(ui.stdout(), "Objects persisted: no")?;
        if let Some(files) = &preview.files {
            writeln!(ui.stdout(), "Files ({}):", files.len())?;
            for path in files {
                writeln!(ui.stdout(), "  {path:?}")?;
            }
        }
        if let Some(history) = &preview.history {
            writeln!(
                ui.stdout(),
                "Projected history ({} commits):",
                history.len()
            )?;
            for commit in history {
                writeln!(
                    ui.stdout(),
                    "  {} {:?} parents=[{}]",
                    commit.commit,
                    commit.description.lines().next().unwrap_or(""),
                    commit.parents.join(", ")
                )?;
            }
        }
    }
    Ok(())
}
