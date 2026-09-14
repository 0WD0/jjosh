use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;

use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_cli::ui::Ui;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::View;
use jj_lib::project::{
    BindingId, BindingRecord, BindingTarget, ConnectionId, ProjectState, Representation,
};
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::Repo as _;
use jj_lib::repo_path::{RepoPath, RepoPathBuf};

use crate::native_source::{NativeSource, SourceState};

#[derive(clap::Args, Clone, Debug)]
#[command(group(clap::ArgGroup::new("sources").args(["nested", "preserve"]).multiple(true).required(true)))]
pub(crate) struct ImportArgs {
    /// Import a local JJ workspace as a new nested project (repeatable).
    #[arg(long, value_name = "NAME=PATH")]
    nested: Vec<String>,
    /// Import a local JJ workspace while preserving its registered projects and
    /// project-bound connection settings (repeatable).
    #[arg(long, value_name = "NAME=PATH")]
    preserve: Vec<String>,
    /// Destination directory for a source (repeatable). Defaults to NAME/.
    #[arg(long, value_name = "NAME=DEST")]
    mount: Vec<String>,
}

#[derive(Clone, Copy)]
enum ImportMode {
    Nested,
    Preserve,
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
    remotes: Vec<crate::native_import::RemoteImport>,
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
    let mut specifications = Vec::with_capacity(args.nested.len() + args.preserve.len());
    for (mode, values) in [
        (ImportMode::Nested, args.nested),
        (ImportMode::Preserve, args.preserve),
    ] {
        for value in values {
            let (name, path) = value
                .split_once('=')
                .ok_or_else(|| user_error("Expected an import source in NAME=PATH form"))?;
            let name = crate::native_project::parse_project(name)
                .map_err(|err| user_error_with_message("Invalid import source name", err))?;
            if path.is_empty() || !names.insert(name.clone()) {
                return Err(user_error(
                    "Source paths must not be empty and source names must be unique across --nested and --preserve",
                ));
            }
            specifications.push((name, command.cwd().join(path), mode));
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
    let git_lock = workspace.lock_git_import_export()?;
    crate::interop::sha1_git_repo_path(&workspace)?;
    let git = jj_lib::git::get_git_repo(workspace.repo().store())?;
    jj_lib::git::ensure_no_pending_remote_management(&git).map_err(user_error)?;
    let destination_path = std::fs::canonicalize(workspace.repo_path())?;
    // Reservation contains foreign commit IDs in preserved mode. Only the
    // individual plans, mapped through rewritten IDs, may reach publication.
    let mut reserved = workspace.repo().view().store_view().clone();
    let mut sources = Vec::with_capacity(resolved.len());
    let mut remotes = HashSet::new();
    let mut remote_configs = Vec::new();
    for (name, path, mount, mode) in resolved {
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
            let plan = plan_outer(&source_view, &mut reserved, &name, &mount)?;
            reserve_remotes(
                &git,
                &reserved,
                plan.remotes.iter().map(|remote| &remote.physical),
                &mut remotes,
            )?;
            for remote in &plan.remotes {
                let source_name =
                    jj_lib::view::remote_identity::resolve(&source_view, &remote.source)
                        .ok()
                        .flatten()
                        .map_or_else(
                            || remote.source.as_str().to_owned(),
                            |identity| {
                                identity.qualified_name(&source_view.project_state, &remote.source)
                            },
                        );
                writeln!(
                    ui.status(),
                    "Import remote {} -> {}#{} (disconnected)",
                    source_name,
                    remote.alias.name.as_str(),
                    name
                )?;
            }
            (ProjectPlan::Outer(plan), None)
        };
        let extra_roots = provenance
            .as_ref()
            .map_or(&[][..], |capture| capture.canonical_roots.as_slice());
        let graph = NativeSource::read_view(loader.store().clone(), source_view, extra_roots)
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

    let destination_operation = workspace.repo().operation().id().hex();
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
                let mut fragment =
                    crate::native_import::map_outer_view(graph.view, &witness.name, &imported.ids);
                crate::native_import::install_remote_names(&mut fragment, plan.remotes);
                view.project_state.projects.extend(plan.metadata.projects);
                view.project_state.labels.extend(plan.metadata.labels);
                view.project_state.bindings.extend(plan.metadata.bindings);
                view.head_ids.extend(fragment.head_ids);
                view.local_bookmarks.extend(fragment.local_bookmarks);
                view.local_tags.extend(fragment.local_tags);
                view.remote_views.extend(fragment.remote_views);
                view.observed_remote_connections
                    .extend(fragment.observed_remote_connections);
                view.observed_remote_names
                    .extend(fragment.observed_remote_names);
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
    for source in &witnesses {
        source.verify().await?;
    }

    let local_bookmarks: HashSet<_> = view
        .local_bookmarks
        .keys()
        .filter(|name| {
            !tx.repo()
                .view()
                .store_view()
                .local_bookmarks
                .contains_key(*name)
        })
        .cloned()
        .collect();
    let local_tags: HashSet<_> = view
        .local_tags
        .keys()
        .filter(|name| !tx.repo().view().store_view().local_tags.contains_key(*name))
        .cloned()
        .collect();

    // Both modes publish every authority-bearing side effect through one journal.
    // Object and cache materialization above never publishes main-repository refs.
    tx.repo_mut().set_view(view);
    let journal =
        jj_lib::git::begin_remote_management(tx.repo().store(), &destination_operation, &[])?;
    let edits: Vec<_> = edits.into_values().collect();
    journal.record_ref_edits(&git, &edits)?;
    git.edit_references(edits).map_err(user_error)?;
    jj_lib::git::import_remote_configs(tx.repo().store(), &remote_configs)?;
    let stats = jj_lib::git::export_some_refs(tx.repo_mut(), |kind, symbol| {
        if symbol.remote == jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO {
            match kind {
                jj_lib::git::GitRefKind::Bookmark => local_bookmarks.contains(symbol.name),
                jj_lib::git::GitRefKind::Tag => local_tags.contains(symbol.name),
            }
        } else {
            remotes.contains(symbol.remote)
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
        return Err(user_error(
            "Cannot install imported Git mirrors; recover the interrupted import with \
             jjosh git remote recover --rollback before retrying",
        ));
    }
    journal.expect_operation(tx.repo().view())?;
    tx.into_inner().commit("import project states").await?;
    journal.complete()?;
    drop(git_lock);
    for (source, (projects, commits, signatures)) in witnesses.iter().zip(summaries) {
        if source.provenance.is_some() {
            writeln!(
                ui.status(),
                "Imported {}: {projects} preserved projects, {commits} commits, \
                {signatures} invalidated signatures removed.",
                source.name
            )?;
        } else {
            writeln!(
                ui.status(),
                "Imported {}: new nested project, {commits} commits, {signatures} invalidated signatures removed.",
                source.name
            )?;
        }
    }
    writeln!(
        ui.status(),
        "Projects imported in one transaction; working copy unchanged. \
        Select imported revisions by their bookmarks or change IDs with jjosh new."
    )?;
    if witnesses.iter().any(|source| source.provenance.is_some()) {
        writeln!(
            ui.status(),
            "Preserved sources retain project identities, connection settings, and conversion evidence. \
            No remote fetch or push was performed."
        )?;
    }
    Ok(())
}

fn plan_outer(
    source: &View,
    reserved: &mut View,
    name: &str,
    mount: &RepoPath,
) -> Result<OuterPlan, CommandError> {
    let project = crate::project_config::register(reserved, name, mount).map_err(user_error)?;
    let binding = BindingId::generate();
    let record = jj_lib::merge::Merge::resolved(Some(BindingRecord {
        target: BindingTarget::Project(project.clone()),
        connection_id: ConnectionId::generate(),
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
    let remotes = crate::native_import::plan_remotes(source, &project).map_err(user_error)?;
    Ok(OuterPlan {
        metadata,
        binding,
        remotes,
    })
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
