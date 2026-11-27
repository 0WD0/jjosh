//! Command scope over the canonical repository, selected after loading an operation.

use std::collections::{HashMap, HashSet};

use futures::TryStreamExt as _;
use jj_lib::backend::{CommitId, TreeValue};
use jj_lib::fileset::FilesetExpression;
use jj_lib::merge::Merge;
use jj_lib::project::ProjectId;
use jj_lib::ref_name::RefName;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::revset::{ResolvedRevsetExpression, RevsetFilterPredicate};

use crate::cli_util::CommandHelper;
use crate::command_error::{CommandError, user_error};

type Subtree = Merge<Option<TreeValue>>;

#[derive(Clone, Debug)]
pub struct ProjectContext {
    pub id: ProjectId,
    pub name: String,
    pub root: RepoPathBuf,
}

impl ProjectContext {
    pub fn resolve(repo: &dyn Repo, name: &str) -> Result<Self, CommandError> {
        let (id, record) = repo
            .view()
            .projects()
            .project_by_name(name)
            .map_err(user_error)?;
        Ok(Self {
            id,
            name: record.name.clone(),
            root: record.canonical_root.clone(),
        })
    }

    /// The project directory of one commit; anything but a directory is no project.
    async fn subtree(&self, repo: &dyn Repo, id: &CommitId) -> Result<Subtree, CommandError> {
        let commit = repo.store().get_commit_async(id).await?;
        Ok(commit
            .tree()
            .path_value(&self.root)
            .await?
            .map(|value| match value {
                Some(TreeValue::Tree(_)) => value.clone(),
                _ => None,
            })
            .simplify())
    }

    pub fn contains_ref(&self, repo: &dyn Repo, name: &RefName) -> bool {
        jj_lib::revset::remote_ref_project(repo.view(), name)
            .is_ok_and(|id| id.as_ref() == Some(&self.id))
    }

    /// Select the commits carrying project history without creating transport objects.
    /// Equal single-parent subtrees collapse; distinct project merge parents survive.
    /// Native conflicts remain signed values, so querying a conflict never requires
    /// resolving it or converting it to a Git transport representation.
    ///
    /// Every ancestor takes part, because a commit's project version is the one its
    /// parents carry. The index supplies the graph and the commits that change a
    /// file below the project root; trees are read only for those commits and for
    /// merges that join project history with history lacking the project.
    pub async fn history(
        &self,
        repo: &dyn Repo,
        heads: Vec<CommitId>,
    ) -> Result<HashSet<CommitId>, CommandError> {
        let store = repo.store();
        let ancestors = ResolvedRevsetExpression::commits(heads).ancestors();
        let changed: HashSet<CommitId> = ancestors
            .filtered(RevsetFilterPredicate::File(FilesetExpression::prefix_path(
                self.root.clone(),
            )))
            .evaluate(repo)?
            .stream()
            .try_collect()
            .await?;
        let graph: Vec<_> = ancestors
            .evaluate(repo)?
            .stream_graph()
            .try_collect()
            .await?;

        // The commit whose project subtree a commit carries, if it has one. A
        // commit without one never had the project anywhere below it.
        let mut representatives: HashMap<CommitId, Option<CommitId>> =
            HashMap::with_capacity(graph.len());
        representatives.insert(store.root_commit_id().clone(), None);
        let mut subtrees: HashMap<CommitId, Subtree> = HashMap::new();
        let mut retained = HashSet::new();
        for (id, edges) in graph.into_iter().rev() {
            if representatives.contains_key(&id) {
                continue;
            }
            let mut carried: Vec<&Option<CommitId>> = edges
                .iter()
                .map(|edge| &representatives[&edge.target])
                .collect();
            let uniform = carried.iter().all(|parent| *parent == carried[0]);
            carried.retain(|parent| parent.is_some());
            carried.sort_unstable();
            carried.dedup();
            let representative = match carried.as_slice() {
                [] | [_] => {
                    let version = carried.first().copied().cloned().flatten();
                    // An unchanged commit carries the merge of its parents'
                    // subtrees, which is the common one when they all agree.
                    let unchanged = if uniform && !changed.contains(&id) {
                        true
                    } else {
                        let expected = match &version {
                            Some(version) => match subtrees.get(version) {
                                Some(subtree) => subtree.clone(),
                                None => {
                                    let subtree = self.subtree(repo, version).await?;
                                    subtrees.insert(version.clone(), subtree.clone());
                                    subtree
                                }
                            },
                            None => Merge::resolved(None),
                        };
                        self.subtree(repo, &id).await? == expected
                    };
                    if unchanged { version } else { Some(id.clone()) }
                }
                _ => Some(id.clone()),
            };
            if representative.as_ref() == Some(&id) {
                retained.insert(id.clone());
            }
            representatives.insert(id, representative);
        }
        // Empty changes still matter when explicitly named or checked out.
        retained.extend(repo.view().wc_commit_ids().values().cloned());
        for (name, target) in repo
            .view()
            .local_bookmarks()
            .chain(repo.view().local_tags())
        {
            if self.contains_ref(repo, name) {
                retained.extend(target.present_adds().cloned());
            }
        }
        for (symbol, remote) in repo
            .view()
            .all_remote_bookmarks()
            .chain(repo.view().all_remote_tags())
        {
            if self.contains_ref(repo, symbol.name) {
                retained.extend(remote.target.present_adds().cloned());
            }
        }
        Ok(retained)
    }
}

/// Reject unsupported commands before loading/snapshotting the working copy.
pub(crate) fn check_command(command: &CommandHelper) -> Result<(), CommandError> {
    if command.global_args().in_project.is_none() {
        return Ok(());
    }
    #[cfg(feature = "git")]
    if !crate::git_remote::capabilities(command).contains(&"jjosh-v1") {
        return Err(user_error("--in-project requires capability jjosh-v1"));
    }
    let mut path = Vec::new();
    let mut matches = command.matches();
    while let Some((name, sub)) = matches.subcommand() {
        path.push(name);
        matches = sub;
    }
    let supported = matches!(
        path.as_slice(),
        ["log" | "diff" | "show" | "status" | "help" | "version"]
            | ["file", "list" | "show" | "annotate"]
            | ["bookmark", "create" | "set" | "list"]
            | ["git", "fetch" | "push"]
            | ["git", "remote", "list"]
    );
    if path.as_slice() == ["git", "fetch"] {
        if matches.get_flag("all_projects")
            || matches
                .get_many::<String>("project")
                .is_some_and(|mut names| {
                    names.any(|name| Some(name) != command.global_args().in_project.as_ref())
                })
        {
            return Err(user_error(
                "--project/--all-projects conflicts with --in-project",
            ));
        }
    }
    if path.as_slice() == ["git", "remote", "list"]
        && matches
            .get_one::<String>("project")
            .is_some_and(|name| Some(name) != command.global_args().in_project.as_ref())
    {
        return Err(user_error("--project conflicts with --in-project"));
    }
    if path.as_slice() == ["git", "push"]
        && ["source", "change", "named"]
            .iter()
            .any(|name| matches.contains_id(name))
    {
        return Err(user_error(
            "--source, --change and --named do not yet support --in-project; select project bookmarks with --bookmark",
        ));
    }
    if !supported {
        return Err(user_error(format!(
            "{} does not yet support --in-project",
            path.join(" ")
        ))
        .hinted("Run this command without --in-project to operate on the complete repository."));
    }
    Ok(())
}
