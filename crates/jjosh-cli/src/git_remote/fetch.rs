use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use gix::bstr::ByteSlice as _;
use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::{CommandError, user_error};
use jj_cli::git_remote::GitRemoteSession as _;
use jj_cli::ui::Ui;
use jj_lib::backend::CommitId;
use jj_lib::git::{GitFetchRefExpression, GitRefKind, GitRemoteObservation};
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::{RefTarget, View};
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

/// The logical target and the physical mirror differ for identity annotated tags.
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
    _ui: &mut Ui,
    command: &CommandHelper,
    repo: &mut MutableRepo,
    selection: GitFetchRefExpression,
) -> Result<Vec<GitRemoteObservation>, CommandError> {
    let git = jj_lib::git::get_git_backend(repo.store())?.git_repo();
    let endpoint = session
        .endpoint_url(&git, gix::remote::Direction::Fetch)
        .map_err(user_error)?;
    // Ordinary identity remotes remain hash-agnostic and never open Josh's
    // SHA-1-only object/cache transaction or its projection lease ledger.
    let transaction = if session.project.is_some() || session.josh.is_some() {
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
    // Keyed by full source names, never logical/project-scoped names. Start with
    // absence so selected refs missing from the advertisement also get observed.
    let mut converted = BTreeMap::<String, Converted>::new();
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
    let native_path = if session
        .project
        .as_ref()
        .is_some_and(|project| project.native)
        && url.scheme == gix::url::Scheme::File
    {
        let path = gix::path::from_bstr(url.path.as_bstr()).into_owned();
        path.join(".jj").is_dir().then_some(path)
    } else {
        None
    };
    if let Some(path) = native_path {
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
        let project = session
            .project
            .as_ref()
            .expect("native project checked above");
        let known = crate::native_project::anchors(repo, &transaction, &project.name)
            .await
            .map_err(user_error)?;
        let imported = crate::native_import::import_source(
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
        crate::native_project::record_mount(&transaction, &project.name, &project.mount)
            .map_err(user_error)?;
        crate::native_project::record_imported_boundaries(
            &transaction,
            &project.name,
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
        let raw_prefix = session.raw_prefix(&git, &endpoint).map_err(user_error)?;
        let mut old_raw = BTreeMap::new();
        for (name, id) in refs_prefixed(&git, &raw_prefix)? {
            let source = &name[raw_prefix.len()..];
            if source_ref(source).is_some_and(|(kind, name)| selected(kind, name)) {
                old_raw.insert(name.to_owned(), id);
                converted
                    .entry(source.to_owned())
                    .or_insert_with(Converted::absent);
            }
        }
        let outcome = crate::git_transport::fetch::fetch(
            session
                .remote(&git, gix::remote::Direction::Fetch)
                .map_err(user_error)?,
            |reference| {
                std::str::from_utf8(reference.unpack().0)
                    .ok()
                    .and_then(source_ref)
                    .is_some_and(|(kind, name)| selected(kind, name))
            },
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
        // No keep file is removed if raw publication fails. Even noncommit tag
        // objects retain their exact direct IDs in this private namespace.
        install_refs(&git, &old_raw, &new_raw)?;
        for path in outcome.keep_paths {
            std::fs::remove_file(path).map_err(user_error)?;
        }
        // Endpoint leases reflect the advertisement, not projection visibility.
        if let Some(transaction) = &transaction {
            for source in converted
                .keys()
                .filter(|name| name.starts_with("refs/heads/"))
            {
                match advertised.get(source).copied().flatten() {
                    Some(id) => {
                        crate::link_refs::record_observation(transaction, &endpoint, source, id)?
                    }
                    None => crate::link_refs::record_absence(transaction, &endpoint, source)?,
                }
            }
            transaction.flush_mem_odb().map_err(user_error)?;
        }
        let mut commits = BTreeMap::new();
        for (name, direct) in &received {
            let object = git
                .find_object(*direct)
                .map_err(user_error)?
                .peel_tags_to_end()
                .map_err(user_error)?;
            if object.kind == gix::objs::Kind::Commit {
                commits.insert(name.clone(), object.id);
            }
        }
        if let Some(project) = session.project.as_ref().filter(|project| project.native) {
            let transaction = transaction
                .as_ref()
                .expect("native project has a Josh transaction");
            let known = crate::native_project::anchors(repo, &transaction, &project.name)
                .await
                .map_err(user_error)?;
            let ids: Vec<_> = commits
                .values()
                .map(|id| CommitId::from_bytes(id.as_bytes()))
                .collect();
            jj_lib::git::get_git_backend(repo.store())?.import_head_commits(ids.iter())?;
            let mut view = View::make_root(repo.store().root_commit_id().clone());
            view.head_ids.extend(ids.iter().cloned());
            let source = crate::native_source::NativeSource::read_view(
                repo.store().clone(),
                repo.op_store().clone(),
                view,
                String::new(),
            )
            .await
            .map_err(user_error)?;
            let imported = crate::native_import::import_source(
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
            crate::native_project::record_mount(&transaction, &project.name, &project.mount)
                .map_err(user_error)?;
            crate::native_project::record_imported_boundaries(
                &transaction,
                &project.name,
                &imported,
                &ids,
            )
            .map_err(user_error)?;
        } else if let Some(filter) = session.filter() {
            let transaction = transaction
                .as_ref()
                .expect("projection has a Josh transaction");
            crate::interop::check_raw_projectable_history(&transaction, commits.values().copied())?;
            let mut matches = crate::link_fetch::ProjectedMatches::new();
            for (name, raw) in &commits {
                let filtered = josh_core::filter_commit(&transaction, filter.clone(), *raw)
                    .map_err(user_error)?;
                if filtered.is_null() {
                    continue;
                }
                let canonical = if let Some(project) = &session.project {
                    crate::link_fetch::canonicalize_filtered_graph(
                        repo,
                        &transaction,
                        Path::new(project.mount.as_internal_file_string()),
                        filtered,
                        &mut matches,
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
    }
    // Generated objects and native boundary refs must be durable before exposing
    // any canonical mirror. No raw commit is added to the destination's heads.
    if let Some(transaction) = &transaction {
        transaction.flush_mem_odb().map_err(user_error)?;
    }
    let mut mirrors = BTreeMap::new();
    let mut observations = Vec::with_capacity(converted.len());
    for (source, converted) in converted {
        let (kind, name) = source_ref(&source).expect("source names classified above");
        if let Some(id) = converted.mirror {
            mirrors.insert(canonical_ref(session, kind, name), id);
        }
        let canonical_git_oid = converted
            .target
            .as_normal()
            .map(|id| gix::ObjectId::try_from(id.as_bytes()))
            .transpose()
            .map_err(user_error)?;
        observations.push(GitRemoteObservation {
            kind,
            symbol: RemoteRefSymbolBuf {
                name: session.local_name(name),
                remote: session.name.clone(),
            },
            target: converted.target,
            canonical_git_oid,
        });
    }
    install_refs(&git, &old_canonical, &mirrors)?;
    Ok(observations)
}
