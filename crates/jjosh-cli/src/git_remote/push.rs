use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use gix::bstr::{BString, ByteSlice};
use gix::remote::Direction;
use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_cli::git_remote::{GitRemotePushOptions, GitRemotePushOutcome, GitRemoteSession as _};
use jj_cli::ui::Ui;
use jj_lib::backend::CommitId;
use jj_lib::git::{GitPushOptions, GitPushRefTargets, GitPushStats, GitRefUpdate};
use jj_lib::object_id::ObjectId as _;
use jj_lib::ref_name::RefName;
use jj_lib::repo::{MutableRepo, Repo as _};
use josh_core::cache::{Expected as RefExpected, Transaction};

use super::Session;
use crate::git_transport::push::{self as transport, Expected, RefStatus, Update};

struct Prepared {
    update: Update,
    publications: Vec<(CommitId, CommitId)>,
}

fn source_destination(session: &Session, update: &GitRefUpdate) -> Result<String> {
    let qualified = update.qualified_name.as_str();
    let (prefix, local) = if let Some(local) = qualified.strip_prefix("refs/heads/") {
        ("refs/heads/", local)
    } else if let Some(local) = qualified.strip_prefix("refs/tags/") {
        ("refs/tags/", local)
    } else {
        bail!("Unsupported logical push reference {qualified}");
    };
    let source = session.source_name(RefName::new(local)).with_context(|| {
        format!("Reference {qualified} does not belong to the selected remote project")
    })?;
    let destination = format!("{prefix}{source}");
    gix::validate::reference::name(destination.as_bytes().as_bstr())?;
    Ok(destination)
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
    let filter = session.filter();
    if filter.is_none() && (preparation.base.is_some() || preparation.merge) {
        return Err(user_error(
            "--base and --merge require a Josh projection; they cannot be used for ordinary or native project pushes",
        ));
    }
    let canonical = jj_lib::git::prepare_push_refs(repo, &session.name, targets)?;
    let transformed = session.project.is_some() || filter.is_some();
    // Validate the complete logical mapping before exporting objects or connecting.
    let mut destinations = Vec::with_capacity(canonical.len());
    let mut unique = HashSet::with_capacity(canonical.len());
    for update in &canonical {
        let destination = source_destination(session, update).map_err(user_error)?;
        if transformed && destination.starts_with("refs/tags/") {
            return Err(user_error(
                "Transformed tag publication is unsupported: annotated and signed tags cannot be reverse-mapped safely",
            ));
        }
        if !unique.insert(destination.clone()) {
            return Err(user_error(format!(
                "Multiple logical references map to push destination {destination}"
            )));
        }
        destinations.push(destination);
    }

    let git = gix::open(&session.git_path).map_err(user_error)?;
    let transport_options = transport::Options {
        dry_run,
        atomic: false,
        push_options: options
            .remote_push_options
            .iter()
            .map(|value| BString::from(value.as_str()))
            .collect(),
    };
    if !transformed {
        // Ordinary Git keeps exact tag objects and JJ's exact Absent/At leases.
        // Its canonical mirrors already retain raw identities, including SHA-256.
        let updates: Vec<_> = canonical
            .iter()
            .zip(&destinations)
            .map(|(update, destination)| Update {
                name: BString::from(destination.as_str()),
                expected: update.targets.before.map_or(Expected::Absent, Expected::At),
                new: update.targets.after,
            })
            .collect();
        let remote = session.remote(&git, Direction::Push).map_err(user_error)?;
        let report =
            transport::push(&git, remote, &updates, &transport_options).map_err(user_error)?;
        return finish(
            session,
            repo,
            targets,
            &canonical,
            report,
            Vec::new(),
            dry_run,
        );
    }
    let push_endpoint = session
        .endpoint_url(&git, Direction::Push)
        .map_err(user_error)?;
    let push_prefix = session
        .raw_prefix(&git, &push_endpoint)
        .map_err(user_error)?;
    let transaction = crate::interop::open_josh_transaction(&session.git_path, dry_run)?;
    let fetch_prefix = if filter.is_some() {
        let endpoint = session
            .endpoint_url(&git, Direction::Fetch)
            .map_err(user_error)?;
        Some(session.raw_prefix(&git, &endpoint).map_err(user_error)?)
    } else {
        None
    };
    let base = if let Some(base) = &preparation.base {
        let source = if base.starts_with("refs/") {
            base.clone()
        } else {
            format!("refs/heads/{base}")
        };
        gix::validate::reference::name(source.as_bytes().as_bstr()).map_err(user_error)?;
        Some(transaction.resolve_ref(&format!("{}{source}", fetch_prefix.as_ref().unwrap()))
            .map_err(user_error)?
            .ok_or_else(|| user_error(format!("Source base {source} has not been fetched from this remote's fetch endpoint")))?)
    } else {
        None
    };
    if filter.is_some() {
        for update in &canonical {
            if let Some(id) = update.targets.after {
                let head = repo
                    .store()
                    .get_commit_async(&CommitId::from_bytes(id.as_bytes()))
                    .await?;
                crate::interop::check_projectable_repo_history(repo, &head).await?;
            }
        }
    }
    let has_new = canonical
        .iter()
        .any(|update| update.targets.after.is_some());
    let known = if let Some(project) = session
        .project
        .as_ref()
        .filter(|project| project.native && has_new)
    {
        crate::native_project::anchors(repo, &transaction, &project.name)
            .await
            .map_err(user_error)?
    } else {
        HashMap::new()
    };
    let mut prepared = Vec::with_capacity(canonical.len());
    for (canonical, destination) in canonical.iter().zip(&destinations) {
        // The receive-pack advertisement must never become rewrite authority.
        let expected = crate::link_refs::observation(&transaction, &push_endpoint, destination)?;
        let mut publications = Vec::new();
        let new = match canonical.targets.after {
            None => None, // Deletion does not inspect or transform commit history.
            Some(canonical_oid) => {
                let id = CommitId::from_bytes(canonical_oid.as_bytes());
                let head = repo.store().get_commit_async(&id).await?;
                if let Some(project) = session.project.as_ref().filter(|project| project.native) {
                    let (raw, deltas) =
                        crate::native_project::export_project(repo, &project.mount, &head, &known)
                            .await
                            .map_err(user_error)?;
                    publications = deltas;
                    Some(gix::ObjectId::try_from(raw.as_bytes()).map_err(user_error)?)
                } else {
                    let filter = filter.expect("non-native transformed remote has a filter");
                    let destination_raw = transaction
                        .resolve_ref(&format!("{}{destination}", fetch_prefix.as_ref().unwrap()))
                        .map_err(user_error)?;
                    crate::interop::check_raw_projectable_history(
                        &transaction,
                        destination_raw.into_iter().chain(base),
                    )?;
                    let projected = if let Some(project) = &session.project {
                        let local = crate::link_metadata::local_link_filter(Path::new(
                            project.mount.as_internal_file_string(),
                        ))
                        .map_err(user_error)?;
                        let isolated = josh_core::filter_commit(&transaction, local, canonical_oid)
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
                    let raw = if session.project.is_some()
                        && preparation.base.is_none()
                        && !preparation.merge
                    {
                        // Linked snapshot roots preserve fetched ancestry; unrelated and empty
                        // monorepo changes are pruned, exactly as in linked history export.
                        let original = destination_raw
                            .unwrap_or_else(|| gix::ObjectId::null(gix::hash::Kind::Sha1));
                        let old = match destination_raw {
                            Some(raw) => josh_core::filter_commit(&transaction, filter, raw)
                                .map_err(user_error)?,
                            None => original,
                        };
                        josh_core::history::unapply_filter(
                            &transaction,
                            filter,
                            original,
                            old,
                            projected,
                            josh_core::history::UnapplyOptions {
                                reparent_orphans: destination_raw,
                                prune_empty: true,
                                ..Default::default()
                            },
                        )
                        .map_err(user_error)?
                    } else {
                        josh_cli::commands::push::prepare_projected_commit(
                            &transaction,
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
            }
        };
        prepared.push(Prepared {
            update: Update {
                name: BString::from(destination.as_str()),
                expected,
                new,
            },
            publications,
        });
    }

    // Only objects have been prepared. Transaction drop is not a rollback mechanism:
    // no successful-publication refs, observations, or anchors may be staged yet.
    transaction.flush_mem_odb().map_err(user_error)?;
    let updates: Vec<_> = prepared
        .iter()
        .map(|prepared| prepared.update.clone())
        .collect();
    let remote = session.remote(&git, Direction::Push).map_err(user_error)?;
    let report = transport::push(&git, remote, &updates, &transport_options).map_err(user_error)?;
    if dry_run {
        return finish(session, repo, targets, &canonical, report, Vec::new(), true);
    }

    let mut save_errors = Vec::new();
    for (prepared, (name, status)) in prepared.iter().zip(&report.refs) {
        if *status != RefStatus::Accepted {
            continue;
        }
        if let Some(project) = session.project.as_ref().filter(|project| project.native) {
            for (raw, canonical) in &prepared.publications {
                if let Err(error) = crate::native_project::record_anchor(
                    &transaction,
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
        // Save the PUSH endpoint only. A fork publication must not rewrite the
        // FETCH endpoint's source context for subsequent reverse filtering.
        let destination = std::str::from_utf8(name).expect("validated UTF-8 source destination");
        if let Err(error) = save_raw_ref(
            &transaction,
            &format!("{push_prefix}{destination}"),
            prepared.update.new,
        ) {
            save_errors.push(format!("{name}: raw publication ref: {error:#}"));
        }
        if destination.starts_with("refs/heads/") {
            let result = match prepared.update.new {
                Some(id) => crate::link_refs::record_observation(
                    &transaction,
                    &push_endpoint,
                    destination,
                    id,
                ),
                None => crate::link_refs::record_absence(&transaction, &push_endpoint, destination),
            };
            if let Err(error) = result {
                save_errors.push(format!("{name}: publication lease: {}", error.error));
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
