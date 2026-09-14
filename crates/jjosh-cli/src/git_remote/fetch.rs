use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use gix::bstr::ByteSlice as _;
use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::{CommandError, user_error};
use jj_cli::git_remote::GitRemoteFetchOptions;
use jj_cli::git_remote::GitRemoteSession as _;
use jj_cli::ui::Ui;
use jj_lib::backend::CommitId;
use jj_lib::git::{GitFetchRefExpression, GitRefKind, GitRemoteObservation};
use jj_lib::merge::Merge;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::{RefTarget, View};
use jj_lib::project::{ConversionTerm, ObservationKey, ObservationKind};
use jj_lib::ref_name::{RefName, RemoteRefSymbolBuf};
use jj_lib::repo::{MutableRepo, Repo as _};

use super::Session;

fn source_ref(reference: &str) -> Option<(GitRefKind, &str)> {
    reference
        .strip_prefix("refs/heads/")
        .map(|name| (GitRefKind::Bookmark, name))
        .or_else(|| {
            reference
                .strip_prefix("refs/tags/")
                .map(|name| (GitRefKind::Tag, name))
        })
}

fn canonical_ref(session: &Session, kind: GitRefKind, source: &str) -> String {
    let prefix = match kind {
        GitRefKind::Bookmark => "refs/remotes/",
        GitRefKind::Tag => jj_lib::git::REMOTE_TAG_REF_NAMESPACE,
    };
    format!(
        "{prefix}{}/{}",
        session.name.as_str(),
        session.local_name(source).as_str()
    )
}

/// Logical tags target commits; physical mirrors retain unsigned annotations.
struct Converted {
    target: RefTarget,
    mirror: Option<gix::ObjectId>,
}

impl Converted {
    fn absent() -> Self {
        Self {
            target: RefTarget::absent(),
            mirror: None,
        }
    }

    fn native(target: RefTarget) -> Result<Self, CommandError> {
        let mirror = target
            .as_normal()
            .map(|id| gix::ObjectId::try_from(id.as_bytes()))
            .transpose()
            .map_err(user_error)?;
        Ok(Self { target, mirror })
    }
}

fn refs_prefixed(
    git: &gix::Repository,
    prefix: &str,
) -> Result<BTreeMap<String, gix::ObjectId>, CommandError> {
    let platform = git.references().map_err(user_error)?;
    let mut refs = BTreeMap::new();
    for reference in platform.prefixed(prefix).map_err(user_error)? {
        let reference = reference.map_err(user_error)?;
        let Ok(name) = std::str::from_utf8(reference.name().as_bstr()) else {
            continue;
        };
        if let gix::refs::TargetRef::Object(id) = reference.target() {
            refs.insert(name.to_owned(), id.to_owned());
        }
    }
    Ok(refs)
}

/// Apply deletions before creations so `x` -> `x/y` works with loose refs.
/// Conversion is complete before this function is used for canonical mirrors.
fn install_refs(
    git: &gix::Repository,
    old: &BTreeMap<String, gix::ObjectId>,
    new: &BTreeMap<String, gix::ObjectId>,
) -> Result<(), CommandError> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    let deletions = old
        .iter()
        .filter(|(name, _)| !new.contains_key(*name))
        .map(|(name, id)| {
            Ok(RefEdit {
                name: name.as_str().try_into().map_err(user_error)?,
                deref: false,
                change: Change::Delete {
                    expected: PreviousValue::MustExistAndMatch((*id).into()),
                    log: RefLog::AndReference,
                },
            })
        })
        .collect::<Result<Vec<_>, CommandError>>()?;
    if !deletions.is_empty() {
        git.edit_references(deletions).map_err(user_error)?;
    }
    let updates = new
        .iter()
        .map(|(name, id)| {
            Ok(RefEdit {
                name: name.as_str().try_into().map_err(user_error)?,
                deref: false,
                change: Change::Update {
                    expected: old.get(name).map_or(PreviousValue::MustNotExist, |old| {
                        PreviousValue::MustExistAndMatch((*old).into())
                    }),
                    new: (*id).into(),
                    log: LogChange {
                        message: "fetch remote reference".into(),
                        ..LogChange::default()
                    },
                },
            })
        })
        .collect::<Result<Vec<_>, CommandError>>()?;
    if !updates.is_empty() {
        git.edit_references(updates).map_err(user_error)?;
    }
    Ok(())
}

pub(super) async fn run(
    session: &Session,
    ui: &mut Ui,
    command: &CommandHelper,
    repo: &mut MutableRepo,
    selection: GitFetchRefExpression,
    options: &GitRemoteFetchOptions,
) -> Result<Vec<GitRemoteObservation>, CommandError> {
    let mut git = jj_lib::git::get_git_backend(repo.store())?.git_repo();
    // Earlier fetches in this operation can publish new packs and observation
    // refs. Negotiation disables ODB refreshes, so it needs a current snapshot.
    git.reload().map_err(user_error)?;
    let configured_endpoint = session
        .endpoint_url(&git, gix::remote::Direction::Fetch)
        .map_err(user_error)?;
    let endpoint = if let Some(url) = &options.fetch_url {
        let url = jj_cli::git_util::absolute_git_url(command.cwd(), url)?;
        super::remote_endpoint(
            &git.remote_at(url.as_str()).map_err(user_error)?,
            gix::remote::Direction::Fetch,
            session.whole(),
        )
        .map_err(user_error)?
    } else {
        configured_endpoint.clone()
    };
    let revisions: Vec<_> = options
        .revisions
        .iter()
        .map(|value| {
            let id = gix::ObjectId::from_hex(value.as_bytes()).map_err(user_error)?;
            if id.kind() != git.object_hash() || id.is_null() {
                return Err(user_error(
                    "Revision must be a non-null object ID using this repository's hash format",
                ));
            }
            Ok(id)
        })
        .collect::<Result<_, CommandError>>()?;
    let changes_depth = options.depth.is_some() || options.deepen.is_some() || options.unshallow;
    let reuse_history = !changes_depth;
    if changes_depth && session.filter().is_none() {
        return Err(user_error(
            "Shallow source history requires a Git projection remote, not an ordinary or native JJ source",
        ));
    }
    // Ordinary SHA-1 observations can authorize a later explicit --source push.
    // Other hash formats remain raw and never enter the SHA-1 conversion store.
    let transaction = if git.object_hash() == gix::hash::Kind::Sha1 {
        Some(crate::interop::open_josh_transaction(
            &session.git_path,
            false,
        )?)
    } else {
        None
    };
    let bookmark_matcher = selection.bookmark.to_matcher();
    let tag_matcher = selection.tag.to_matcher();
    let selected = |kind, name: &str| match kind {
        GitRefKind::Bookmark => bookmark_matcher.is_match(name),
        GitRefKind::Tag => tag_matcher.is_match(name),
    };
    if session.binding.is_none() {
        for (key, values) in &repo.view().store_view().project_observations {
            if key.remote != session.name {
                continue;
            }
            for evidence in values.iter().flatten() {
                let wire_selected =
                    source_ref(&evidence.raw_ref).is_some_and(|(kind, name)| selected(kind, name));
                // Also fence literal wire names that would occupy a protected
                // logical mirror. Unrelated raw selections never touch these names.
                let local_selected = match key.kind {
                    ObservationKind::Bookmark => selected(GitRefKind::Bookmark, key.name.as_str()),
                    ObservationKind::Tag => selected(GitRefKind::Tag, key.name.as_str()),
                    ObservationKind::Revision => revisions.iter().any(|id| id == key.name.as_str()),
                };
                if wire_selected || local_selected {
                    return Err(user_error(format!(
                        "Remote {} retains converted publication evidence for {}; raw fetch would replace that observation. Explicitly configure the matching binding or forget the converted observations before fetching this selection",
                        session.name.as_str(),
                        key.name.as_str(),
                    )));
                }
            }
        }
    }
    // Keyed by full source names, never logical/project-scoped names. Start with
    // absence so selected refs missing from the advertisement also get observed.
    let mut converted = BTreeMap::<String, Converted>::new();
    // A previous absent observation has neither a physical mirror nor a RemoteRef,
    // but a selected fetch must still refresh its exact endpoint evidence.
    if session.binding.is_some() {
        for (key, values) in &repo.view().store_view().project_observations {
            if key.remote != session.name || key.kind == ObservationKind::Revision {
                continue;
            }
            for evidence in values.iter().flatten() {
                let Some((kind, source)) = source_ref(&evidence.raw_ref) else {
                    return Err(user_error(
                        "Stored conversion observation has an invalid wire reference",
                    ));
                };
                if !selected(kind, source) {
                    continue;
                }
                if session.local_name(source) != key.name {
                    return Err(user_error(
                        "Stored conversion observation does not match this binding's reference label",
                    ));
                }
                converted
                    .entry(evidence.raw_ref.clone())
                    .or_insert_with(Converted::absent);
            }
        }
    }
    let mut old_canonical = BTreeMap::new();
    for (kind, prefix) in [
        (GitRefKind::Bookmark, "refs/remotes/"),
        (GitRefKind::Tag, jj_lib::git::REMOTE_TAG_REF_NAMESPACE),
    ] {
        let prefix = format!("{prefix}{}/", session.name.as_str());
        for (name, id) in refs_prefixed(&git, &prefix)? {
            if let Some(source) = session.source_name(RefName::new(&name[prefix.len()..]))
                && selected(kind, source)
            {
                old_canonical.insert(name.to_owned(), id);
                let source_prefix = if kind == GitRefKind::Bookmark {
                    "refs/heads/"
                } else {
                    "refs/tags/"
                };
                converted.insert(format!("{source_prefix}{source}"), Converted::absent());
            }
        }
        // Restoring an operation can leave a last-imported mirror entry without
        // either a physical ref or a remote-view entry. It still needs an
        // explicit selected absence observation.
        for name in repo.view().git_refs().keys() {
            if let Some(local) = name.as_str().strip_prefix(&prefix)
                && let Some(source) = session.source_name(RefName::new(local))
                && selected(kind, source)
            {
                let source_prefix = if kind == GitRefKind::Bookmark {
                    "refs/heads/"
                } else {
                    "refs/tags/"
                };
                converted
                    .entry(format!("{source_prefix}{source}"))
                    .or_insert_with(Converted::absent);
            }
        }
    }
    if let Some(view) = repo.view().get_remote_view(&session.name) {
        for (kind, refs) in [
            (GitRefKind::Bookmark, &view.bookmarks),
            (GitRefKind::Tag, &view.tags),
        ] {
            for name in refs.keys() {
                if let Some(source) = session.source_name(name)
                    && selected(kind, source)
                {
                    let prefix = if kind == GitRefKind::Bookmark {
                        "refs/heads/"
                    } else {
                        "refs/tags/"
                    };
                    converted.insert(format!("{prefix}{source}"), Converted::absent());
                }
            }
        }
    }
    for (kind, expression) in [
        (GitRefKind::Bookmark, &selection.bookmark),
        (GitRefKind::Tag, &selection.tag),
    ] {
        for name in expression
            .exact_strings()
            .filter(|name| selected(kind, name))
        {
            let prefix = if kind == GitRefKind::Bookmark {
                "refs/heads/"
            } else {
                "refs/tags/"
            };
            converted.insert(format!("{prefix}{name}"), Converted::absent());
        }
    }

    let url = gix::url::parse(endpoint.as_str()).map_err(user_error)?;
    let native_path =
        if session.whole() && session.project.is_some() && url.scheme == gix::url::Scheme::File {
            let path = gix::path::from_bstr(url.path.as_bstr()).into_owned();
            path.join(".jj").is_dir().then_some(path)
        } else {
            None
        };
    let mut annotations = BTreeMap::new();
    let mut raw_terms = BTreeMap::<String, Vec<ConversionTerm>>::new();
    let mut raw_oids = BTreeMap::<String, gix::ObjectId>::new();
    let mut generations = BTreeMap::<String, String>::new();
    if let Some(path) = native_path {
        if !revisions.is_empty() {
            return Err(user_error(
                "Literal Git revisions require a Git endpoint, not a native JJ workspace",
            ));
        }
        let transaction = transaction
            .as_ref()
            .expect("native project has a Josh transaction");
        let workspace = command.load_workspace_at(&path, command.settings())?;
        let source_git = jj_lib::git::get_git_backend(workspace.repo_loader().store())?;
        if std::fs::canonicalize(source_git.git_repo_path())?
            == std::fs::canonicalize(&session.git_path)?
        {
            return Err(user_error(
                "Native fetch requires an external source repository",
            ));
        }
        let source =
            crate::native_source::NativeSource::read_selected(workspace.repo_loader(), &selection)
                .await
                .map_err(user_error)?;
        // Native JJ sources also have Git annotation objects outside their
        // commit-target view. Preserve them only when the mirror still agrees
        // with that view, and reject signed conversion before any observations.
        let source_objects = source_git.git_repo();
        for (name, target) in &source.view.local_tags {
            let Some(commit) = target.as_normal() else {
                continue;
            };
            let reference_name = format!("refs/tags/{}", name.as_str());
            let Some(reference) = source_objects
                .try_find_reference(reference_name.as_str())
                .map_err(user_error)?
            else {
                continue;
            };
            let Some(id) = reference.try_id() else {
                continue;
            };
            let peeled = id
                .object()
                .map_err(user_error)?
                .peel_tags_to_end()
                .map_err(user_error)?;
            if peeled.kind == gix::objs::Kind::Commit && peeled.id.as_bytes() == commit.as_bytes() {
                raw_oids.insert(reference_name.clone(), id.detach());
                annotations.insert(
                    format!("refs/tags/{}", name.as_str()),
                    crate::remote_refs::CommitTag::read(&source_objects, id.detach())
                        .map_err(user_error)?,
                );
            }
        }
        let project = session
            .project
            .as_ref()
            .expect("native project checked above");
        let known = session.anchors(repo, transaction).await?;
        let imported = crate::native_import::rewrite_graph(
            &source,
            repo,
            &project.name,
            &project.mount,
            known,
        )
        .await
        .map_err(user_error)?;
        for (prefix, refs) in [
            ("refs/heads/", &source.view.local_bookmarks),
            ("refs/tags/", &source.view.local_tags),
        ] {
            for (name, target) in refs {
                raw_terms.insert(
                    format!("{prefix}{}", name.as_str()),
                    target
                        .as_merge()
                        .iter()
                        .map(|term| ConversionTerm {
                            canonical: term.as_ref().map(|id| imported.ids[id].clone()),
                            raw: term.as_ref().map(|id| id.hex()),
                        })
                        .collect(),
                );
                let target = RefTarget::from_merge(
                    target
                        .as_merge()
                        .map(|term| term.as_ref().map(|id| imported.ids[id].clone())),
                );
                converted.insert(
                    format!("{prefix}{}", name.as_str()),
                    Converted::native(target)?,
                );
            }
        }
        crate::native_project::record_imported_boundaries(
            transaction,
            session.binding_id(),
            &imported,
            source
                .view
                .local_bookmarks
                .values()
                .chain(source.view.local_tags.values())
                .flat_map(|target| target.as_merge().iter().flatten()),
        )
        .map_err(user_error)?;
        transaction.flush_mem_odb().map_err(user_error)?;
    } else {
        let source_repo = session
            .filter()
            .is_some()
            .then(|| crate::source_repo::SourceRepo::open(&git, &endpoint))
            .transpose()
            .map_err(user_error)?;
        let raw_git = source_repo.as_ref().map_or(&git, |source| source.git());
        if (options.deepen.is_some() || options.unshallow)
            && raw_git
                .shallow_commits()
                .map_err(user_error)?
                .is_none_or(|ids| ids.is_empty())
        {
            return Err(user_error(
                "Cannot deepen a source that is not shallow; use --depth for its initial shallow fetch",
            ));
        }
        let shallow = if let Some(depth) = options.depth {
            gix::remote::fetch::Shallow::DepthAtRemote(depth)
        } else if let Some(depth) = options.deepen {
            gix::remote::fetch::Shallow::Deepen(depth.get())
        } else if options.unshallow {
            gix::remote::fetch::Shallow::undo()
        } else {
            gix::remote::fetch::Shallow::NoChange
        };
        let raw_prefix = session.raw_prefix(&git, &endpoint).map_err(user_error)?;
        if let Some(source) = &source_repo {
            let prefix = session.raw_prefix(&git, &endpoint).map_err(user_error)?;
            source.migrate_context(&git, &prefix).map_err(user_error)?;
        }
        let mut old_raw = BTreeMap::new();
        for (name, id) in refs_prefixed(raw_git, &raw_prefix)? {
            let source = &name[raw_prefix.len()..];
            if source_ref(source).is_some_and(|(kind, name)| selected(kind, name))
                || source.strip_prefix("pins/").is_some_and(|pin| {
                    options
                        .revisions
                        .iter()
                        .any(|id| id.eq_ignore_ascii_case(pin))
                })
            {
                old_raw.insert(name.to_owned(), id);
                converted
                    .entry(source.to_owned())
                    .or_insert_with(Converted::absent);
            }
        }
        let outcome = crate::git_transport::fetch::fetch(
            if options.fetch_url.is_some() {
                raw_git.remote_at(endpoint.as_str()).map_err(user_error)?
            } else {
                session
                    .remote(raw_git, gix::remote::Direction::Fetch)
                    .map_err(user_error)?
            },
            |reference| {
                std::str::from_utf8(reference.unpack().0)
                    .ok()
                    .and_then(source_ref)
                    .is_some_and(|(kind, name)| selected(kind, name))
            },
            &revisions,
            shallow,
            &AtomicBool::new(false),
        )
        .map_err(user_error)?;
        let mut advertised = BTreeMap::new();
        for reference in &outcome.advertised {
            let (name, id, ..) = reference.unpack();
            let Ok(name) = std::str::from_utf8(name) else {
                continue;
            };
            if source_ref(name).is_some_and(|(kind, name)| selected(kind, name)) {
                advertised.insert(
                    name.to_owned(),
                    id.filter(|id| !id.is_null()).map(ToOwned::to_owned),
                );
                converted
                    .entry(name.to_owned())
                    .or_insert_with(Converted::absent);
            }
        }
        // Repository-wide inputs have no project mapping. A source ref that
        // happens to spell a registered label must not claim canonical scope.
        // Check the whole selection before raw refs, mirrors, or leases change.
        if session.project.is_none() {
            for source in converted.keys() {
                if let Some((_, name)) = source_ref(source)
                    && let Some((_, label)) = name.rsplit_once('#')
                    && session
                        .state
                        .resolve_label(label)
                        .map_err(user_error)?
                        .is_some()
                {
                    return Err(user_error(format!(
                        "Raw Git ref {source} occupies a registered project label"
                    )));
                }
            }
        }
        let mut new_raw = BTreeMap::new();
        let mut received = BTreeMap::new();
        for reference in &outcome.received {
            let (name, id, ..) = reference.unpack();
            let name = std::str::from_utf8(name).map_err(user_error)?;
            let id = id
                .ok_or_else(|| user_error("Received reference has no object ID"))?
                .to_owned();
            new_raw.insert(format!("{raw_prefix}{name}"), id);
            received.insert(name.to_owned(), id);
        }
        for id in &revisions {
            let name = format!("pins/{id}");
            new_raw.insert(format!("{raw_prefix}{name}"), *id);
            received.insert(name, *id);
        }
        let mut commits = BTreeMap::new();
        for (name, direct) in &received {
            let object = raw_git
                .find_object(*direct)
                .map_err(user_error)?
                .peel_tags_to_end()
                .map_err(user_error)?;
            if object.kind == gix::objs::Kind::Commit {
                if session.binding.is_some() && name.starts_with("refs/tags/") {
                    // Preflight the entire selected batch before installing refs
                    // or granting endpoint leases. Keep received immutable objects,
                    // but never manufacture an unsigned mirror of a signed tag.
                    annotations.insert(
                        name.clone(),
                        crate::remote_refs::CommitTag::read(raw_git, *direct)
                            .map_err(user_error)?,
                    );
                }
                commits.insert(name.clone(), object.id);
            } else if name.starts_with("pins/") {
                return Err(user_error(format!(
                    "Revision {direct} does not peel to a commit"
                )));
            }
        }
        raw_oids.extend(received.iter().map(|(name, id)| (name.clone(), *id)));
        // No keep file is removed if raw publication fails. Even noncommit tag
        // objects retain their exact direct IDs in this private namespace.
        install_refs(raw_git, &old_raw, &new_raw)?;
        for path in outcome.keep_paths {
            std::fs::remove_file(path).map_err(user_error)?;
        }
        if let Some(project) = session.project.as_ref().filter(|_| session.whole()) {
            let transaction = transaction
                .as_ref()
                .expect("native project has a Josh transaction");
            let known = session.anchors(repo, transaction).await?;
            let ids: Vec<_> = commits
                .values()
                .map(|id| CommitId::from_bytes(id.as_bytes()))
                .collect();
            jj_lib::git::get_git_backend(repo.store())?.import_head_commits(ids.iter())?;
            let view = View::make_root(repo.store().root_commit_id().clone());
            let source =
                crate::native_source::NativeSource::read_view(repo.store().clone(), view, &ids)
                    .await
                    .map_err(user_error)?;
            let imported = crate::native_import::rewrite_graph(
                &source,
                repo,
                &project.name,
                &project.mount,
                known,
            )
            .await
            .map_err(user_error)?;
            for (name, raw) in &commits {
                let id = &imported.ids[&CommitId::from_bytes(raw.as_bytes())];
                converted.insert(
                    name.clone(),
                    Converted::native(RefTarget::normal(id.clone()))?,
                );
            }
            crate::native_project::record_imported_boundaries(
                transaction,
                session.binding_id(),
                &imported,
                &ids,
            )
            .map_err(user_error)?;
        } else if let Some(filter) = session.filter() {
            let source = source_repo
                .as_ref()
                .expect("filtered fetch has an isolated source");
            let source_tx = crate::interop::open_josh_transaction(source.path(), false)?;
            let tips: Vec<_> = commits.values().copied().collect();
            let normalized = source.normalize(&source_tx, &tips).map_err(user_error)?;
            for (name, raw) in &commits {
                generations.insert(
                    name.clone(),
                    crate::source_repo::generation(&endpoint, normalized.tips[raw]),
                );
            }
            crate::interop::check_raw_projectable_history(
                &source_tx,
                normalized.tips.values().copied(),
            )?;
            let filter = crate::projection_history::map_source_ids(filter, &normalized.pairs);
            let mut matches = crate::projection_history::ProjectedMatches::new();
            for (name, raw) in &commits {
                let filtered = josh_core::filter_commit(&source_tx, filter, normalized.tips[raw])
                    .map_err(user_error)?;
                if filtered.is_null() {
                    continue;
                }
                let canonical = if let Some(project) = &session.project {
                    source_tx.flush_mem_odb().map_err(user_error)?;
                    source
                        .copy_complete_to(&git, &[filtered])
                        .map_err(user_error)?;
                    crate::projection_history::canonicalize_filtered_graph(
                        repo,
                        &source_tx,
                        Path::new(project.mount.as_internal_file_string()),
                        filtered,
                        &mut matches,
                        reuse_history,
                    )
                    .await?
                } else {
                    CommitId::from_bytes(filtered.as_bytes())
                };
                converted.insert(
                    name.clone(),
                    Converted::native(RefTarget::normal(canonical))?,
                );
            }
            source_tx.flush_mem_odb().map_err(user_error)?;
            let canonical: Vec<_> = converted
                .values()
                .filter_map(|value| value.mirror)
                .collect();
            source
                .copy_complete_to(&git, &canonical)
                .map_err(user_error)?;
            source
                .record_normalized(&source_tx, &normalized)
                .map_err(user_error)?;
            source
                .retain_observations(&source_tx, &received.values().copied().collect::<Vec<_>>())
                .map_err(user_error)?;
            source_tx.flush_mem_odb().map_err(user_error)?;
        } else {
            for (name, commit) in &commits {
                converted.insert(
                    name.clone(),
                    Converted {
                        target: RefTarget::normal(CommitId::from_bytes(commit.as_bytes())),
                        mirror: Some(received[name]),
                    },
                );
            }
        }
        // Only a successfully converted selection advances publication leases.
        if let Some(transaction) = &transaction {
            for source in converted.keys().filter(|name| source_ref(name).is_some()) {
                match advertised.get(source).copied().flatten() {
                    Some(id) => {
                        crate::remote_refs::record_observation(transaction, &endpoint, source, id)?
                    }
                    None => crate::remote_refs::record_absence(transaction, &endpoint, source)?,
                }
            }
        }
    }
    for (source, tag) in annotations {
        if session.whole() {
            tag.copy_annotations_to(&git).map_err(user_error)?;
        }
        if let Some(converted) = converted.get_mut(&source)
            && let Some(commit) = converted.mirror
        {
            let name = source
                .strip_prefix("refs/tags/")
                .expect("tag classified above");
            converted.mirror = Some(
                tag.retarget(&git, commit, session.local_name(name).as_str())
                    .map_err(user_error)?,
            );
        }
    }
    // Generated objects and native boundary refs must be durable before exposing
    // any canonical mirror. No raw commit is added to the destination's heads.
    if let Some(transaction) = &transaction {
        transaction.flush_mem_odb().map_err(user_error)?;
    }
    let pins: Vec<_> = converted
        .keys()
        .filter(|name| name.starts_with("pins/"))
        .cloned()
        .collect();
    for pin in pins {
        let converted = converted.remove(&pin).expect("selected pinned revision");
        let Some(id) = converted.target.as_normal() else {
            return Err(user_error(format!(
                "Revision {} has no content through this projection",
                &pin[5..]
            )));
        };
        jj_lib::git::get_git_backend(repo.store())?.import_head_commits([id])?;
        let commit = repo.store().get_commit_async(id).await?;
        repo.add_head(&commit).await?;
        if session.binding.is_some() {
            let raw = raw_oids.get(&pin).expect("received literal revision");
            let connection = session.connection.as_ref().expect("binding connection");
            let evidence = session.evidence(
                connection,
                &endpoint,
                String::new(),
                vec![ConversionTerm {
                    canonical: Some(id.clone()),
                    raw: Some(raw.to_string()),
                }],
                None,
                generations.remove(&pin),
            );
            let view = repo.view_mut().store_view_mut();
            view.remote_connections.insert(
                session.name.clone(),
                Merge::resolved(Some(connection.clone())),
            );
            jj_lib::view::remote_observations::RemoteObservations::new(view).record(
                ObservationKey {
                    remote: session.name.clone(),
                    name: raw.to_string().into(),
                    kind: ObservationKind::Revision,
                },
                evidence,
            );
        }
        writeln!(
            ui.status(),
            "Fetched revision {} as {}",
            &pin[5..],
            id.hex()
        )?;
    }
    let mut mirrors = BTreeMap::new();
    if session.whole()
        && let Some(transaction) = &transaction
    {
        for raw in raw_oids.values() {
            let name = format!(
                "{}observed/{raw}",
                crate::native_project::binding_ref_prefix(session.binding_id())
            );
            let old = transaction.resolve_ref(&name).map_err(user_error)?;
            transaction
                .update_ref(
                    &name,
                    old.map_or(
                        josh_core::cache::Expected::Absent,
                        josh_core::cache::Expected::At,
                    ),
                    *raw,
                    "retain direct native observation",
                )
                .map_err(user_error)?;
        }
        transaction.flush_mem_odb().map_err(user_error)?;
    }
    let mut observations = Vec::with_capacity(converted.len());
    for (source, converted) in converted {
        let (kind, name) = source_ref(&source).expect("source names classified above");
        if let Some(id) = converted.mirror {
            if session.whole()
                && !raw_oids.contains_key(&source)
                && let Some(terms) = raw_terms.get(&source)
                && terms.len() == 1
                && let Some(raw) = &terms[0].raw
            {
                raw_oids.insert(
                    source.clone(),
                    gix::ObjectId::from_hex(raw.as_bytes()).map_err(user_error)?,
                );
            }
            mirrors.insert(canonical_ref(session, kind, name), id);
        }
        let canonical_git_oid = converted
            .target
            .as_normal()
            .map(|id| gix::ObjectId::try_from(id.as_bytes()))
            .transpose()
            .map_err(user_error)?;
        let evidence = session.binding.as_ref().map(|_| {
            let mut terms = raw_terms.remove(&source).unwrap_or_else(|| {
                vec![ConversionTerm {
                    canonical: converted.target.as_normal().cloned(),
                    raw: raw_oids.get(&source).map(ToString::to_string),
                }]
            });
            if kind == GitRefKind::Tag
                && terms.len() == 1
                && let Some(raw) = raw_oids.get(&source)
            {
                terms[0].raw = Some(raw.to_string());
            }
            session.evidence(
                session.connection.as_ref().expect("binding connection"),
                &endpoint,
                source.clone(),
                terms,
                None,
                generations.remove(&source),
            )
        });
        observations.push(GitRemoteObservation {
            kind,
            symbol: RemoteRefSymbolBuf {
                name: session.local_name(name),
                remote: session.name.clone(),
            },
            target: converted.target,
            canonical_git_oid,
            evidence,
        });
    }
    install_refs(&git, &old_canonical, &mirrors)?;
    Ok(observations)
}
