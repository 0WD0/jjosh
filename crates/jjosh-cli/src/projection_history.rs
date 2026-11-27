use std::collections::HashMap;
use std::path::Path;

use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;
use jj_lib::backend::ChangeId;
use jj_lib::backend::CommitId;
use jj_lib::index::ResolvedChangeState;
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::MutableRepo;
use jj_lib::repo::Repo as _;
use josh_core::cache::Transaction;

/// Project content excludes historical access markers, regardless of their contents.
pub(crate) fn local_project_filter(path: &Path) -> anyhow::Result<josh_core::filter::Filter> {
    let path = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Project path is not valid UTF-8"))?
        .trim_matches('/');
    anyhow::ensure!(!path.is_empty(), "Project path cannot be empty");
    Ok(josh_core::filter::Filter::new()
        .subdir(path)
        .exclude(josh_core::filter::Filter::new().file(".link.josh"))
        .prefix(path))
}

/// Literal source revisions must follow the same boundary rewrite as the input graph.
pub(crate) fn map_source_ids(
    filter: josh_core::filter::Filter,
    pairs: &[(gix::ObjectId, gix::ObjectId)],
) -> josh_core::filter::Filter {
    use josh_core::filter::{Filter, Op, RevMatch, to_filter, to_op};
    if pairs.iter().all(|(raw, normalized)| raw == normalized) {
        return filter;
    }
    let ids: HashMap<_, _> = pairs
        .iter()
        .copied()
        .filter(|(raw, input)| raw != input)
        .collect();
    fn map_node(
        filter: Filter,
        ids: &HashMap<gix::ObjectId, gix::ObjectId>,
        memo: &mut HashMap<gix::ObjectId, Filter>,
    ) -> Filter {
        let key = filter.id();
        if let Some(mapped) = memo.get(&key) {
            return *mapped;
        }
        let map_id = |id: &mut gix::ObjectId| {
            if let Some(mapped) = ids.get(id) {
                *id = *mapped;
            }
        };
        let mut op = to_op(filter);
        match &mut op {
            Op::Rev(arms) => {
                for (matcher, child) in arms {
                    match matcher {
                        RevMatch::AncestorStrict(id)
                        | RevMatch::AncestorInclusive(id)
                        | RevMatch::Equal(id) => map_id(id),
                        RevMatch::Default => {}
                    }
                    *child = map_node(*child, ids, memo);
                }
            }
            Op::Unapply(id, child) => {
                map_id(id);
                *child = map_node(*child, ids, memo);
            }
            Op::Downstack(id) => map_id(id),
            Op::Meta(_, child)
            | Op::Starlark(_, child)
            | Op::TreeId(_, child)
            | Op::Exclude(child)
            | Op::Select(child)
            | Op::Pin(child) => {
                *child = map_node(*child, ids, memo);
            }
            Op::Compose(children) | Op::Chain(children) => {
                for child in children {
                    *child = map_node(*child, ids, memo);
                }
            }
            Op::Subtract(left, right) => {
                *left = map_node(*left, ids, memo);
                *right = map_node(*right, ids, memo);
            }
            _ => {}
        }
        let mapped = to_filter(op);
        memo.insert(key, mapped);
        mapped
    }
    map_node(filter, &ids, &mut HashMap::new())
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
        let local_filter = local_project_filter(path).map_err(user_error)?;
        for (id, state) in &targets.targets {
            if *state != ResolvedChangeState::Visible {
                continue;
            }
            let candidate = gix_hash::ObjectId::try_from(id.as_bytes()).map_err(user_error)?;
            let candidate = josh_core::filter_commit(transaction, local_filter, candidate)
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
    repo: &mut MutableRepo,
    transaction: &Transaction,
    path: &Path,
    filtered: gix_hash::ObjectId,
    matches: &mut ProjectedMatches,
    reuse_existing: bool,
) -> Result<CommitId, CommandError> {
    let mount = crate::native_project::parse_mount(
        path.to_str()
            .ok_or_else(|| user_error("Project mount must be UTF-8"))?,
    )
    .map_err(user_error)?;
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
                let (change_id, content, reused) = if reuse_existing {
                    find_existing_filtered_commit(repo, transaction, path, id, matches).await?
                } else {
                    let (change_id, content) = projected_info(transaction, id)?;
                    (change_id, content, None)
                };
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
                    if canonical_parents == original_parents {
                        crate::interop::commit_id_from_josh_oid(id)
                    } else {
                        // Reusing a local publication also inherits its other
                        // projects. A project-only tree would otherwise encode
                        // their deletion against the newly reused parent.
                        let original_id = crate::interop::commit_id_from_josh_oid(id);
                        let original = repo.store().get_commit_async(&original_id).await?;
                        let mut intended = original.store_commit().as_ref().clone();
                        intended.parents = canonical_parents
                            .into_iter()
                            .map(crate::interop::commit_id_from_josh_oid)
                            .collect();
                        intended.secure_sig = None;
                        let tree =
                            crate::native_project::inherit_other_projects(repo, &mount, &intended)
                                .await
                                .map_err(user_error)?;
                        intended.root_tree = tree.tree_ids().clone();
                        intended.conflict_labels = tree.labels().as_merge().clone();
                        let rewritten = repo.store().write_commit(intended, None).await?;
                        repo.index_commits(std::slice::from_ref(&rewritten)).await?;
                        rewritten.id().clone()
                    }
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
