//! Command scope over the canonical repository, selected after loading an operation.

use std::collections::{HashMap, HashSet};

use jj_lib::backend::{CommitId, TreeValue};
use jj_lib::commit::Commit;
use jj_lib::merge::Merge;
use jj_lib::project::ProjectId;
use jj_lib::ref_name::RefName;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPathBuf;

use crate::cli_util::CommandHelper;
use crate::command_error::{CommandError, user_error};

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

    pub fn contains_ref(&self, repo: &dyn Repo, name: &RefName) -> bool {
        jj_lib::revset::remote_ref_project(repo.view(), name)
            .is_ok_and(|id| id.as_ref() == Some(&self.id))
    }

    /// Select the commits carrying project history without creating transport objects.
    /// Equal single-parent subtrees collapse; distinct project merge parents survive.
    /// Native conflicts remain signed values, so querying a conflict never requires
    /// resolving it or converting it to a Git transport representation.
    pub async fn history(
        &self,
        repo: &dyn Repo,
        heads: Vec<CommitId>,
    ) -> Result<HashSet<CommitId>, CommandError> {
        type Entry = (Option<CommitId>, Merge<Option<TreeValue>>);
        enum Visit {
            Read(CommitId),
            Write(Commit),
        }
        let root = repo.store().root_commit_id().clone();
        let mut mapped: HashMap<CommitId, Entry> =
            HashMap::from([(root, (None, Merge::resolved(None)))]);
        let mut pending: Vec<_> = heads.into_iter().map(Visit::Read).collect();
        let mut retained = HashSet::new();
        while let Some(visit) = pending.pop() {
            match visit {
                Visit::Read(id) => {
                    if mapped.contains_key(&id) {
                        continue;
                    }
                    let commit = repo.store().get_commit_async(&id).await?;
                    let parents = commit.parent_ids().to_vec();
                    pending.push(Visit::Write(commit));
                    pending.extend(parents.into_iter().map(Visit::Read));
                }
                Visit::Write(commit) => {
                    let value = commit
                        .tree()
                        .path_value(&self.root)
                        .await?
                        .map(|value| match value {
                            Some(TreeValue::Tree(_)) => value.clone(),
                            _ => None,
                        })
                        .simplify();
                    let parents: HashSet<_> = commit
                        .parent_ids()
                        .iter()
                        .filter_map(|id| mapped[id].0.as_ref())
                        .collect();
                    let equivalent = if parents.len() <= 1 {
                        commit.parent_ids().iter().find(|id| {
                            (parents.is_empty()
                                || mapped[*id].0.as_ref().is_some_and(|p| parents.contains(p)))
                                && mapped[*id].1 == value
                        })
                    } else {
                        None
                    };
                    let representative = if let Some(parent) = equivalent {
                        mapped[parent].0.clone()
                    } else {
                        retained.insert(commit.id().clone());
                        Some(commit.id().clone())
                    };
                    mapped.insert(commit.id().clone(), (representative, value));
                }
            }
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
