//! Content-based project summaries, interpreted through the command's operation View.
//!
//! This is a display property, not commit ownership or a publication route. It deliberately
//! does not consult bindings, reference labels, Git configuration, or remote observations.

use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::rc::Rc;

use futures::StreamExt as _;
use jj_cli::commit_templater::CommitTemplateBuildFnTable;
use jj_cli::commit_templater::CommitTemplateLanguageExtension;
use jj_cli::templater::TemplatePropertyExt as _;
use jj_lib::backend::BackendResult;
use jj_lib::backend::CommitId;
use jj_lib::commit::Commit;
use jj_lib::config::ConfigLayer;
use jj_lib::config::ConfigSource;
use jj_lib::extensions_map::ExtensionsMap;
use jj_lib::matchers::PrefixMatcher;
use jj_lib::object_id::ObjectId as _;
use jj_lib::project::ProjectState;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPathBuf;
use pollster::FutureExt as _;

pub(crate) struct Extension;

pub(crate) fn default_config() -> ConfigLayer {
    ConfigLayer::parse(ConfigSource::Default, include_str!("config/templates.toml"))
        .expect("valid jjosh default templates")
}

impl CommitTemplateLanguageExtension for Extension {
    fn build_fn_table<'repo>(&self) -> CommitTemplateBuildFnTable<'repo> {
        let mut table = CommitTemplateBuildFnTable::empty();
        table.commit_methods.insert(
            "touched_projects",
            |language, _diagnostics, _build_context, property, call| {
                call.expect_no_arguments()?;
                let repo = language.repo();
                let cache = language
                    .cache_extension::<Rc<TouchedProjectsCache>>()
                    .expect("project template cache is installed")
                    .clone();
                let out_property =
                    property.and_then(move |commit| Ok(cache.get(repo, &commit).block_on()?));
                Ok(out_property.into_dyn_wrapped())
            },
        );
        table
    }

    fn build_cache_extensions(&self, extensions: &mut ExtensionsMap) {
        extensions.insert(Rc::new(TouchedProjectsCache::default()));
    }
}

#[derive(Default)]
struct TouchedProjectsCache {
    // Each template environment belongs to one loaded View. Never share this cache across
    // commands: project rename/remove/restore can change the interpretation of the same commit.
    layout: OnceCell<Vec<ProjectScope>>,
    commits: RefCell<HashMap<CommitId, Vec<String>>>,
}

struct ProjectScope {
    display: String,
    matcher: PrefixMatcher,
}

impl TouchedProjectsCache {
    async fn get(&self, repo: &dyn Repo, commit: &Commit) -> BackendResult<Vec<String>> {
        if let Some(names) = self.commits.borrow().get(commit.id()) {
            return Ok(names.clone());
        }
        let layout = self
            .layout
            .get_or_init(|| project_scopes(repo.view().project_state()));
        let mut names = Vec::new();
        // With no registered projects, don't load parent trees at all. With projects, use the
        // same merged-parent baseline as `jj diff`, not a first-parent or ancestry union.
        if !layout.is_empty() {
            let before = commit.parent_tree(repo).await?;
            let after = commit.tree();
            if before.tree_ids() != after.tree_ids() {
                for project in layout {
                    // Native tree diff skips equal subtrees. One changed entry establishes a
                    // project hit; no text hunks, blob reads, or rename detection are needed.
                    if let Some(entry) = before.diff_stream(&after, &project.matcher).next().await {
                        entry.values?; // An unreadable tree is not an empty project diff.
                        names.push(project.display.clone());
                    }
                }
            }
        }
        names.sort();
        self.commits
            .borrow_mut()
            .insert(commit.id().clone(), names.clone());
        Ok(names)
    }
}

fn project_scopes(state: &ProjectState) -> Vec<ProjectScope> {
    struct Candidate {
        id: String,
        names: BTreeSet<String>,
        roots: BTreeSet<RepoPathBuf>,
        uncertain: bool,
    }

    let mut candidates: Vec<_> = state
        .projects
        .iter()
        .filter_map(|(id, target)| {
            let records: Vec<_> = target.adds().flatten().collect();
            if records.is_empty() {
                return None; // Deleted definitions and historical observations don't activate projects.
            }
            Some(Candidate {
                id: id.hex(),
                names: records.iter().map(|record| record.name.clone()).collect(),
                roots: records
                    .iter()
                    .map(|record| record.canonical_root.clone())
                    .collect(),
                uncertain: !target.is_resolved()
                    || records.iter().any(|record| {
                        record.canonical_root.is_root()
                            || record.name.is_empty()
                            || record.name.contains(['#', '/', '\\'])
                            || record.name.chars().any(char::is_whitespace)
                    }),
            })
        })
        .collect();

    // Only definition ambiguity matters to this display. A broken binding or disconnected
    // remote must not hide an otherwise unambiguous project. Conflicting definitions use
    // their positive candidate roots, and are labelled by ID instead of choosing a name.
    for i in 0..candidates.len() {
        for j in i + 1..candidates.len() {
            let a = &candidates[i];
            let b = &candidates[j];
            if !a.names.is_disjoint(&b.names) || !a.roots.is_disjoint(&b.roots) {
                candidates[i].uncertain = true;
                candidates[j].uncertain = true;
            }
        }
    }
    candidates
        .iter()
        .map(|candidate| {
            let display = if candidate.uncertain {
                let mut length = candidate.id.len().min(12);
                while length < candidate.id.len()
                    && candidates.iter().any(|other| {
                        other.id != candidate.id && other.id.starts_with(&candidate.id[..length])
                    })
                {
                    length += 1;
                }
                format!("project:{}?", &candidate.id[..length])
            } else {
                candidate.names.first().expect("present definition").clone()
            };
            ProjectScope {
                display,
                matcher: PrefixMatcher::new(&candidate.roots),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use jj_lib::matchers::Matcher as _;
    use jj_lib::merge::Merge;
    use jj_lib::project::ProjectId;
    use jj_lib::project::ProjectRecord;

    use super::*;

    fn record(name: &str, path: &str) -> Option<ProjectRecord> {
        Some(ProjectRecord {
            name: name.to_owned(),
            canonical_root: RepoPathBuf::from_internal_string(path).unwrap(),
        })
    }

    #[test]
    fn unresolved_scopes_use_only_positive_roots_and_do_not_reactivate_deleted_projects() {
        let mut state = ProjectState::default();
        state.projects.insert(
            ProjectId::new(vec![1; 16]),
            Merge::from_removes_adds(
                [record("old", "retired")],
                [record("left", "apps/left"), record("right", "apps/right")],
            ),
        );
        state
            .projects
            .insert(ProjectId::new(vec![2; 16]), Merge::resolved(None));
        let scopes = project_scopes(&state);
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].display, "project:010101010101?");
        for (path, expected) in [
            ("apps/left/file", true),
            ("apps/right/file", true),
            ("apps/leftover/file", false),
            ("retired/file", false),
        ] {
            let path = RepoPathBuf::from_internal_string(path).unwrap();
            assert_eq!(scopes[0].matcher.matches(&path), expected);
        }
    }

    #[test]
    fn identical_roots_and_duplicate_names_have_distinct_uncertain_ids() {
        let mut state = ProjectState::default();
        for (suffix, name, path) in [
            (1, "same", "apps/a"),
            (2, "same", "apps/b"),
            (3, "same-root", "apps/a"),
            (4, "healthy", "libs/c"),
        ] {
            let mut id = vec![0; 16];
            id[15] = suffix;
            state
                .projects
                .insert(ProjectId::new(id), Merge::resolved(record(name, path)));
        }
        let scopes = project_scopes(&state);
        let names: BTreeSet<_> = scopes.iter().map(|scope| scope.display.as_str()).collect();
        assert_eq!(names.len(), 4);
        assert!(names.contains("healthy"));
        for suffix in 1..=3 {
            assert!(names.contains(format!("project:{suffix:032x}?").as_str()));
        }
    }
}
