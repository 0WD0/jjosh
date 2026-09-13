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
    /// Register an existing monorepo directory independently of any remote.
    Add(ProjectAddArgs),
    /// List projects, mounts, projection remotes, and configuration problems.
    List(OutputArgs),
    /// Show one project's layout, remotes, and publication readiness.
    Show(ShowArgs),
    /// Diagnose project configuration without contacting remotes.
    Check(CheckArgs),
    /// Preview a projected commit without changing refs or the working copy.
    Preview(PreviewArgs),
    /// Configure projection remotes.
    Remote(RemoteArgs),
}

#[derive(clap::Args, Clone, Debug)]
struct ProjectAddArgs {
    /// Project identity used in bookmark and tag names such as main#PROJECT.
    project: String,
    /// Existing directory, relative to the repository root in recorded @.
    #[arg(long)]
    mount: String,
}

#[derive(clap::Args, Clone, Debug)]
struct OutputArgs {
    /// Emit structured JSON instead of human-readable output.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args, Clone, Debug)]
struct ShowArgs {
    project: String,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(clap::Args, Clone, Debug)]
struct CheckArgs {
    /// Limit diagnostics to this project and its remotes.
    project: Option<String>,
    #[command(flatten)]
    output: OutputArgs,
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
    clap::ArgGroup::new("selection").required(true).args(["filter", "view", "project"])
))]
struct PreviewArgs {
    /// Josh filter expression applied to the selected recorded revision.
    filter: Option<String>,
    /// Versioned workspace.josh view, relative to the repository root.
    #[arg(long)]
    view: Option<String>,
    /// Preview this project's directory view using its recorded mount.
    #[arg(long)]
    project: Option<String>,

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

#[derive(clap::Args, Clone, Debug)]
struct RemoteArgs {
    #[command(subcommand)]
    command: RemoteCommand,
}

#[derive(clap::Subcommand, Clone, Debug)]
enum RemoteCommand {
    /// Add or replace a projection remote.
    Add(RemoteAddArgs),
    /// Attach a named Git remote to a project without changing recorded history.
    Attach(RemoteAttachArgs),
}

#[derive(clap::Args, Clone, Debug)]
struct RemoteAddArgs {
    /// Remote name exposed to Jujutsu, such as `origin`.
    name: String,
    /// Upstream Git URL containing the unprojected history.
    url: String,
    #[command(flatten)]
    projection: FilterArgs,
    /// Optional publication endpoint; otherwise use the source endpoint.
    #[arg(long)]
    push_url: Option<String>,
    /// Mount the projected history as this project.
    #[arg(long)]
    project: Option<String>,
    /// Project mount; defaults to its recorded native mount or project name.
    #[arg(long, requires = "project")]
    mount: Option<String>,
    /// Source branch, full ref, or commit OID used as the authoritative base.
    #[arg(long)]
    base: Option<String>,
    /// Fetch only; do not publish through this remote.
    #[arg(long, conflicts_with = "writable")]
    read_only: bool,
    /// Allow publication through this remote.
    #[arg(long)]
    writable: bool,
}

#[derive(clap::Args, Clone, Debug)]
struct RemoteAttachArgs {
    name: String,
    project: String,
    #[arg(long)]
    mount: Option<String>,
    /// Source branch, full ref, or commit OID used as the authoritative base.
    #[arg(long)]
    base: Option<String>,
    /// Fetch only; do not publish through this remote.
    #[arg(long, conflicts_with = "writable")]
    read_only: bool,
    /// Allow publication through this remote.
    #[arg(long)]
    writable: bool,
}

pub(crate) async fn run(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: Args,
) -> Result<(), CommandError> {
    match args.command {
        Command::Add(args) => run_project_add(ui, command_helper, args).await,
        Command::List(args) => run_inspect(ui, command_helper, None, args, false).await,
        Command::Show(args) => {
            run_inspect(ui, command_helper, Some(args.project), args.output, false).await
        }
        Command::Check(args) => {
            run_inspect(ui, command_helper, args.project, args.output, true).await
        }
        Command::Preview(args) => run_preview(ui, command_helper, args).await,
        Command::Remote(args) => run_remote(ui, command_helper, args).await,
    }
}

fn require_current_operation(command_helper: &CommandHelper) -> Result<(), CommandError> {
    if !command_helper.is_at_head_operation() || command_helper.global_args().no_integrate_operation
    {
        return Err(user_error(
            "Projection configuration requires the current integrated operation",
        ));
    }
    Ok(())
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

async fn run_project_add(
    ui: &mut Ui,
    command: &CommandHelper,
    args: ProjectAddArgs,
) -> Result<(), CommandError> {
    require_current_operation(command)?;
    let workspace = recorded_workspace(ui, command).await?;
    let git_path = sha1_git_repo_path(&workspace)?;
    let project = crate::native_project::parse_project(&args.project).map_err(user_error)?;
    let mount = crate::native_project::parse_mount(&args.mount).map_err(user_error)?;
    let commit = workspace.resolve_single_rev(ui, &RevisionArg::AT).await?;
    if !crate::native_project::commit_path_occupied(&commit, &mount)
        .await
        .map_err(user_error)?
    {
        return Err(user_error(format!(
            "No directory at {} in recorded @; record working files before registering the project",
            mount.as_internal_file_string(),
        )));
    }
    // This also rejects a file or symlink where a directory is expected.
    crate::native_project::project_tree(&commit, &mount)
        .await
        .map_err(user_error)?;
    let _git_lock = workspace.lock_git_import_export()?;
    crate::project_config::register(&git_path, &project, &mount).map_err(user_error)?;
    writeln!(
        ui.status(),
        "Registered {project} at {}",
        mount.as_internal_file_string()
    )?;
    Ok(())
}

fn write_json(ui: &mut Ui, value: &impl serde::Serialize) -> Result<(), CommandError> {
    serde_json::to_writer_pretty(ui.stdout(), value).map_err(user_error)?;
    writeln!(ui.stdout())?;
    Ok(())
}

async fn run_inspect(
    ui: &mut Ui,
    command: &CommandHelper,
    project: Option<String>,
    args: OutputArgs,
    check: bool,
) -> Result<(), CommandError> {
    use crate::project_config::Severity;
    let workspace = recorded_workspace(ui, command).await?;
    let git_path = sha1_git_repo_path(&workspace)?;
    let mut inventory = crate::project_config::inspect(&git_path).map_err(user_error)?;
    if let Some(project) = project {
        let project = crate::native_project::parse_project(&project).map_err(user_error)?;
        let selected = inventory
            .projects
            .iter()
            .find(|entry| entry.name == project)
            .ok_or_else(|| user_error(format!("Unknown project {project}")))?;
        inventory
            .issues
            .retain(|issue| issue.subject == project || selected.remotes.contains(&issue.subject));
        inventory
            .remotes
            .retain(|remote| selected.remotes.contains(&remote.name));
        inventory.projects.retain(|entry| entry.name == project);
    }
    let failed = inventory
        .issues
        .iter()
        .any(|issue| issue.severity == Severity::Error);
    if args.json {
        write_json(ui, &inventory)?;
    } else {
        writeln!(ui.stdout(), "PROJECT\tMOUNT\tREGISTRATION\tREMOTES")?;
        for project in &inventory.projects {
            writeln!(
                ui.stdout(),
                "{}\t{}\t{}\t{}",
                project.name,
                project.mount.as_deref().unwrap_or("(invalid)"),
                if project.native {
                    "native"
                } else if project.registered {
                    "registered"
                } else {
                    "remote-derived"
                },
                if project.remotes.is_empty() {
                    "(none)".to_owned()
                } else {
                    project.remotes.join(", ")
                },
            )?;
        }
        for remote in &inventory.remotes {
            writeln!(ui.stdout(), "\nRemote: {}", remote.name)?;
            writeln!(
                ui.stdout(),
                "  Project: {}",
                remote.project.as_deref().unwrap_or("(standalone)")
            )?;
            writeln!(
                ui.stdout(),
                "  Fetch: {}",
                remote.fetch_url.as_deref().unwrap_or("(missing)")
            )?;
            writeln!(
                ui.stdout(),
                "  Push: {}",
                remote.push_url.as_deref().unwrap_or("(missing)")
            )?;
            writeln!(
                ui.stdout(),
                "  Writable: {}",
                match remote.read_only {
                    Some(false) => "yes",
                    Some(true) => "no",
                    None => "invalid policy",
                }
            )?;
            if let Some(filter) = &remote.filter {
                writeln!(ui.stdout(), "  Source filter: {filter}")?;
            }
        }
        for issue in &inventory.issues {
            writeln!(
                ui.stdout(),
                "{} [{}]: {}",
                match issue.severity {
                    Severity::Error => "Error",
                    Severity::Warning => "Warning",
                },
                issue.subject,
                issue.message
            )?;
        }
        if check && inventory.issues.is_empty() {
            writeln!(ui.stdout(), "Configuration is consistent.")?;
        }
    }
    if check && failed {
        return Err(user_error("Projection configuration has errors"));
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct Preview {
    project: Option<String>,
    mount: Option<String>,
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
    let project = args
        .project
        .as_deref()
        .map(crate::native_project::parse_project)
        .transpose()
        .map_err(user_error)?;
    let mount = project
        .as_deref()
        .map(|project| {
            crate::git_remote::recorded_project_mount(&git_repo_path, project)?
                .ok_or_else(|| user_error(format!("Unknown project {project}")))
        })
        .transpose()?;
    let filter = if let Some(mount) = &mount {
        josh_core::filter::Filter::new()
            .subdir(mount.as_internal_file_string())
            .exclude(josh_core::filter::Filter::new().file(".link.josh"))
    } else {
        FilterArgs {
            filter: args.filter,
            view: args.view,
        }
        .resolve()?
    };
    let source_oid = commit_as_josh_oid(&commit)?;

    // Filtering transforms history; the displayed change ID belongs to the source,
    // not a claim that the derived graph preserves every source commit.
    let transaction = open_josh_transaction(&git_repo_path, true)?;
    let projected_oid = josh_core::filter_commit(&transaction, filter, source_oid)
        .map_err(|err| user_error_with_message("Failed to apply the Josh projection", err))?;

    let preview = Preview {
        project,
        mount: mount.map(|mount| mount.as_internal_file_string().to_owned()),
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
        if let (Some(project), Some(mount)) = (&preview.project, &preview.mount) {
            writeln!(ui.stdout(), "Project directory view: {project} at {mount}")?;
        }
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
        if preview.project.is_some() {
            writeln!(
                ui.stdout(),
                "This is the local directory view; git push --dry-run resolves the reverse-publication plan."
            )?;
        }
    }
    Ok(())
}

async fn run_remote(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: RemoteArgs,
) -> Result<(), CommandError> {
    match args.command {
        RemoteCommand::Add(args) => run_remote_add(ui, command_helper, args).await,
        RemoteCommand::Attach(args) => run_remote_attach(ui, command_helper, args).await,
    }
}

async fn run_remote_add(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: RemoteAddArgs,
) -> Result<(), CommandError> {
    let base = args
        .base
        .as_deref()
        .map(crate::project_config::parse_base)
        .transpose()
        .map_err(user_error)?;
    require_current_operation(command_helper)?;
    let workspace = recorded_workspace(ui, command_helper).await?;
    let git_path = sha1_git_repo_path(&workspace)?;
    let _git_lock = workspace.lock_git_import_export()?;
    let git = gix::open(&git_path).map_err(user_error)?;
    // Even an explicit override must not hide a malformed existing policy.
    let existing_read_only =
        crate::git_remote::remote_read_only(&git, jj_lib::ref_name::RemoteName::new(&args.name))
            .map_err(user_error)?;
    let existing_remote = git
        .remote_names()
        .iter()
        .any(|name| std::str::from_utf8(name).is_ok_and(|name| name == args.name));
    let project = args.project.or(crate::git_remote::config_string(
        &git,
        &format!("remote.{}.jjosh-project", args.name),
    )
    .map_err(user_error)?);
    let mut filter = args.projection.resolve()?;
    let attachment = if let Some(project) = project {
        let project = crate::native_project::parse_project(&project).map_err(user_error)?;
        let mount = args.mount.or(crate::git_remote::config_string(
            &git,
            &format!("remote.{}.jjosh-mount", args.name),
        )
        .map_err(user_error)?);
        let mount = attachment_mount(&git_path, &project, mount.as_deref())?;
        filter = filter.prefix(mount.as_internal_file_string());
        crate::git_remote::validate_attachment(
            &git_path,
            &args.name,
            &project,
            &mount,
            Some(filter),
        )
        .map_err(user_error)?;
        crate::project_config::validate_registration(&git_path, &project, &mount)
            .map_err(user_error)?;
        Some((project, mount))
    } else {
        None
    };
    let read_only = if args.read_only {
        true
    } else if args.writable || args.push_url.is_some() {
        false
    } else if existing_remote {
        existing_read_only
    } else {
        attachment.is_some()
    };
    let settings = if let Some((project, mount)) = &attachment {
        crate::git_remote::attachment_settings(project, mount, read_only, base.as_deref())
    } else {
        let mut settings = vec![("jjosh-readOnly", if read_only { "true" } else { "false" })];
        if let Some(base) = base.as_deref() {
            settings.push(("jjosh-base", base));
        }
        settings
    };
    josh_cli::remote_ops::configure_remote(
        &git_path,
        &args.name,
        &args.url,
        &josh_core::filter::spec(filter),
        None,
        args.push_url.as_deref(),
        None,
        &settings,
    )
    .map_err(|err| user_error_with_message("Failed to configure Josh projection remote", err))?;
    if let Some((project, mount)) = &attachment {
        crate::project_config::write_registration(&git_path, project, mount).map_err(user_error)?;
    }
    writeln!(
        ui.status(),
        "Configured projection remote {}: {} through {}",
        args.name,
        args.url,
        josh_core::filter::spec(filter),
    )?;
    Ok(())
}

fn attachment_mount(
    git_path: &Path,
    project: &str,
    mount: Option<&str>,
) -> Result<jj_lib::repo_path::RepoPathBuf, CommandError> {
    if let Some(mount) = mount {
        crate::native_project::parse_mount(mount).map_err(user_error)
    } else {
        crate::git_remote::project_mount(git_path, project)
    }
}

async fn run_remote_attach(
    ui: &mut Ui,
    command: &CommandHelper,
    args: RemoteAttachArgs,
) -> Result<(), CommandError> {
    let base = args
        .base
        .as_deref()
        .map(crate::project_config::parse_base)
        .transpose()
        .map_err(user_error)?;
    require_current_operation(command)?;
    let workspace = recorded_workspace(ui, command).await?;
    let git_path = sha1_git_repo_path(&workspace)?;
    let _git_lock = workspace.lock_git_import_export()?;
    let project = crate::native_project::parse_project(&args.project).map_err(user_error)?;
    let mount = attachment_mount(&git_path, &project, args.mount.as_deref())?;
    let git = gix::open(&git_path).map_err(user_error)?;
    git.find_remote(args.name.as_str()).map_err(user_error)?;
    let existing =
        crate::git_remote::config_string(&git, &format!("remote.{}.jjosh-project", args.name))
            .map_err(user_error)?;
    let read_only =
        crate::git_remote::remote_read_only(&git, jj_lib::ref_name::RemoteName::new(&args.name))
            .map_err(user_error)?;
    let read_only = if args.read_only {
        true
    } else if args.writable {
        false
    } else {
        read_only
    };
    let config = josh_changes::remote_config::try_read_remote_config(&git_path, &args.name)
        .map_err(user_error)?;
    let filter = config.as_ref().map(|config| {
        if existing.is_none() {
            config
                .semantic_filter()
                .prefix(mount.as_internal_file_string())
        } else {
            config.semantic_filter()
        }
    });
    crate::git_remote::validate_attachment(&git_path, &args.name, &project, &mount, filter)
        .map_err(user_error)?;
    crate::project_config::validate_registration(&git_path, &project, &mount)
        .map_err(user_error)?;
    if let Some(config) = config {
        josh_cli::remote_ops::configure_remote(
            &git_path,
            &args.name,
            &config.url,
            &josh_core::filter::spec(filter.unwrap()),
            config.forge,
            config.push_url.as_deref(),
            Some(config.gerrit_mode),
            &crate::git_remote::attachment_settings(
                &project,
                &mount,
                read_only,
                base.as_deref(),
            ),
        )
        .map_err(user_error)?;
    } else {
        crate::git_remote::configure_attachment(
            &git_path,
            &args.name,
            &project,
            &mount,
            read_only,
            base.as_deref(),
        )
        .map_err(user_error)?;
    }
    crate::project_config::write_registration(&git_path, &project, &mount).map_err(user_error)?;
    writeln!(
        ui.status(),
        "Attached {} to {project} at {}",
        args.name,
        mount.as_internal_file_string()
    )?;
    Ok(())
}
