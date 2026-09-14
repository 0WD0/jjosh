use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Result, bail};
use gix::bstr::{BString, ByteSlice};
use gix::remote::Direction;
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_cli::git_remote::{
    GitPreparedPush, GitRemotePushOptions, GitRemotePushOutcome, RemoteFuture,
};
use jj_lib::backend::CommitId;
use jj_lib::git::{GitPushOptions, GitPushRefTargets, GitPushStats, GitRefUpdate};
use jj_lib::object_id::ObjectId as _;
use jj_lib::project::{ConversionObservation, ConversionTerm, ObservationKey, ObservationKind, ProjectId};
use jj_lib::merge::Merge;
use jj_lib::ref_name::{GitRefNameBuf, RemoteName, RemoteNameBuf};
use jj_lib::repo::{MutableRepo, Repo as _};
use josh_core::cache::{Expected as RefExpected, Transaction};

use super::Session;
use crate::git_transport::push::{self as transport, Expected, RefStatus, Update};

struct Prepared {
    update: Update,
    publications: Vec<(CommitId, CommitId)>,
    scope: Option<usize>,
    evidence: Option<ConversionObservation>,
    normalized: Option<crate::source_repo::Normalized>,
}

struct PreparedPush {
    remote: RemoteNameBuf,
    remote_display: String,
    targets: GitPushRefTargets,
    canonical: Vec<GitRefUpdate>,
    scopes: Vec<(Session, Vec<(usize, String)>)>,
    prepared: Vec<Prepared>,
    transaction: Option<Transaction>,
    source_transactions: Vec<Option<Transaction>>,
    endpoint: String,
    sources: Vec<Option<crate::source_repo::SourceRepo>>,
    raw_prefix: Option<String>,
    destinations: Vec<GitRefNameBuf>,
    transport: transport::PreparedPush,
}

fn source_destination(
    session: &Session,
    update: &GitRefUpdate,
) -> Result<(String, Option<ProjectId>)> {
    let qualified = update.qualified_name.as_str();
    let (prefix, local) = if let Some(local) = qualified.strip_prefix("refs/heads/") {
        ("refs/heads/", local)
    } else if let Some(local) = qualified.strip_prefix("refs/tags/") {
        ("refs/tags/", local)
    } else {
        bail!("Unsupported logical push reference {qualified}");
    };
    let (source, scope) = match local.rsplit_once('#') {
        Some((source, label)) => match session.state.resolve_label(label).map_err(anyhow::Error::msg)? {
            Some(project) => (source, Some(project)),
            None => (local, None),
        },
        None => (local, None),
    };
    let destination = format!("{prefix}{source}");
    gix::validate::reference::name(destination.as_bytes().as_bstr())?;
    Ok((destination, scope))
}

fn cached_source_targets<'a>(
    view: &'a jj_lib::view::View,
    remote: &'a RemoteName,
) -> impl Iterator<
    Item = (
        ObservationKind,
        &'a jj_lib::ref_name::RefName,
        &'a jj_lib::op_store::RefTarget,
    ),
> {
    let recorded = view.get_remote_view(remote).into_iter().flat_map(|refs| {
        refs.bookmarks
            .iter()
            .map(|(name, value)| (ObservationKind::Bookmark, name.as_ref(), &value.target))
            .chain(
                refs.tags
                    .iter()
                    .map(|(name, value)| (ObservationKind::Tag, name.as_ref(), &value.target)),
            )
    });
    let mirrors = view.git_refs().iter().filter_map(move |(name, target)| {
        if let Some((kind, symbol)) = jj_lib::git::parse_git_ref(name) {
            let kind = match kind {
                jj_lib::git::GitRefKind::Bookmark => ObservationKind::Bookmark,
                jj_lib::git::GitRefKind::Tag => ObservationKind::Tag,
            };
            return (symbol.remote == remote).then_some((kind, symbol.name, target));
        }
        let suffix = name
            .as_str()
            .strip_prefix(jj_lib::git::REMOTE_TAG_REF_NAMESPACE)?;
        let (owner, name) = suffix.split_once('/')?;
        (owner == remote.as_str()).then_some((
            ObservationKind::Tag,
            jj_lib::ref_name::RefName::new(name),
            target,
        ))
    });
    recorded.chain(mirrors)
}

fn source_observation(
    scope: &Session,
    repo: &MutableRepo,
    destination: &str,
    preparation: &GitRemotePushOptions,
) -> Result<Option<ConversionObservation>, CommandError> {
    use jj_cli::git_remote::GitRemoteSession as _;
    let base = preparation.base.as_ref().or_else(|| scope.binding.as_ref().and_then(|(_, record)| record.base.as_ref()));
    let revision = base.and_then(|base| gix::ObjectId::from_hex(base.strip_prefix("pins/").unwrap_or(base).as_bytes()).ok());
    let key = if let Some(revision) = revision {
        ObservationKey { remote: scope.name.clone(), name: revision.to_string().into(), kind: ObservationKind::Revision }
    } else {
        let reference = base.map_or_else(|| destination.to_owned(), |base| if base.starts_with("refs/") { base.clone() } else { format!("refs/heads/{base}") });
        let (kind, name) = if let Some(name) = reference.strip_prefix("refs/tags/") { (ObservationKind::Tag, name) } else { (ObservationKind::Bookmark, reference.strip_prefix("refs/heads/").ok_or_else(|| user_error("Source context must name a bookmark or tag"))?) };
        ObservationKey { remote: scope.name.clone(), name: scope.local_name(name), kind }
    };
    repo.view()
        .validate_project_observation(&key)
        .map_err(user_error)?;
    let observation = repo
        .view()
        .store_view()
        .project_observations
        .get(&key)
        .map(|value| {
            value.as_resolved().ok_or_else(|| {
                user_error(
                    "Source conversion observations are conflicted; select an explicit \
                     unambiguous base",
                )
            })
        })
        .transpose()?
        .and_then(Option::as_ref);
    let Some(observation) = observation else {
        let cached = cached_source_targets(repo.view(), &scope.name).any(|(kind, name, target)| {
            kind == key.kind && name == key.name && !target.is_absent()
        });
        if base.is_some() || cached {
            return Err(user_error(
                "Selected source reference has no immutable conversion observation; historical \
                 tracking is not source evidence",
            ));
        }
        return Ok(None);
    };
    if &observation.binding_id != scope.binding_id()
        || Some(&observation.connection_id) != scope.connection.as_ref()
    {
        return Err(user_error(
            "Source observation belongs to another binding or connection",
        ));
    }
    if observation.terms.len() != 1 {
        return Err(user_error(
            "Source reference is conflicted; select an explicit unambiguous base",
        ));
    }
    if let Some(revision) = revision
        && (observation.terms[0].raw.as_deref() != Some(revision.to_string().as_str())
            || observation.generation.is_none())
    {
        return Err(user_error(
            "Literal source base lacks its exact raw revision and normalization generation",
        ));
    }
    Ok(Some(observation.clone()))
}

fn generation_endpoint(
    scope: &Session,
    git: &gix::Repository,
    observation: Option<&ConversionObservation>,
) -> Result<String, CommandError> {
    if let Some(observation) = observation {
        if let Some(generation) = &observation.generation {
            return crate::source_repo::parse_generation(generation).map(|(endpoint, _)| endpoint).map_err(user_error);
        }
        return Ok(observation.endpoint.clone());
    }
    scope
        .endpoint_url(git, Direction::Fetch)
        .map_err(user_error)
}

/// Reject unwitnessed cached ancestry before choosing an automatic source base.
async fn validate_cached_source_ancestry(
    scope: &Session,
    repo: &MutableRepo,
    git: &gix::Repository,
    head: &CommitId,
) -> Result<(), CommandError> {
    for (kind, name, target) in cached_source_targets(repo.view(), &scope.name) {
        for cached in target.added_ids() {
            // Recorded Git tag mirrors may point at an annotation, unlike the
            // peeled commit targets in remote_views.
            let raw = gix::ObjectId::try_from(cached.as_bytes()).map_err(user_error)?;
            let canonical = CommitId::from_bytes(peel_commit(git, raw)?.as_bytes());
            if !repo.index().is_ancestor(&canonical, head).await? {
                continue;
            }
            let key = ObservationKey {
                remote: scope.name.clone(),
                name: name.to_owned(),
                kind,
            };
            let witnessed = repo
                .view()
                .store_view()
                .project_observations
                .get(&key)
                .and_then(|value| value.as_resolved())
                .and_then(Option::as_ref)
                .is_some_and(|evidence| {
                    &evidence.binding_id == scope.binding_id()
                        && Some(&evidence.connection_id) == scope.connection.as_ref()
                        && evidence.terms.iter().any(|term| {
                            term.canonical.as_ref() == Some(&canonical) && term.raw.is_some()
                        })
                });
            if !witnessed {
                return Err(user_error(format!(
                    "Source ancestry includes unverified cached reference {}; select an \
                     explicitly witnessed --base",
                    repo.view()
                        .remote_ref_symbol(name.to_remote_symbol(&scope.name)),
                )));
            }
            repo.view()
                .validate_project_observation(&key)
                .map_err(user_error)?;
        }
    }
    Ok(())
}

async fn ancestral_observation(
    scope: &Session,
    repo: &MutableRepo,
    git: &gix::Repository,
    head: &CommitId,
) -> Result<Option<ConversionObservation>, CommandError> {
    let mut candidates = Vec::new();
    let mut has_source_history = false;
    for (key, values) in &repo.view().store_view().project_observations {
        for evidence in values.adds().flatten() {
            if &evidence.binding_id != scope.binding_id() { continue; }
            for term in &evidence.terms {
                let (Some(canonical), Some(_)) = (&term.canonical, &term.raw) else { continue; };
                has_source_history = true;
                if !repo.index().is_ancestor(canonical, head).await? { continue; }
                if !values.is_resolved() || evidence.terms.len() != 1 {
                    return Err(user_error("Source ancestry has conflicting conversion witnesses; choose an explicit --base"));
                }
                repo.view().validate_project_observation(key).map_err(user_error)?;
                candidates.push((canonical.clone(), evidence));
            }
        }
    }
    let mut maximal = Vec::new();
    for (index, (canonical, evidence)) in candidates.iter().enumerate() {
        let mut superseded = false;
        for (other_index, (other, _)) in candidates.iter().enumerate() {
            if index != other_index && canonical != other && repo.index().is_ancestor(canonical, other).await? {
                superseded = true;
                break;
            }
        }
        if !superseded { maximal.push(*evidence); }
    }
    let mut selected: Option<(&ConversionObservation, gix::ObjectId)> = None;
    for evidence in maximal {
        let raw = gix::ObjectId::from_hex(evidence.terms[0].raw.as_ref().expect("candidate raw witness").as_bytes()).map_err(user_error)?;
        let endpoint = generation_endpoint(scope, git, Some(evidence))?;
        let source = crate::source_repo::SourceRepo::open(git, &endpoint).map_err(user_error)?;
        let raw = peel_commit(source.git(), raw)?;
        if evidence.generation.is_none() {
            return Err(user_error("Source ancestry lacks an immutable normalization generation; choose an explicitly fetched --base"));
        }
        if let Some((previous, previous_raw)) = selected {
            if previous_raw != raw || previous.generation != evidence.generation {
                return Err(user_error("Source ancestry has multiple maximal raw contexts; choose an explicit --base"));
            }
        } else {
            selected = Some((evidence, raw));
        }
    }
    if selected.is_none() && has_source_history {
        return Err(user_error("No witnessed source context is an ancestor of this revision; choose an explicit --base"));
    }
    Ok(selected.map(|(evidence, _)| evidence.clone()))
}

fn save_raw_ref(transaction: &Transaction, name: &str, new: Option<gix::ObjectId>) -> Result<()> {
    let previous = transaction.resolve_ref(name)?;
    match new {
        Some(id) => transaction.update_ref(
            name,
            previous.map_or(RefExpected::Absent, RefExpected::At),
            id,
            "jjosh confirmed remote publication",
        )?,
        None => {
            if let Some(previous) = previous {
                transaction.delete_ref(name, RefExpected::At(previous))?;
            }
        }
    }
    transaction.flush_mem_odb()
}

fn canonical_stats(canonical: &[GitRefUpdate], report: &transport::Outcome) -> GitPushStats {
    let mut stats = GitPushStats::default();
    for (update, (_, status)) in canonical.iter().zip(&report.refs) {
        let name = update.qualified_name.clone();
        match status {
            RefStatus::Accepted => stats.pushed.push(name),
            RefStatus::Rejected(reason) => {
                let reason = String::from_utf8_lossy(reason).into_owned();
                if reason.starts_with("stale lease:") {
                    stats.rejected.push((name, Some(reason)));
                } else {
                    stats.remote_rejected.push((name, Some(reason)));
                }
            }
            RefStatus::Indeterminate => stats.remote_rejected.push((
                name,
                Some("Publication outcome is indeterminate; fetch before retrying".to_owned()),
            )),
            RefStatus::Planned => {}
        }
    }
    stats
}

pub(super) async fn prepare(
    session: &Session,
    repo: &mut MutableRepo,
    targets: &GitPushRefTargets,
    options: &GitPushOptions,
    preparation: &GitRemotePushOptions,
    dry_run: bool,
) -> Result<Box<dyn GitPreparedPush>, CommandError> {
    let canonical = jj_lib::git::prepare_push_refs(repo, &session.name, targets)?;
    let git = jj_lib::git::get_git_backend(repo.store())?.git_repo();
    // Resolve the actual selected receive-pack endpoint before conversion.
    // Native source workspaces are not publication endpoints.
    let remote = session.remote(&git, Direction::Push).map_err(user_error)?;
    let push_endpoint =
        super::remote_endpoint(&remote, Direction::Push, false).map_err(user_error)?;
    let mut scopes: Vec<(Session, Vec<(usize, String)>)> = Vec::new();
    let mut destinations = HashSet::with_capacity(canonical.len());
    for (index, update) in canonical.iter().enumerate() {
        let (destination, scope) = source_destination(session, update).map_err(user_error)?;
        if !destinations.insert(destination.clone()) {
            return Err(user_error(format!(
                "Multiple selected references map to push destination {destination}"
            )));
        }
        let conversion = session.push_scope(&git, scope.as_ref(), preparation.source.as_deref())?;
        if conversion.filter().is_none() && (preparation.base.is_some() || preparation.merge) {
            return Err(user_error("--base and --merge require a Josh projection"));
        }
        scopes.push((conversion, vec![(index, destination)]));
    }
    let mut contexts = Vec::with_capacity(scopes.len());
    for (scope, updates) in &scopes {
        if canonical[updates[0].0].targets.after.is_none() {
            // Deletion needs an independently valid wire lease, not conversion.
            contexts.push(None);
            continue;
        }
        let mut context = if scope.filter().is_some() {
            source_observation(scope, repo, &updates[0].1, preparation)?
        } else {
            None
        };
        if scope.filter().is_some()
            && preparation.base.is_none()
            && scope
                .binding
                .as_ref()
                .and_then(|(_, record)| record.base.as_ref())
                .is_none()
            && let Some(head) = canonical[updates[0].0].targets.after
        {
            let head = CommitId::from_bytes(peel_commit(&git, head)?.as_bytes());
            validate_cached_source_ancestry(scope, repo, &git, &head).await?;
            if context
                .as_ref()
                .is_none_or(|value| value.terms[0].raw.is_none())
                && let Some(ancestor) = ancestral_observation(scope, repo, &git, &head).await?
            {
                context = Some(ancestor);
            }
        }
        contexts.push(context);
    }
    let sources: Vec<_> = scopes.iter().zip(&contexts).map(|((scope, _), context)| {
        scope.filter().map(|_| {
            let endpoint = generation_endpoint(scope, &git, context.as_ref())?;
            crate::source_repo::SourceRepo::open(&git, &endpoint).map_err(user_error)
        }).transpose()
    }).collect::<Result<_, CommandError>>()?;
    let transformed = scopes
        .iter()
        .any(|(scope, _)| scope.binding.is_some());
    let transaction = transformed
        .then(|| crate::interop::open_josh_transaction(&session.git_path, true))
        .transpose()?;
    let raw_prefix = transformed
        .then(|| session.raw_prefix(&git, &push_endpoint).map_err(user_error))
        .transpose()?;
    let mut prepared: Vec<Option<Prepared>> = (0..canonical.len()).map(|_| None).collect();
    // Resolve and prepare every selected scope before opening a mutation request.
    for (scope_index, (scope, updates)) in scopes.iter().enumerate() {
        if scope.project.is_none() && scope.filter().is_none() {
            for (index, destination) in updates {
                let update = &canonical[*index];
                let canonical_commit = if scope.binding.is_some() {
                    if session.connection.is_none() { return Err(user_error("Converted publication requires an identified destination connection")); }
                    update.targets.after.map(|id| peel_commit(&git, id)).transpose()?
                } else { None };
                prepared[*index] = Some(Prepared {
                    update: Update {
                        name: BString::from(destination.as_str()),
                        expected: if scope.binding.is_some() {
                            crate::remote_refs::observation(transaction.as_ref().unwrap(), &push_endpoint, destination)?
                        } else { update.targets.before.map_or(Expected::Absent, Expected::At) },
                        new: update.targets.after,
                    },
                    publications: Vec::new(),
                    scope: scope.binding.as_ref().map(|_| scope_index),
                    evidence: if let Some(connection) = &session.connection {
                        scope.binding.as_ref().map(|_| scope.evidence(connection, &push_endpoint, destination.clone(), vec![ConversionTerm {
                            canonical: canonical_commit.map(|id| CommitId::from_bytes(id.as_bytes())),
                            raw: update.targets.after.map(|id| id.to_string()),
                        }], None, None))
                    } else { None },
                    normalized: None,
                });
            }
        } else {
            for (index, update) in prepare_scope(
                scope,
                scope_index,
                updates,
                &canonical,
                repo,
                &git,
                transaction.as_ref().unwrap(),
                &push_endpoint,
                preparation,
                sources[scope_index].as_ref(),
                session.connection.as_ref(),
                contexts[scope_index].as_ref(),
            )
            .await?
            {
                prepared[index] = Some(update);
            }
        }
    }
    let mut prepared: Vec<_> = prepared.into_iter().map(Option::unwrap).collect();
    if let Some(transaction) = &transaction {
        // Persist only objects for the independent transport ODB. The ephemeral
        // conversion transaction discards filter refs and correspondence writes.
        transaction.flush_mem_odb().map_err(user_error)?;
    }
    let mut updates: Vec<_> = prepared
        .iter()
        .map(|prepared| prepared.update.clone())
        .collect();
    let source_objects: Vec<_> = sources.iter().flatten().map(|source| source.git()).collect();
    let transfer_repo = if source_objects.len() > 1 {
        Some(crate::source_repo::transport_repository(&git, &source_objects).map_err(user_error)?)
    } else {
        None
    };
    let objects = transfer_repo.as_ref().map(|(_, git)| &git.objects)
        .or_else(|| source_objects.first().map(|git| &git.objects))
        .unwrap_or(&git.objects);
    let transport = transport::prepare(
        &git,
        remote,
        objects,
        &mut updates,
        &transport::Options {
            atomic: false,
            push_options: options
                .remote_push_options
                .iter()
                .map(|value| BString::from(value.as_str()))
                .collect(),
        },
    )
    .map_err(user_error)?;
    for (prepared, update) in prepared.iter_mut().zip(&updates) {
        prepared.update.expected = update.expected;
    }
    // Success records use a fresh transaction, never the conversion transaction's
    // queued refs. Opening it now also keeps local preparation failures pre-send.
    drop(transaction);
    let transaction = transformed
        .then(|| crate::interop::open_josh_transaction(&session.git_path, dry_run))
        .transpose()?;
    let source_transactions: Vec<_> = sources.iter().map(|source| {
        source.as_ref().map(|source| crate::interop::open_josh_transaction(source.path(), dry_run)).transpose()
    }).collect::<Result<_, CommandError>>()?;
    let destinations = updates
        .iter()
        .map(|update| {
            GitRefNameBuf::from(
                std::str::from_utf8(&update.name).expect("validated UTF-8 destination"),
            )
        })
        .collect();
    Ok(Box::new(PreparedPush {
        remote: session.name.clone(),
        remote_display: repo.view().remote_qualified_name(&session.name),
        targets: targets.clone(),
        canonical,
        scopes,
        prepared,
        transaction,
        source_transactions,
        endpoint: push_endpoint,
        raw_prefix,
        destinations,
        sources,
        transport,
    }))
}

fn session_remote_display(session: &Session) -> String {
    if let Some(identity) = session
        .connection
        .as_ref()
        .and_then(|connection| session.state.remote_names.get(connection))
        .and_then(Merge::as_resolved)
        .and_then(Option::as_ref)
        && let Some(project) = &session.project
    {
        return format!("{}#{}", identity.name.as_str(), project.label);
    }
    session.name.as_str().to_owned()
}

impl GitPreparedPush for PreparedPush {
    fn destinations(&self) -> (&str, &[GitRefNameBuf]) {
        (&self.endpoint, &self.destinations)
    }

    fn describe(&self, ui: &mut jj_cli::ui::Ui) -> Result<(), CommandError> {
        // Display URLs through their password-redacting formatter, never the
        // credential-bearing serialization used for endpoint identity.
        let endpoint = gix::url::parse(self.endpoint.as_bytes().as_bstr())
            .map_err(|_| user_error("Prepared publication endpoint is not a valid URL"))?;
        writeln!(ui.status(), "  Endpoint: {endpoint}")?;
        for (canonical, prepared) in self.canonical.iter().zip(&self.prepared) {
            let name = canonical.qualified_name.as_str();
            let name = name
                .strip_prefix("refs/heads/")
                .or_else(|| name.strip_prefix("refs/tags/"))
                .expect("prepared bookmark or tag");
            let action = if prepared.update.new.is_none() {
                "delete"
            } else if matches!(prepared.update.expected, Expected::Absent) {
                "create"
            } else {
                "update"
            };
            writeln!(
                ui.status(),
                "  {name:?} -> {} -> {} ({action})",
                self.remote_display,
                prepared.update.name
            )?;
            if let Some(index) = prepared.scope {
                let scope = &self.scopes[index].0;
                if let Some(project) = &scope.project {
                    writeln!(
                        ui.status(),
                        "    Project: {} (#{}), path: {}",
                        project.name,
                        project.label,
                        project.mount.as_internal_file_string()
                    )?;
                }
                let (id, binding) = scope.binding.as_ref().expect("prepared conversion binding");
                writeln!(
                    ui.status(),
                    "    Source: {} (binding {})",
                    session_remote_display(scope),
                    id.hex()
                )?;
                writeln!(
                    ui.status(),
                    "    Representation: {:?}",
                    binding.representation
                )?;
                if let Some(base) = prepared
                    .evidence
                    .as_ref()
                    .and_then(|evidence| evidence.base.as_ref())
                {
                    writeln!(ui.status(), "    Resolved base: {base}")?;
                }
            }
            match prepared.update.expected {
                Expected::Unknown => writeln!(ui.status(), "    Expected old: unknown")?,
                Expected::Absent => writeln!(ui.status(), "    Expected old: absent")?,
                Expected::At(id) => writeln!(ui.status(), "    Expected old: {id}")?,
            }
            if let Some(id) = prepared.update.new {
                writeln!(ui.status(), "    New raw target: {id}")?;
            }
        }
        Ok(())
    }

    fn publish<'a>(
        self: Box<Self>,
        repo: &'a mut MutableRepo,
    ) -> RemoteFuture<'a, GitRemotePushOutcome> {
        Box::pin(async move {
            let Self {
                remote,
                targets,
                canonical,
                scopes,
                prepared,
                transaction,
                source_transactions,
                sources,
                endpoint: push_endpoint,
                raw_prefix,
                transport,
                ..
            } = *self;
            let report = transport.publish();
            let mut save_errors = Vec::new();
            for (index, prepared) in prepared.iter().enumerate() {
                if report.refs[index].1 != RefStatus::Accepted { continue; }
                if let Some(evidence) = &prepared.evidence {
                    let qualified = canonical[index].qualified_name.as_str();
                    let (kind, name) = if let Some(name) = qualified.strip_prefix("refs/tags/") { (ObservationKind::Tag, name) } else { (ObservationKind::Bookmark, qualified.strip_prefix("refs/heads/").expect("prepared bookmark")) };
                    let view = repo.view_mut().store_view_mut();
                    view.remote_connections.insert(remote.clone(), Merge::resolved(Some(evidence.connection_id.clone())));
                    view.project_observations.insert(ObservationKey { remote: remote.clone(), name: name.into(), kind }, Merge::resolved(Some(evidence.clone())));
                }
            }
            if let Some(transaction) = &transaction {
                let push_prefix = raw_prefix.as_deref().unwrap();
                for (prepared, (name, status)) in prepared.iter().zip(&report.refs) {
                    if *status != RefStatus::Accepted {
                        continue;
                    }
                    let Some(scope_index) = prepared.scope else {
                        continue;
                    };
                    if let Some(source) = &sources[scope_index] {
                        let source_tx = source_transactions[scope_index].as_ref().unwrap();
                        let saved = (|| -> Result<()> {
                            if let Some(normalized) = &prepared.normalized {
                                source.record_normalized(source_tx, normalized)?;
                            }
                            source.retain_observations(source_tx, &prepared.update.new.into_iter().collect::<Vec<_>>())?;
                            source_tx.flush_mem_odb()
                        })();
                        if let Err(error) = saved { save_errors.push(format!("{name}: conversion generation: {error:#}")); }
                    }
                    if sources[scope_index].is_none() && let Some(raw) = prepared.update.new {
                        let retained = format!("{}observed/{raw}", crate::native_project::binding_ref_prefix(scopes[scope_index].0.binding_id()));
                        if let Err(error) = save_raw_ref(transaction, &retained, Some(raw)) {
                            save_errors.push(format!("{name}: direct publication object: {error:#}"));
                        }
                    }
                    if scopes[scope_index].0.whole() && scopes[scope_index].0.project.is_some() {
                        for (raw, canonical) in &prepared.publications {
                            if let Err(error) = crate::native_project::record_anchor(
                                transaction,
                                scopes[scope_index].0.binding_id(),
                                "published",
                                raw,
                                canonical,
                            )
                            .and_then(|()| transaction.flush_mem_odb())
                            {
                                save_errors
                                    .push(format!("{name}: native publication anchor: {error:#}"));
                            }
                        }
                    }
                    let destination =
                        std::str::from_utf8(name).expect("validated UTF-8 destination");
                    if let Err(error) = save_raw_ref(
                        source_transactions[scope_index].as_ref().unwrap_or(transaction),
                        &format!("{push_prefix}{destination}"),
                        prepared.update.new,
                    ) {
                        save_errors.push(format!("{name}: raw publication ref: {error:#}"));
                    }
                    {
                        let result = match prepared.update.new {
                            Some(id) => crate::remote_refs::record_observation(
                                transaction,
                                &push_endpoint,
                                destination,
                                id,
                            ),
                            None => crate::remote_refs::record_absence(
                                transaction,
                                &push_endpoint,
                                destination,
                            ),
                        };
                        if let Err(error) = result {
                            save_errors.push(format!("{name}: publication lease: {}", error.error));
                        }
                    }
                }
            }
            finish(&remote, repo, &targets, &canonical, report, save_errors)
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn prepare_scope(
    scope: &Session,
    scope_index: usize,
    updates: &[(usize, String)],
    canonical: &[GitRefUpdate],
    repo: &MutableRepo,
    git: &gix::Repository,
    transaction: &Transaction,
    push_endpoint: &str,
    preparation: &GitRemotePushOptions,
    source_store: Option<&crate::source_repo::SourceRepo>,
    destination_connection: Option<&jj_lib::project::ConnectionId>,
    observation: Option<&ConversionObservation>,
) -> Result<Vec<(usize, Prepared)>, CommandError> {
    let canonical_transaction = transaction;
    let source_transaction = source_store.map(|source| crate::interop::open_josh_transaction(source.path(), true)).transpose()?;
    let transaction = source_transaction.as_ref().unwrap_or(transaction);
    let source_git = source_store.map_or(git, |source| source.git());
    let filter = scope.filter();
    let source_endpoint = generation_endpoint(scope, git, observation)?;
    let has_new = updates
        .iter()
        .any(|(index, _)| canonical[*index].targets.after.is_some());
    let base_rule = preparation.base.as_ref().or_else(|| scope.binding.as_ref().and_then(|(_, record)| record.base.as_ref()));
    let base = if has_new {
        base_rule.map(|_| -> Result<gix::ObjectId, CommandError> {
            let raw = observation.and_then(|value| value.terms[0].raw.as_ref()).ok_or_else(|| user_error("Selected source base has no immutable raw observation"))?;
            let id = gix::ObjectId::from_hex(raw.as_bytes()).map_err(user_error)?;
            peel_commit(source_git, id)
        }).transpose()?
    } else { None };
    let known = if scope.whole() && scope.project.is_some() && has_new {
        scope.anchors(repo, canonical_transaction).await?
    } else { HashMap::new() };
    let destination_connection = destination_connection.ok_or_else(|| user_error("Converted publication requires an identified destination connection; explicitly adopt the remote first"))?;
    let mut prepared = Vec::with_capacity(updates.len());
    for (index, destination) in updates {
        let canonical = &canonical[*index];
        let expected = crate::remote_refs::observation(canonical_transaction, push_endpoint, destination)?;
        let mut publications = Vec::new();
        let mut resolved_base = None;
        let mut generation = None;
        let mut publication_generation = None;
        let annotation = if destination.starts_with("refs/tags/") {
            canonical
                .targets
                .after
                .map(|id| crate::remote_refs::CommitTag::read(git, id).map_err(user_error))
                .transpose()?
        } else {
            None
        };
        let canonical_commit = annotation.as_ref().map_or(canonical.targets.after, |tag| Some(tag.commit));
        let new = if let Some(canonical_oid) = canonical.targets.after {
            let canonical_oid = annotation.as_ref().map_or(canonical_oid, |tag| tag.commit);
            let head = repo
                .store()
                .get_commit_async(&CommitId::from_bytes(canonical_oid.as_bytes()))
                .await?;
            if let Some(project) = scope.project.as_ref().filter(|_| scope.whole()) {
                let (raw, deltas) =
                    crate::native_project::export_project(repo, &project.mount, &head, &known)
                        .await
                        .map_err(user_error)?;
                publications = deltas;
                Some(gix::ObjectId::try_from(raw.as_bytes()).map_err(user_error)?)
            } else {
                crate::interop::check_projectable_repo_history(repo, &head).await?;
                let filter = filter.expect("non-native transformed scope has a filter");
                let destination_raw = if base_rule.is_none() {
                    observation.and_then(|value| value.terms[0].raw.as_ref()).map(|raw| gix::ObjectId::from_hex(raw.as_bytes()).map_err(user_error)).transpose()?.map(|id| peel_commit(source_git, id)).transpose()?
                } else { None };
                let source = source_store.expect("filtered publication has a source store");
                let tips: Vec<_> = destination_raw.into_iter().chain(base).collect();
                let context = base.or(destination_raw);
                if context.is_none() {
                    // Forgetting canonical observations does not make an
                    // existing source or a populated destination an empty one.
                    let raw_prefix = scope.raw_prefix(git, &source_endpoint).map_err(user_error)?;
                    let mut retained = git.references().map_err(user_error)?
                        .prefixed(raw_prefix.as_str()).map_err(user_error)?
                        .next().transpose().map_err(user_error)?.is_some();
                    if !retained {
                        for reference in source.git().references().map_err(user_error)?.all().map_err(user_error)? {
                            let reference = reference.map_err(user_error)?;
                            if reference.name().as_bstr() != crate::source_repo::INITIALIZED_REF.as_bytes() {
                                retained = true;
                                break;
                            }
                        }
                    }
                    if retained || matches!(expected, Expected::At(_)) {
                        return Err(user_error("Filtered publication has retained source history or a populated destination but no witnessed source context; select an explicitly witnessed --base"));
                    }
                }
                let normalized = match (context, observation.and_then(|value| value.generation.as_ref())) {
                    (Some(raw), Some(input)) => source.witnessed_generation(transaction, raw, crate::source_repo::parse_generation(input).map_err(user_error)?.1).map_err(user_error)?,
                    _ => source.normalize(transaction, &tips).map_err(user_error)?,
                };
                resolved_base = context.map(|id| id.to_string());
                let filter = crate::projection_history::map_source_ids(filter, &normalized.pairs);
                let destination_raw = destination_raw.map(|id| normalized.tips[&id]);
                let base = base.map(|id| normalized.tips[&id]);
                crate::interop::check_raw_projectable_history(transaction, normalized.tips.values().copied())?;
                let projected = if let Some(project) = &scope.project {
                    let local = crate::projection_history::local_project_filter(Path::new(
                        project.mount.as_internal_file_string(),
                    ))
                    .map_err(user_error)?;
                    let isolated = josh_core::filter_commit(transaction, local, canonical_oid)
                        .map_err(user_error)?;
                    if isolated.is_null() {
                        return Err(user_error(format!(
                            "No content found at project mount {} to push",
                            project.mount.as_internal_file_string()
                        )));
                    }
                    isolated
                } else {
                    canonical_oid
                };
                let raw = if scope.project.is_some()
                    && preparation.base.is_none()
                    && !preparation.merge
                {
                    let context = base.or(destination_raw);
                    let original =
                        context.unwrap_or_else(|| gix::ObjectId::null(gix::hash::Kind::Sha1));
                    let old = match context {
                        Some(raw) => josh_core::filter_commit(transaction, filter, raw)
                            .map_err(user_error)?,
                        None => original,
                    };
                    josh_core::history::unapply_filter(
                        transaction,
                        filter,
                        original,
                        old,
                        projected,
                        josh_core::history::UnapplyOptions {
                            reparent_orphans: context,
                            prune_empty: true,
                            ..Default::default()
                        },
                    )
                    .map_err(user_error)?
                } else {
                    josh_cli::commands::push::prepare_projected_commit(
                        transaction,
                        filter,
                        projected,
                        destination_raw,
                        base,
                        preparation.merge,
                    )
                    .map_err(user_error)?
                    .unfiltered_oid
                };
                let publication = source.denormalize(transaction, raw, &normalized).map_err(user_error)?;
                let denormalized = *publication.tips.keys().next().expect("one publication tip");
                generation = Some(crate::source_repo::generation(&source_endpoint, raw));
                publication_generation = Some(publication);
                Some(denormalized)
            }
        } else {
            None
        };
        let new = match (new, annotation) {
            (Some(target), Some(tag)) => Some(
                tag.retarget(source_git, target, destination.strip_prefix("refs/tags/").unwrap())
                    .map_err(user_error)?,
            ),
            (target, _) => target,
        };
        prepared.push((
            *index,
            Prepared {
                update: Update {
                    name: destination.as_str().into(),
                    expected,
                    new,
                },
                publications,
                scope: Some(scope_index),
                evidence: Some(scope.evidence(destination_connection, push_endpoint, destination.clone(), vec![ConversionTerm {
                    canonical: canonical_commit.map(|id| CommitId::from_bytes(id.as_bytes())),
                    raw: new.map(|id| id.to_string()),
                }], resolved_base, generation)),
                normalized: publication_generation,
            },
        ));
    }
    if let Some(source) = source_store {
        transaction.flush_mem_odb().map_err(user_error)?;
        let tips: Vec<_> = prepared.iter().filter_map(|(_, update)| update.update.new).collect();
        source.retain_raw(transaction, &tips).map_err(user_error)?;
    }
    Ok(prepared)
}

fn peel_commit(git: &gix::Repository, id: gix::ObjectId) -> Result<gix::ObjectId, CommandError> {
    let object = git
        .find_object(id)
        .map_err(user_error)?
        .peel_tags_to_end()
        .map_err(user_error)?;
    if object.kind != gix::objs::Kind::Commit {
        return Err(user_error("A projection base must peel to a commit"));
    }
    Ok(object.id)
}

fn finish(
    remote: &RemoteName,
    repo: &mut MutableRepo,
    targets: &GitPushRefTargets,
    canonical: &[GitRefUpdate],
    report: transport::Outcome,
    mut save_errors: Vec<String>,
) -> Result<GitRemotePushOutcome, CommandError> {
    // Import independently confirmed results even when other refs or local saves
    // failed. JJ owns the final operation commit and receives both facts and errors.
    let stats = match jj_lib::git::import_push_results(
        repo,
        remote,
        targets,
        canonical,
        canonical_stats(canonical, &report),
    ) {
        Ok(stats) => stats,
        Err(error) => {
            save_errors.push(format!("canonical push state: {error}"));
            canonical_stats(canonical, &report)
        }
    };
    let error = if !save_errors.is_empty() {
        if let Some(error) = report.error {
            save_errors.push(format!("Transport also reported: {error:#}"));
        }
        Some(user_error_with_message(
            "Remote publication was confirmed, but some local publication state could not be saved; do not blindly retry",
            anyhow::anyhow!(save_errors.join("\n")),
        ))
    } else {
        report.error.map(|error| user_error_with_message(
            "Push transport failed; confirmed references were recorded, but indeterminate references require a fetch before retrying",
            error,
        ))
    };
    Ok(GitRemotePushOutcome { stats, error })
}
