use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;

use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;
use jj_cli::ui::Ui;
use jj_lib::backend::ChangeId;
use jj_lib::backend::CommitId;
use jj_lib::git::GitFetchRefExpression;
use jj_lib::git::GitRefKind;
use jj_lib::index::ResolvedChangeState;
use jj_lib::object_id::ObjectId as _;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::MutableRepo;
use jj_lib::repo::Repo as _;
use jj_lib::str_util::StringExpression;
use josh_core::cache::Expected;
use josh_core::cache::Transaction;
use josh_core::filter::Filter;

fn native_project_name(
    transaction: &Transaction,
    path: &str,
) -> Result<Option<String>, CommandError> {
    let mount = crate::native_project::parse_mount(path).map_err(user_error)?;
    crate::native_project::native_project_for_mount(transaction, &mount).map_err(user_error)
}

pub(crate) fn remote_name(project: &str, label: &str) -> Result<RemoteNameBuf, CommandError> {
    crate::ref_names::observation_remote(project, label)
        .map_err(user_error)
        .map(Into::into)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectedSignature {
    name: Vec<u8>,
    email: Vec<u8>,
    time: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectedContent {
    tree: gix_hash::ObjectId,
    author: ProjectedSignature,
    committer: ProjectedSignature,
    message: Vec<u8>,
}

// A source history can have many commits; only divergent versions of one change
// need a linear projected-content comparison.
pub(crate) type ProjectedMatches = HashMap<ChangeId, Vec<(ProjectedContent, CommitId)>>;

fn projected_info(
    transaction: &Transaction,
    oid: gix_hash::ObjectId,
) -> Result<(ChangeId, ProjectedContent), CommandError> {
    let commit =
        josh_core::objects::CommitData::read(transaction.odb(), oid).map_err(user_error)?;
    let parsed = gix_object::CommitRef::from_bytes(commit.bytes(), gix_hash::Kind::Sha1)
        .map_err(user_error)?;
    let author = parsed.author().map_err(user_error)?;
    let committer = parsed.committer().map_err(user_error)?;
    let change_id =
        jj_lib::git_backend::extract_change_id_from_commit(&parsed).unwrap_or_else(|| {
            jj_lib::git_backend::synthetic_change_id_from_git_commit_id(&CommitId::from_bytes(
                oid.as_bytes(),
            ))
        });
    Ok((
        change_id,
        ProjectedContent {
            tree: commit.tree_id().map_err(user_error)?,
            author: ProjectedSignature {
                name: author.name.to_owned().into(),
                email: author.email.to_owned().into(),
                time: author.time.to_owned(),
            },
            committer: ProjectedSignature {
                name: committer.name.to_owned().into(),
                email: committer.email.to_owned().into(),
                time: committer.time.to_owned(),
            },
            message: parsed.message.to_owned().into(),
        },
    ))
}

async fn find_existing_filtered_commit(
    repo: &MutableRepo,
    transaction: &Transaction,
    path: &Path,
    filtered: gix_hash::ObjectId,
    matches: &ProjectedMatches,
) -> Result<(ChangeId, ProjectedContent, Option<CommitId>), CommandError> {
    let (change_id, content) = projected_info(transaction, filtered)?;
    if let Some(previous) = matches.get(&change_id).and_then(|versions| {
        versions
            .iter()
            .find(|(previous, _)| previous == &content)
            .map(|(_, canonical)| canonical)
    }) {
        return Ok((change_id, content, Some(previous.clone())));
    }

    let mut canonical: Option<CommitId> = None;
    if let Some(targets) = repo
        .resolve_change_id(&change_id)
        .await
        .map_err(user_error)?
    {
        let local_filter = crate::link_metadata::local_link_filter(path).map_err(user_error)?;
        for (id, state) in &targets.targets {
            if *state != ResolvedChangeState::Visible {
                continue;
            }
            let candidate = gix_hash::ObjectId::try_from(id.as_bytes()).map_err(user_error)?;
            let candidate = josh_core::filter_commit(transaction, local_filter.clone(), candidate)
                .map_err(user_error)?;
            if candidate.is_null() {
                continue;
            }
            let (_, candidate_content) = projected_info(transaction, candidate)?;
            if candidate_content != content {
                continue;
            }
            if let Some(previous) = &canonical {
                return Err(user_error(format!(
                    "Change {} has multiple matching projected versions {} and {}",
                    change_id.hex(),
                    previous.hex(),
                    id.hex()
                )));
            }
            canonical = Some(id.clone());
        }
    }
    Ok((change_id, content, canonical))
}

enum ProjectedVisit {
    Read(gix_hash::ObjectId),
    Write(gix_hash::ObjectId),
}

pub(crate) async fn canonicalize_filtered_graph(
    repo: &MutableRepo,
    transaction: &Transaction,
    path: &Path,
    filtered: gix_hash::ObjectId,
    matches: &mut ProjectedMatches,
) -> Result<CommitId, CommandError> {
    let mut mapped: HashMap<gix_hash::ObjectId, CommitId> = HashMap::new();
    let mut pending = vec![ProjectedVisit::Read(filtered)];
    while let Some(visit) = pending.pop() {
        match visit {
            ProjectedVisit::Read(id) => {
                if mapped.contains_key(&id) {
                    continue;
                }
                let commit = josh_core::objects::CommitData::read(transaction.odb(), id)
                    .map_err(user_error)?;
                let parents: Vec<_> = commit.parent_ids().collect();
                pending.push(ProjectedVisit::Write(id));
                pending.extend(parents.into_iter().rev().map(ProjectedVisit::Read));
            }
            ProjectedVisit::Write(id) => {
                if mapped.contains_key(&id) {
                    continue;
                }
                let (change_id, content, reused) =
                    find_existing_filtered_commit(repo, transaction, path, id, matches).await?;
                let canonical = if let Some(reused) = reused {
                    reused
                } else {
                    let commit = josh_core::objects::CommitData::read(transaction.odb(), id)
                        .map_err(user_error)?;
                    let original_parents: Vec<_> = commit.parent_ids().collect();
                    let mut canonical_parents = Vec::with_capacity(original_parents.len());
                    for parent in &original_parents {
                        let mapped_parent = mapped.get(parent).ok_or_else(|| {
                            user_error(format!(
                                "Filtered parent {parent} was not canonicalized before {id}"
                            ))
                        })?;
                        canonical_parents.push(
                            gix_hash::ObjectId::try_from(mapped_parent.as_bytes())
                                .map_err(user_error)?,
                        );
                    }
                    let rewritten = if canonical_parents == original_parents {
                        id
                    } else {
                        josh_core::history::rewrite_commit(
                            transaction.odb(),
                            &commit,
                            &canonical_parents,
                            josh_core::filter::Rewrite::from_commit_data(&commit)
                                .map_err(user_error)?,
                            josh_core::history::GpgsigMode::Remove,
                        )
                        .map_err(user_error)?
                    };
                    crate::interop::commit_id_from_josh_oid(rewritten)
                };
                matches
                    .entry(change_id)
                    .or_default()
                    .push((content, canonical.clone()));
                mapped.insert(id, canonical);
            }
        }
    }
    mapped
        .remove(&filtered)
        .ok_or_else(|| user_error("Filtered commit was not canonicalized"))
}

