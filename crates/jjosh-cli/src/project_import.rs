use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Write as _;

use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;
use jj_cli::command_error::user_error_with_message;
use jj_cli::git_remote::GitRemoteFetchOptions;
use jj_cli::git_remote::GitRemoteSession as _;
use jj_cli::ui::Ui;
use jj_lib::git::GitFetchRefExpression;
use jj_lib::git::GitSettings;
use jj_lib::merge::Merge;
use jj_lib::op_store::View;
use jj_lib::project::BindingId;
use jj_lib::project::BindingRecord;
use jj_lib::project::BindingTarget;
use jj_lib::project::ConnectionId;
use jj_lib::project::ProjectState;
use jj_lib::project::Representation;
use jj_lib::project::ScopedRemoteName;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::Repo as _;
use jj_lib::repo_path::RepoPath;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::settings::JJRng;
use jj_lib::str_util::StringExpression;

use crate::native_source::NativeSource;
use crate::native_source::SourceState;

#[derive(clap::Args, Clone, Debug)]
#[command(group(clap::ArgGroup::new("sources").args(["nested", "preserve", "git"]).multiple(true).required(true)))]
pub(crate) struct ImportArgs {
    /// Import a local JJ workspace as a new nested project (repeatable).
    #[arg(long, value_name = "NAME=PATH")]
    nested: Vec<String>,
    /// Import a local JJ workspace while preserving its registered projects and
    /// project-bound connection settings (repeatable).
    #[arg(long, value_name = "NAME=PATH")]
    preserve: Vec<String>,
    /// Fetch all Git branches and tags with complete history as a new project,
    /// retaining a project-scoped origin connection (repeatable).
    #[arg(long, value_name = "NAME=URL")]
    git: Vec<String>,
    /// Destination directory for a source (repeatable). Defaults to NAME/.
    #[arg(long, value_name = "NAME=DEST")]
    mount: Vec<String>,
}

#[derive(Clone, Copy)]
enum ImportMode {
    Nested,
    Preserve,
    Git,
}

struct SourceWitness {
    name: String,
    workspace: jj_lib::workspace::Workspace,
    state: SourceState,
    provenance: Option<crate::project_provenance::Capture>,
}

impl SourceWitness {
    async fn verify(&self) -> Result<(), CommandError> {
        self.state
            .verify(self.workspace.repo_loader())
            .await
            .map_err(|err| {
                user_error_with_message(format!("Source {} changed during import", self.name), err)
            })?;
        if let Some(provenance) = &self.provenance {
            provenance.verify_source().map_err(user_error)?;
        }
        Ok(())
    }
}

struct OuterPlan {
    metadata: ProjectState,
    binding: BindingId,
}

impl OuterPlan {
    fn merge_into(self, view: &mut View) {
        view.project_state.projects.extend(self.metadata.projects);
        view.project_state.labels.extend(self.metadata.labels);
        view.project_state.bindings.extend(self.metadata.bindings);
        view.project_state
            .remote_names
            .extend(self.metadata.remote_names);
    }
}

struct GitSource {
    name: String,
    remote: RemoteNameBuf,
    url: String,
    connection: ConnectionId,
    plan: OuterPlan,
}

enum ProjectPlan {
    Outer(OuterPlan),
    Preserved(Box<crate::project_preserve::Plan>),
}

struct ImportSource {
    witness: SourceWitness,
    mount: RepoPathBuf,
    graph: NativeSource,
    plan: ProjectPlan,
}

pub(crate) async fn run_import(
    ui: &mut Ui,
    command: &CommandHelper,
    args: ImportArgs,
) -> Result<(), CommandError> {
    crate::project::require_current_operation(command)?;
    let mut names = HashSet::new();
    let mut specifications =
        Vec::with_capacity(args.nested.len() + args.preserve.len() + args.git.len());
    for (mode, values) in [
        (ImportMode::Nested, args.nested),
        (ImportMode::Preserve, args.preserve),
        (ImportMode::Git, args.git),
    ] {
        for value in values {
            let (name, path) = value.split_once('=').ok_or_else(|| {
                user_error("Expected an import source in NAME=PATH or NAME=URL form")
            })?;
            let name = crate::native_project::parse_project(name)
                .map_err(|err| user_error_with_message("Invalid import source name", err))?;
            if path.is_empty() || !names.insert(name.clone()) {
                return Err(user_error(
                    "Source paths and URLs must not be empty and source names must be unique across \
                     --nested, --preserve and --git",
                ));
            }
            specifications.push((name, path.to_owned(), mode));
        }
    }
    let mut mounts = HashMap::new();
    for value in args.mount {
        let (name, path) = value
            .split_once('=')
            .ok_or_else(|| user_error("Expected --mount NAME=DEST"))?;
        let name = crate::native_project::parse_project(name)
            .map_err(|err| user_error_with_message("Invalid native project name", err))?;
        if !names.contains(&name) {
            return Err(user_error(format!(
                "Mount {name:?} does not match an imported source"
            )));
        }
        let mount = crate::native_project::parse_mount(path).map_err(|err| {
            user_error_with_message(format!("Invalid mount for native project {name}"), err)
        })?;
        if mounts.insert(name.clone(), mount).is_some() {
            return Err(user_error(format!("Duplicate mount for project {name:?}")));
        }
    }
    let mut resolved = Vec::with_capacity(specifications.len());
    for (name, path, mode) in specifications {
        let mount = match mounts.remove(&name) {
            Some(mount) => mount,
            None => crate::native_project::default_mount(&name).map_err(user_error)?,
        };
        resolved.push((name, path, mount, mode));
    }
    crate::native_project::check_mounts_disjoint(
        resolved
            .iter()
            .map(|(name, _, mount, _)| (name.as_str(), mount.as_ref())),
    )
    .map_err(user_error)?;

    let mut workspace = crate::project::recorded_workspace(ui, command).await?;
    let git_path = crate::interop::sha1_git_repo_path(&workspace)?;
    let git = jj_lib::git::get_git_repo(workspace.repo().store())?;
    let destination_path = std::fs::canonicalize(workspace.repo_path())?;
    // Reservation contains foreign commit IDs in preserved mode. Only the
    // individual plans, mapped through rewritten IDs, may reach publication.
    let mut reserved = workspace.repo().view().store_view().clone();
    let mut sources = Vec::with_capacity(resolved.len());
    let mut git_sources = Vec::new();
    let mut git_remotes = HashSet::new();
    let mut remotes = HashSet::new();
    let mut remote_configs = Vec::new();
    for (name, path, mount, mode) in resolved {
        for head in workspace.repo().view().heads() {
            let commit = workspace.repo().store().get_commit_async(head).await?;
            if crate::native_project::commit_path_occupied(&commit, &mount)
                .await
                .map_err(user_error)?
            {
                return Err(user_error(format!(
                    "Destination path {} is already occupied in visible history",
                    mount.as_internal_file_string()
                )));
            }
        }
        if matches!(mode, ImportMode::Git) {
            let url = jj_cli::git_util::absolute_git_url(command.cwd(), &path)?;
            let mut plan = plan_outer(
                &mut reserved,
                &name,
                &mount,
                command.settings().get_rng().as_ref(),
            )?;
            let remote = RemoteNameBuf::from(format!("origin#{name}"));
            reserve_remotes(&git, &reserved, std::iter::once(&remote), &mut remotes)?;
            let binding = plan.metadata.bindings[&plan.binding]
                .as_resolved()
                .and_then(Option::as_ref)
                .expect("new outer binding is resolved");
            let BindingTarget::Project(project) = &binding.target else {
                unreachable!("new outer binding targets a project");
            };
            let connection = binding.connection_id.clone();
            let scoped_name = Merge::resolved(Some(ScopedRemoteName {
                project: project.clone(),
                name: RemoteNameBuf::from("origin"),
            }));
            plan.metadata
                .remote_names
                .insert(connection.clone(), scoped_name.clone());
            reserved
                .project_state
                .remote_names
                .insert(connection.clone(), scoped_name);
            reserved
                .remote_connections
                .insert(remote.clone(), Merge::resolved(Some(connection.clone())));
            remote_configs.push(jj_lib::git::ImportedRemoteConfig::for_new_connection(
                &git,
                &remote,
                &url,
                &connection,
                true,
            )?);
            git_remotes.insert(remote.clone());
            git_sources.push(GitSource {
                name,
                remote,
                url,
                connection,
                plan,
            });
            continue;
        }
        let path = command.cwd().join(path);
        if !path.is_dir() {
            return Err(user_error(format!(
                "Project source {} must be a local JJ workspace directory",
                path.display()
            )));
        }
        let source_workspace = command.load_workspace_at(&path, workspace.settings())?;
        if std::fs::canonicalize(source_workspace.repo_path())? == destination_path {
            return Err(user_error(
                "Cannot import a workspace from the destination's own repository",
            ));
        }
        let loader = source_workspace.repo_loader();
        let (state, source_view) = SourceState::capture(loader).await.map_err(|err| {
            user_error_with_message(format!("Cannot read project source {name}"), err)
        })?;
        let (plan, provenance) = if matches!(mode, ImportMode::Preserve) {
            let mut source_git = jj_lib::git::get_git_repo(loader.store())?;
            source_git.reload().map_err(user_error)?;
            let mut configured = BTreeMap::new();
            for section in source_git
                .config_snapshot()
                .sections_by_name("remote")
                .into_iter()
                .flatten()
            {
                if let Some(remote) = section.header().subsection_name() {
                    let remote =
                        RemoteNameBuf::from(std::str::from_utf8(remote).map_err(user_error)?);
                    let endpoint = section.contains_value_name("url")
                        || section.contains_value_name("pushurl");
                    *configured.entry(remote).or_insert(false) |= endpoint;
                }
            }
            let plan = crate::project_preserve::prepare(
                &source_view,
                &reserved,
                &name,
                &mount,
                &configured,
            )
            .map_err(|err| {
                user_error_with_message(format!("Cannot preserve projects from {name}"), err)
            })?;
            reserve_remotes(
                &git,
                &reserved,
                plan.remotes.values().map(|mapping| &mapping.destination),
                &mut remotes,
            )?;
            remote_configs.extend(jj_lib::git::capture_import_remote_configs(
                &source_git,
                &plan.remotes,
            )?);
            let provenance = crate::project_provenance::Capture::read(&source_git, &source_view)
                .map_err(|err| {
                    user_error_with_message(format!("Cannot read provenance for {name}"), err)
                })?;
            plan.reserve_into(&mut reserved);
            (ProjectPlan::Preserved(Box::new(plan)), Some(provenance))
        } else {
            let plan = plan_outer(
                &mut reserved,
                &name,
                &mount,
                command.settings().get_rng().as_ref(),
            )?;
            (ProjectPlan::Outer(plan), None)
        };
        let extra_roots = provenance
            .as_ref()
            .map_or(&[][..], |capture| capture.canonical_roots.as_slice());
        let graph =
            NativeSource::read_view(loader.store().clone(), source_view, extra_roots, |_| false)
                .await
                .map_err(user_error)?;
        let witness = SourceWitness {
            name,
            workspace: source_workspace,
            state,
            provenance,
        };
        witness.verify().await?;
        sources.push(ImportSource {
            witness,
            mount,
            graph,
            plan,
        });
    }
    jj_lib::git::check_import_remote_configs(workspace.repo().store(), &remote_configs)?;

    let mut tx = workspace.start_transaction();
    let mut view = tx.repo().view().store_view().clone();
    let mut edits = BTreeMap::new();
    let mut witnesses = Vec::with_capacity(sources.len());
    let mut summaries = Vec::with_capacity(sources.len());
    for ImportSource {
        witness,
        mount,
        graph,
        plan,
    } in sources
    {
        let imported = crate::native_import::rewrite_graph(
            &graph,
            tx.repo_mut(),
            &witness.name,
            &mount,
            HashMap::new(),
        )
        .await
        .map_err(user_error)?;
        let projects = match plan {
            ProjectPlan::Outer(plan) => {
                collect_edits(
                    &mut edits,
                    crate::native_project::prepare_offline_import(
                        &git,
                        &plan.binding,
                        &graph,
                        &imported.ids,
                    )
                    .map_err(user_error)?,
                )?;
                let fragment =
                    crate::native_import::map_outer_view(graph.view, &witness.name, &imported.ids);
                plan.merge_into(&mut view);
                view.head_ids.extend(fragment.head_ids);
                view.local_bookmarks.extend(fragment.local_bookmarks);
                view.local_tags.extend(fragment.local_tags);
                1
            }
            ProjectPlan::Preserved(plan) => {
                let count = plan
                    .view
                    .project_state
                    .projects
                    .values()
                    .filter(|value| value.as_resolved().is_some_and(Option::is_some))
                    .count();
                plan.merge_into(&mut view, &imported.ids);
                count
            }
        };
        if let Some(provenance) = &witness.provenance {
            collect_edits(
                &mut edits,
                provenance
                    .materialize(&git, &imported.ids)
                    .map_err(user_error)?
                    .refs,
            )?;
        }
        summaries.push((
            projects,
            imported.commits.len(),
            imported.stripped_signatures,
        ));
        witnesses.push(witness);
    }

    let mut endpoints = Vec::with_capacity(git_sources.len());
    for GitSource {
        name,
        remote,
        url,
        connection,
        plan,
    } in git_sources
    {
        plan.merge_into(&mut view);
        view.remote_connections
            .insert(remote.clone(), Merge::resolved(Some(connection.clone())));
        endpoints.push((name, remote, url, connection));
    }
    // Install the planned local view before fetch preparation: conversion writes
    // commits and other operation-owned state into this same mutable repository.
    // Never restore this snapshot after a fetch has started.
    tx.repo_mut().set_view(view);
    let import_options = if endpoints.is_empty() {
        None
    } else {
        let git_settings = GitSettings::from_settings(tx.settings())?;
        let remote_settings = jj_cli::revset_util::resolve_remote_settings(
            tx.repo().view(),
            tx.settings().remote_settings()?,
        )?;
        Some(jj_cli::git_util::load_git_import_options(
            ui,
            &git_settings,
            &remote_settings,
        )?)
    };
    let mut prepared_fetches = Vec::with_capacity(endpoints.len());
    let mut git_summaries = Vec::with_capacity(endpoints.len());
    let fetch_options = GitRemoteFetchOptions::default();
    for (name, remote, url, connection) in endpoints {
        let session = crate::git_remote::Session::for_import(
            &git,
            git_path.clone(),
            tx.repo().view(),
            &remote,
            connection,
            url,
        )?;
        prepared_fetches.push(
            session
                .prepare_fetch(
                    ui,
                    command,
                    tx.repo_mut(),
                    GitFetchRefExpression {
                        bookmark: StringExpression::all(),
                        tag: StringExpression::all(),
                    },
                    &fetch_options,
                )
                .await?,
        );
        // An empty repository still owns an addressable project origin.
        tx.repo_mut().ensure_remote(&remote);
        git_summaries.push(name);
    }

    // Recheck source captures and the exact destination resources before publishing.
    for source in &witnesses {
        source.verify().await?;
    }
    // As with ordinary fetch, only shared mirror publication holds this lock;
    // all network receive and conversion has already completed.
    let _git_lock = tx.base_workspace_helper().lock_git_import_export()?;
    let mut git = jj_lib::git::get_git_repo(tx.repo().store())?;
    git.reload().map_err(user_error)?;
    let mut checked_remotes = HashSet::new();
    reserve_remotes(
        &git,
        tx.base_repo().view().store_view(),
        remotes.iter(),
        &mut checked_remotes,
    )?;
    jj_lib::git::check_import_remote_configs(tx.repo().store(), &remote_configs)?;

    let edits: Vec<_> = edits.into_values().collect();
    let publication = async {
        git.edit_references(edits).map_err(user_error)?;
        jj_lib::git::import_remote_configs(tx.repo().store(), &remote_configs)?;
        if let Some(import_options) = &import_options {
            let mut observations = Vec::new();
            for prepared in prepared_fetches {
                observations.extend(prepared.publish(tx.repo_mut()).await?);
            }
            let stats = jj_lib::git::import_remote_observations(
                tx.repo_mut(),
                import_options,
                observations,
                |_, symbol| git_remotes.contains(symbol.remote),
            )
            .await?;
            jj_cli::git_util::print_git_import_stats(ui, &tx, &stats)?;
        }
        let view = tx.repo().view().store_view();
        let base = tx.base_repo().view().store_view();
        let local_bookmarks: HashSet<_> = view
            .local_bookmarks
            .keys()
            .filter(|name| !base.local_bookmarks.contains_key(*name))
            .cloned()
            .collect();
        let local_tags: HashSet<_> = view
            .local_tags
            .keys()
            .filter(|name| !base.local_tags.contains_key(*name))
            .cloned()
            .collect();
        let stats = jj_lib::git::export_some_refs(tx.repo_mut(), |kind, symbol| {
            if symbol.remote == jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO {
                match kind {
                    jj_lib::git::GitRefKind::Bookmark => local_bookmarks.contains(symbol.name),
                    jj_lib::git::GitRefKind::Tag => local_tags.contains(symbol.name),
                }
            } else {
                remotes.contains(symbol.remote) && !git_remotes.contains(symbol.remote)
            }
        })?;
        jj_cli::git_util::print_git_export_stats(ui, tx.repo().view(), &stats)?;
        if stats
            .failed_bookmarks
            .iter()
            .chain(&stats.failed_tags)
            .any(|(_, reason)| {
                !matches!(
                    reason,
                    jj_lib::git::FailedRefExportReason::InvalidGitName
                        | jj_lib::git::FailedRefExportReason::OnRootCommit
                )
            })
        {
            return Err(user_error("Cannot install imported Git mirrors"));
        }
        command
            .maybe_commit_transaction(tx.into_inner(), "import project states")
            .await?;
        Ok::<_, CommandError>(())
    }
    .await;
    publication.map_err(|error| {
        user_error_with_message(
            "Project import stopped after destination installation began. Source repositories are \
             unchanged; copied objects, provenance and any completed destination config/ref \
             changes were retained. Resolve the reported resource conflict before importing again.",
            error.error,
        )
    })?;
    for (source, (projects, commits, signatures)) in witnesses.iter().zip(summaries) {
        if source.provenance.is_some() {
            writeln!(
                ui.status(),
                "Imported {}: {projects} preserved projects, {commits} commits, {signatures} \
                 invalidated signatures removed.",
                source.name
            )?;
        } else {
            writeln!(
                ui.status(),
                "Imported {}: new nested project, {commits} commits, {signatures} invalidated \
                 signatures removed.",
                source.name
            )?;
        }
    }
    for name in git_summaries {
        writeln!(
            ui.status(),
            "Imported {name}: new Git project with complete branch and tag history; \
             origin#{name} is available for fetch and push."
        )?;
    }
    writeln!(
        ui.status(),
        "Projects imported in one transaction; working copy unchanged. Select imported revisions \
         by their bookmarks or change IDs with jjosh new."
    )?;
    if witnesses.iter().any(|source| source.provenance.is_some()) {
        writeln!(
            ui.status(),
            "Preserved sources retain project identities, connection settings, and conversion \
             evidence. Their source connections were imported without contacting their remotes."
        )?;
    }
    Ok(())
}

fn plan_outer(
    reserved: &mut View,
    name: &str,
    mount: &RepoPath,
    rng: &JJRng,
) -> Result<OuterPlan, CommandError> {
    let project =
        crate::project_config::register(reserved, name, mount, rng).map_err(user_error)?;
    let binding = BindingId::new(rng.new_random_id(16));
    let record = jj_lib::merge::Merge::resolved(Some(BindingRecord {
        target: BindingTarget::Project(project.clone()),
        connection_id: ConnectionId::new(rng.new_random_id(16)),
        representation: Representation::Whole,
        base: None,
    }));
    reserved
        .project_state
        .bindings
        .insert(binding.clone(), record.clone());
    let mut metadata = ProjectState::default();
    metadata.projects.insert(
        project.clone(),
        reserved.project_state.projects[&project].clone(),
    );
    metadata
        .labels
        .insert(name.to_owned(), reserved.project_state.labels[name].clone());
    metadata.bindings.insert(binding.clone(), record);
    Ok(OuterPlan { metadata, binding })
}

fn reserve_remotes<'a>(
    git: &gix::Repository,
    destination: &View,
    names: impl Iterator<Item = &'a RemoteNameBuf>,
    reserved: &mut HashSet<RemoteNameBuf>,
) -> Result<(), CommandError> {
    for name in names {
        if !reserved.insert(name.clone())
            || destination.remote_views.contains_key(name)
            || destination.remote_connections.contains_key(name)
            || destination.observed_remote_connections.contains_key(name)
            || git
                .config_snapshot()
                .sections_by_name("remote")
                .into_iter()
                .flatten()
                .any(|section| {
                    section
                        .header()
                        .subsection_name()
                        .is_some_and(|value| value == name.as_str().as_bytes())
                })
        {
            return Err(user_error(format!(
                "Imported remote {} collides with an existing connection",
                name.as_str()
            )));
        }
        for prefix in [
            format!("refs/remotes/{}/", name.as_str()),
            format!(
                "{}{}/",
                jj_lib::git::REMOTE_TAG_REF_NAMESPACE,
                name.as_str()
            ),
        ] {
            if git
                .references()
                .map_err(user_error)?
                .prefixed(prefix.as_str())
                .map_err(user_error)?
                .next()
                .transpose()
                .map_err(|err| user_error(err.to_string()))?
                .is_some()
            {
                return Err(user_error(format!(
                    "Imported remote {} collides with existing Git refs",
                    name.as_str()
                )));
            }
        }
    }
    Ok(())
}

fn collect_edits(
    edits: &mut BTreeMap<gix::refs::FullName, gix::refs::transaction::RefEdit>,
    incoming: Vec<gix::refs::transaction::RefEdit>,
) -> Result<(), CommandError> {
    for edit in incoming {
        match edits.entry(edit.name.clone()) {
            std::collections::btree_map::Entry::Occupied(previous) if previous.get() != &edit => {
                return Err(user_error(format!(
                    "Imported sources have conflicting provenance for {}",
                    edit.name.as_bstr()
                )));
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(edit);
            }
        }
    }
    Ok(())
}
