use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Result, bail};
use gix::bstr::{BString, ByteSlice};
use gix::remote::Direction;
use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_cli::git_remote::{GitRemotePushOptions, GitRemotePushOutcome};
use jj_cli::ui::Ui;
use jj_lib::backend::CommitId;
use jj_lib::git::{GitPushOptions, GitPushRefTargets, GitPushStats, GitRefUpdate};
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::{MutableRepo, Repo as _};
use josh_core::cache::{Expected as RefExpected, Transaction};

use super::Session;
use crate::git_transport::push::{self as transport, Expected, RefStatus, Update};

struct Prepared {
    update: Update,
    publications: Vec<(CommitId, CommitId)>,
    scope: Option<usize>,
}

fn source_destination(update: &GitRefUpdate) -> Result<(String, Option<&str>)> {
    let qualified = update.qualified_name.as_str();
    let (prefix, local) = if let Some(local) = qualified.strip_prefix("refs/heads/") {
        ("refs/heads/", local)
    } else if let Some(local) = qualified.strip_prefix("refs/tags/") {
        ("refs/tags/", local)
    } else {
        bail!("Unsupported logical push reference {qualified}");
    };
    let (source, scope) = local
        .rsplit_once('#')
        .map_or((local, None), |(source, scope)| (source, Some(scope)));
    let destination = format!("{prefix}{source}");
    gix::validate::reference::name(destination.as_bytes().as_bstr())?;
    Ok((destination, scope))
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

pub(super) async fn run(
    session: &Session,
    _ui: &mut Ui,
    _command: &CommandHelper,
    repo: &mut MutableRepo,
    targets: &GitPushRefTargets,
    options: &GitPushOptions,
    preparation: &GitRemotePushOptions,
    dry_run: bool,
) -> Result<GitRemotePushOutcome, CommandError> {
    let canonical = jj_lib::git::prepare_push_refs(repo, &session.name, targets)?;
    let git = jj_lib::git::get_git_backend(repo.store())?.git_repo();
    let mut scopes: Vec<(Session, Vec<(usize, String)>)> = Vec::new();
    let mut scope_indices = HashMap::new();
    let mut destinations = HashSet::with_capacity(canonical.len());
    for (index, update) in canonical.iter().enumerate() {
        let (destination, scope) = source_destination(update).map_err(user_error)?;
        if !destinations.insert(destination.clone()) {
            return Err(user_error(format!(
                "Multiple selected references map to push destination {destination}"
            )));
        }
        let scope_index = match scope_indices.get(&scope) {
            Some(index) => *index,
            None => {
                let conversion = if let Some(scope) = scope {
                    session.push_scope(&git, scope)?
                } else {
                    // Unscoped names do not inherit a destination's project.
                    // A standalone projection view still uses its own filter.
                    Session {
                        name: session.name.clone(),
                        git_path: session.git_path.clone(),
                        project: None,
                        josh: if session.project.is_none() {
                            josh_changes::remote_config::try_read_remote_config(
                                &session.git_path,
                                session.name.as_str(),
                            )
                            .map_err(user_error)?
                        } else {
                            None
                        },
                    }
                };
                let filter = conversion.filter();
                if filter.is_none() && (preparation.base.is_some() || preparation.merge) {
                    return Err(user_error(
                        "--base and --merge require a Josh projection; they cannot be used for ordinary or native project pushes",
                    ));
                }
                let index = scopes.len();
                scopes.push((conversion, Vec::new()));
                scope_indices.insert(scope, index);
                index
            }
        };
        let conversion = &scopes[scope_index].0;
        if (conversion.project.is_some() || conversion.filter().is_some())
            && destination.starts_with("refs/tags/")
        {
            return Err(user_error(
                "Transformed tag publication is unsupported: annotated and signed tags cannot be reverse-mapped safely",
            ));
        }
        scopes[scope_index].1.push((index, destination));
    }
    let transformed = scopes
        .iter()
        .any(|(scope, _)| scope.project.is_some() || scope.filter().is_some());
    let transaction = transformed
        .then(|| crate::interop::open_josh_transaction(&session.git_path, dry_run))
        .transpose()?;
    let push_endpoint = if transformed {
        Some(
            session
                .endpoint_url(&git, Direction::Push)
                .map_err(user_error)?,
        )
    } else {
        None
    };
    let mut prepared: Vec<Option<Prepared>> = (0..canonical.len()).map(|_| None).collect();
    // Resolve and prepare every selected scope before opening a mutation request.
    for (scope_index, (scope, updates)) in scopes.iter().enumerate() {
        if scope.project.is_none() && scope.filter().is_none() {
            for (index, destination) in updates {
                let update = &canonical[*index];
                prepared[*index] = Some(Prepared {
                    update: Update {
                        name: BString::from(destination.as_str()),
                        expected: update.targets.before.map_or(Expected::Absent, Expected::At),
                        new: update.targets.after,
                    },
                    publications: Vec::new(),
                    scope: None,
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
                push_endpoint.as_deref().unwrap(),
                preparation,
            )
            .await?
            {
                prepared[index] = Some(update);
            }
        }
    }
    let prepared: Vec<_> = prepared.into_iter().map(Option::unwrap).collect();
    if let Some(transaction) = &transaction {
        // Only prepared objects may be durable before publication, never success records.
        transaction.flush_mem_odb().map_err(user_error)?;
    }
    let updates: Vec<_> = prepared
        .iter()
        .map(|prepared| prepared.update.clone())
        .collect();
    let remote = session.remote(&git, Direction::Push).map_err(user_error)?;
    let report = transport::push(
        &git,
        remote,
        &updates,
        &transport::Options {
            dry_run,
            atomic: false,
            push_options: options
                .remote_push_options
                .iter()
                .map(|value| BString::from(value.as_str()))
                .collect(),
        },
    )
    .map_err(user_error)?;
    if dry_run {
        return finish(session, repo, targets, &canonical, report, Vec::new(), true);
    }

    let mut save_errors = Vec::new();
    if let Some(transaction) = &transaction {
        let push_endpoint = push_endpoint.as_deref().unwrap();
        let push_prefix = session
            .raw_prefix(&git, push_endpoint)
            .map_err(user_error)?;
        for (prepared, (name, status)) in prepared.iter().zip(&report.refs) {
            if *status != RefStatus::Accepted {
                continue;
            }
            let Some(scope_index) = prepared.scope else {
                continue;
            };
            if let Some(project) = scopes[scope_index]
                .0
                .project
                .as_ref()
                .filter(|project| project.native)
            {
                for (raw, canonical) in &prepared.publications {
                    if let Err(error) = crate::native_project::record_anchor(
                        transaction,
                        &project.name,
                        "published",
                        raw,
                        canonical,
                    )
                    .and_then(|()| transaction.flush_mem_odb())
                    {
                        save_errors.push(format!("{name}: native publication anchor: {error:#}"));
                    }
                }
            }
            let destination = std::str::from_utf8(name).expect("validated UTF-8 destination");
            if let Err(error) = save_raw_ref(
                transaction,
                &format!("{push_prefix}{destination}"),
                prepared.update.new,
            ) {
                save_errors.push(format!("{name}: raw publication ref: {error:#}"));
            }
            if destination.starts_with("refs/heads/") {
                let result = match prepared.update.new {
                    Some(id) => crate::link_refs::record_observation(
                        transaction,
                        push_endpoint,
                        destination,
                        id,
                    ),
                    None => {
                        crate::link_refs::record_absence(transaction, push_endpoint, destination)
                    }
                };
                if let Err(error) = result {
                    save_errors.push(format!("{name}: publication lease: {}", error.error));
                }
            }
        }
    }
    finish(
        session,
        repo,
        targets,
        &canonical,
        report,
        save_errors,
        false,
    )
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
) -> Result<Vec<(usize, Prepared)>, CommandError> {
    let filter = scope.filter();
    let fetch_prefix = if filter.is_some() {
        let endpoint = scope
            .endpoint_url(git, Direction::Fetch)
            .map_err(user_error)?;
        Some(scope.raw_prefix(git, &endpoint).map_err(user_error)?)
    } else {
        None
    };
    let has_new = updates
        .iter()
        .any(|(index, _)| canonical[*index].targets.after.is_some());
    let base = if let Some(base) = &preparation.base {
        let source = if base.starts_with("refs/") {
            base.clone()
        } else {
            format!("refs/heads/{base}")
        };
        gix::validate::reference::name(source.as_bytes().as_bstr()).map_err(user_error)?;
        Some(transaction.resolve_ref(&format!("{}{source}", fetch_prefix.as_ref().unwrap()))
            .map_err(user_error)?.ok_or_else(|| user_error(format!(
                "Source base {source} has not been fetched from this scope's source endpoint"
            )))?)
    } else {
        None
    };
    let linked_base = if let Some(project) = scope
        .project
        .as_ref()
        .filter(|project| !project.native && has_new)
    {
        let prefix = fetch_prefix.as_ref().expect("linked project has a filter");
        let configured =
            super::config_string(git, &format!("remote.{}.jjosh-base", scope.name.as_str()))
                .map_err(user_error)?;
        let observed = match configured {
            Some(source) => transaction
                .resolve_ref(&format!("{prefix}{source}"))
                .map_err(user_error)?,
            None => None,
        };
        match observed {
            Some(id) => Some(id),
            None => transaction
                .resolve_ref(&format!("{prefix}bases/{}", project.name))
                .map_err(user_error)?,
        }
    } else {
        None
    };
    let known = if let Some(project) = scope
        .project
        .as_ref()
        .filter(|project| project.native && has_new)
    {
        crate::native_project::anchors(repo, transaction, &project.name)
            .await
            .map_err(user_error)?
    } else {
        HashMap::new()
    };
    let mut prepared = Vec::with_capacity(updates.len());
    for (index, destination) in updates {
        let canonical = &canonical[*index];
        let expected = crate::link_refs::observation(transaction, push_endpoint, destination)?;
        let mut publications = Vec::new();
        let new = if let Some(canonical_oid) = canonical.targets.after {
            let head = repo
                .store()
                .get_commit_async(&CommitId::from_bytes(canonical_oid.as_bytes()))
                .await?;
            if let Some(project) = scope.project.as_ref().filter(|project| project.native) {
                let (raw, deltas) =
                    crate::native_project::export_project(repo, &project.mount, &head, &known)
                        .await
                        .map_err(user_error)?;
                publications = deltas;
                Some(gix::ObjectId::try_from(raw.as_bytes()).map_err(user_error)?)
            } else {
                crate::interop::check_projectable_repo_history(repo, &head).await?;
                let filter = filter.expect("non-native transformed scope has a filter");
                let destination_raw = transaction
                    .resolve_ref(&format!("{}{destination}", fetch_prefix.as_ref().unwrap()))
                    .map_err(user_error)?;
                crate::interop::check_raw_projectable_history(
                    transaction,
                    destination_raw.into_iter().chain(base).chain(linked_base),
                )?;
                let projected = if let Some(project) = &scope.project {
                    let local = crate::link_metadata::local_link_filter(Path::new(
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
                    let context = linked_base.or(destination_raw);
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
                Some(raw)
            }
        } else {
            None
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
            },
        ));
    }
    Ok(prepared)
}

fn finish(
    session: &Session,
    repo: &mut MutableRepo,
    targets: &GitPushRefTargets,
    canonical: &[GitRefUpdate],
    report: transport::Outcome,
    mut save_errors: Vec<String>,
    dry_run: bool,
) -> Result<GitRemotePushOutcome, CommandError> {
    if dry_run {
        let rejected: Vec<_> = report
            .refs
            .iter()
            .filter_map(|(name, status)| match status {
                RefStatus::Rejected(reason) => Some(format!("{name}: {reason}")),
                _ => None,
            })
            .collect();
        if !rejected.is_empty() {
            return Err(user_error(format!(
                "Push preflight rejected references:\n{}",
                rejected.join("\n")
            )));
        }
        if let Some(error) = report.error {
            return Err(user_error(error));
        }
        return Ok(GitRemotePushOutcome {
            stats: GitPushStats::default(),
            error: None,
        });
    }
    // Import independently confirmed results even when other refs or local saves
    // failed. JJ owns the final operation commit and receives both facts and errors.
    let stats = match jj_lib::git::import_push_results(
        repo,
        &session.name,
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
