// Copyright 2020-2023 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::io;
use std::num::NonZeroU32;

use clap_complete::ArgValueCandidates;
use itertools::Itertools as _;
use jj_lib::git;
use jj_lib::git::GitFetch;
use jj_lib::git::GitFetchRefExpression;
use jj_lib::git::GitSettings;
use jj_lib::git::IgnoredRefspec;
use jj_lib::git::IgnoredRefspecs;
use jj_lib::git::expand_fetch_refspecs;
use jj_lib::git::get_git_backend;
use jj_lib::git::load_default_fetch_bookmarks;
use jj_lib::ref_name::RefName;
use jj_lib::ref_name::RemoteName;
use jj_lib::repo::Repo as _;
use jj_lib::str_util::StringExpression;

use crate::cli_util::CommandHelper;
use crate::cli_util::WorkspaceCommandTransaction;
use crate::command_error::CommandError;
use crate::command_error::cli_error;
use crate::command_error::user_error;
use crate::complete;
use crate::git_remote::GitRemoteFetchOptions;
use crate::git_remote::select_remote_names;
use crate::git_util::GitSubprocessUi;
use crate::git_util::load_git_import_options;
use crate::git_util::print_git_import_stats;
use crate::revset_util::parse_remote_fetch_bookmarks;
use crate::revset_util::parse_remote_fetch_tags;
use crate::revset_util::parse_union_name_patterns;
use crate::ui::Ui;

/// Fetch from a Git remote
///
/// Explicit remotes take precedence over the selected scope's configuration.
/// Project fetches use only `git.projects.<label>.fetch`, never `git.fetch`.
///
/// If no default is configured, uses the only candidate or the remote named
/// "origin". Project fetches consider only that project's bound remotes;
/// ordinary fetches consider only root remotes.
///
/// If no branches, tags, or revisions are specified, fetches bookmarks and tags
/// specified by the `remotes.<name>.fetch-bookmarks`/`fetch-tags` settings. If
/// `remotes.<name>.fetch-bookmarks` is not configured, the default fetch
/// refspecs for the selected remotes are read from the Git configuration.
///
/// Commits that are no longer reachable from any branch on the remote will be
/// considered abandoned by the remote, and will be abandoned in the local repo
/// to match the remote. Set `git.abandon-unreachable-commits` to `false` to
/// disable this behavior.
///
/// If a working-copy commit gets abandoned, it will be given a new, empty
/// commit. This is true in general; it is not specific to this command.
#[derive(clap::Args, Clone, Debug)]
#[command(group(clap::ArgGroup::new("specific").multiple(true)))]
#[command(group(clap::ArgGroup::new("history").multiple(false)))]
pub struct GitFetchArgs {
    /// Name of the branch to fetch (can be repeated)
    ///
    /// By default, the specified pattern matches branch names with glob syntax,
    /// but only `*` is expanded. Other wildcard characters such as `?` are
    /// *not* supported. Patterns can be repeated or combined with [logical
    /// operators] to specify multiple branches, but only union and negative
    /// intersection are supported.
    ///
    /// Examples: `push-*`, `(push-* | foo/*) ~ foo/unwanted`
    ///
    /// [logical operators]:
    ///     https://docs.jj-vcs.dev/latest/revsets/#string-patterns
    #[arg(long = "branch", short, alias = "bookmark", value_name = "BRANCH")]
    #[arg(add = ArgValueCandidates::new(complete::bookmark_names))]
    branches: Option<Vec<String>>,

    /// Fetch only some of the tags (can be repeated)
    ///
    /// By default, the specified pattern matches tag names with glob syntax,
    /// but only `*` is expanded. Other wildcard characters such as `?` are
    /// *not* supported. Patterns can be repeated or combined with [logical
    /// operators] to specify multiple tags, but only union and negative
    /// intersection are supported.
    ///
    /// [logical operators]:
    ///     https://docs.jj-vcs.dev/latest/revsets/#string-patterns
    #[arg(long = "tag", short, group = "specific", value_name = "TAG")]
    tags: Option<Vec<String>>,

    /// Fetch a full source commit ID (can be repeated)
    ///
    /// Requires exactly one matching named remote. Does not fetch default
    /// branches or tags unless they are explicitly selected.
    #[arg(long = "revision", group = "specific", value_name = "OID")]
    revisions: Vec<String>,

    /// Limit fetched source history to this many commits
    #[arg(long, group = "history", value_name = "N")]
    depth: Option<NonZeroU32>,

    /// Extend existing shallow source history by this many commits
    #[arg(long, group = "history", value_name = "N")]
    deepen: Option<NonZeroU32>,

    /// Fetch the complete history of a shallow source
    #[arg(long, group = "history")]
    unshallow: bool,

    /// Fetch from this URL once without changing the named remote
    ///
    /// Requires exactly one matching named remote.
    #[arg(long, value_name = "URL")]
    fetch_url: Option<String>,

    /// Fetch only tracked bookmarks and tags
    ///
    /// This fetches only bookmarks and tags that are already tracked from the
    /// specified remote(s).
    #[arg(long, conflicts_with = "specific")]
    tracked: bool,

    /// The remote to fetch from (only named remotes are supported, can be
    /// repeated)
    ///
    /// By default, the specified pattern matches remote names with glob syntax,
    /// e.g. `--remote '*'`. You can also use other [string pattern syntax].
    ///
    /// [string pattern syntax]:
    ///     https://docs.jj-vcs.dev/latest/revsets/#string-patterns
    #[arg(long = "remote", value_name = "REMOTE")]
    #[arg(add = ArgValueCandidates::new(complete::git_remotes))]
    remotes: Option<Vec<String>>,

    /// Fetch from this project's remotes (can be repeated)
    ///
    /// Each project uses git.projects.<label>.fetch after --remote. The label
    /// remains stable when the project's display name changes.
    #[arg(long, value_name = "NAME")]
    project: Vec<String>,

    /// Fetch from every registered project's remotes, excluding root remotes
    ///
    /// Resolves remote selections and defaults independently in each project.
    #[arg(long, conflicts_with = "project")]
    all_projects: bool,

    /// Fetch from all remotes
    #[arg(long, conflicts_with = "remotes")]
    all_remotes: bool,
}

#[tracing::instrument(skip_all)]
pub async fn cmd_git_fetch(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &GitFetchArgs,
) -> Result<(), CommandError> {
    if command.global_args().no_integrate_operation {
        // TODO: Remove this restriction by fetching remote refs in memory or
        // into a temporary namespace.
        return Err(cli_error("--no-integrate-operation is not respected"));
    }
    if !crate::git_remote::capabilities(command).contains(&"jjosh-v1")
        && (!args.revisions.is_empty()
            || args.deepen.is_some()
            || args.unshallow
            || !args.project.is_empty()
            || args.all_projects
            || args.fetch_url.is_some())
    {
        return Err(user_error(
            "--revision, --deepen, --unshallow, --fetch-url, --project, and --all-projects require capability jjosh-v1",
        ));
    }
    for revision in &args.revisions {
        gix::ObjectId::from_hex(revision.as_bytes())
            .map_err(|err| user_error(format!("Invalid source revision {revision:?}: {err}")))?;
    }
    let fetch_options = GitRemoteFetchOptions {
        revisions: args.revisions.clone(),
        depth: args.depth,
        deepen: args.deepen,
        unshallow: args.unshallow,
        fetch_url: args.fetch_url.clone(),
    };
    let mut workspace_command = command.workspace_helper(ui).await?;
    let all_remotes = git::get_all_remote_names(workspace_command.repo().store())?;
    let view = workspace_command.repo().view();
    let state = view.project_state();
    let projects = if args.all_projects {
        let mut projects = Vec::new();
        for (id, target) in &state.projects {
            if target.adds().flatten().next().is_none() {
                continue;
            }
            state.validate_project(id).map_err(user_error)?;
            projects.push(Some(id.clone()));
        }
        if projects.is_empty() {
            return Err(user_error("No registered projects to fetch from"));
        }
        projects
    } else if args.project.is_empty() {
        vec![None]
    } else {
        args.project.iter().map(|name| {
            state.project_by_name(name).map(|(id, _)| Some(id)).map_err(user_error)
        }).try_collect()?
    };
    let mut selected_remotes = Vec::new();
    let mut selected_projects = std::collections::HashSet::new();
    let mut seen_remotes = std::collections::HashSet::new();
    for project in projects {
        if !selected_projects.insert(project.clone()) {
            continue;
        }
        let remotes = select_remote_names(
            ui,
            view,
            workspace_command.settings(),
            &all_remotes,
            project.as_ref(),
            args.remotes.as_deref(),
            gix::remote::Direction::Fetch,
            args.all_remotes,
        )?;
        if remotes.is_empty() {
            return Err(user_error("No git remotes to fetch from in a selected scope"));
        }
        selected_remotes.extend(remotes.into_iter().filter(|remote| seen_remotes.insert(remote.clone())));
    }
    let matching_remotes: Vec<&RemoteName> =
        selected_remotes.iter().map(AsRef::as_ref).collect();
    if matching_remotes.is_empty() {
        return Err(user_error("No git remotes to fetch from"));
    }
    if (!args.revisions.is_empty() || args.fetch_url.is_some()) && matching_remotes.len() != 1 {
        return Err(user_error(
            "--revision and --fetch-url require exactly one matching named remote",
        ));
    }

    for remote in &matching_remotes {
        crate::git_remote::check_remote(command, &workspace_command, remote)?;
    }
    let mut remote_sessions = std::collections::HashMap::new();
    if let Some(extension) = command.git_remote_extension() {
        for remote in &matching_remotes {
            remote_sessions.insert(
                *remote,
                extension.open(command, &workspace_command, remote)?,
            );
        }
    }
    let git_lock = if remote_sessions.is_empty() {
        None
    } else {
        Some(workspace_command.lock_git_import_export()?)
    };

    let remote_settings = crate::revset_util::resolve_remote_settings(
        workspace_command.repo().view(),
        workspace_command.settings().remote_settings()?,
    )?;

    let is_specific =
        args.branches.is_some() || args.tags.is_some() || !args.revisions.is_empty();
    let common_bookmark_expr = match &args.branches {
        Some(texts) => Some(parse_union_name_patterns(ui, texts)?),
        None => is_specific.then(StringExpression::none),
    };
    let common_tag_expr = match &args.tags {
        Some(texts) => Some(parse_union_name_patterns(ui, texts)?),
        None => is_specific.then(StringExpression::none),
    };
    let mut expansions = Vec::with_capacity(matching_remotes.len());
    if args.tracked {
        for remote in &matching_remotes {
            let bookmark = StringExpression::union_all(
                workspace_command.repo()
                    .view()
                    .local_remote_bookmarks(remote)
                    .filter(|(_, targets)| targets.remote_ref.is_tracked())
                    .filter_map(|(name, _)| {
                        let source = remote_sessions
                            .get(remote)
                            .map_or(Some(name.as_str()), |session| session.source_name(name))?;
                        Some(StringExpression::exact(source))
                    })
                    .collect(),
            );
            let tag = StringExpression::union_all(
                workspace_command.repo()
                    .view()
                    .local_remote_tags(remote)
                    .filter(|(_, targets)| targets.remote_ref.is_tracked())
                    .filter_map(|(name, _)| {
                        let source = remote_sessions
                            .get(remote)
                            .map_or(Some(name.as_str()), |session| session.source_name(name))?;
                        Some(StringExpression::exact(source))
                    })
                    .collect(),
            );
            let ref_expr = GitFetchRefExpression { bookmark, tag };
            if !crate::git_remote::capabilities(command).contains(&"jjosh-v1") {
                git::check_raw_fetch_selection(workspace_command.repo().view(), remote, &ref_expr).map_err(user_error)?;
            }
            let expanded = expand_fetch_refspecs(remote, ref_expr)?;
            expansions.push((remote, expanded));
        }
    } else {
        let git_repo = get_git_backend(workspace_command.repo().store())?.git_repo();
        for remote in &matching_remotes {
            let bookmark = if let Some(expr) = &common_bookmark_expr {
                expr.clone()
            } else if let Some(expr) = parse_remote_fetch_bookmarks(ui, &remote_settings, remote)? {
                expr
            } else {
                let (ignored, expr) = if let Some(session) = remote_sessions.get(remote) {
                    session.default_fetch_bookmarks()?
                } else {
                    load_default_fetch_bookmarks(remote, &git_repo)?
                };
                warn_ignored_refspecs(ui, &workspace_command.repo().view().remote_qualified_name(remote), ignored)?;
                expr
            };
            let tag = if let Some(expr) = &common_tag_expr {
                expr.clone()
            } else if let Some(expr) = parse_remote_fetch_tags(ui, &remote_settings, remote)? {
                expr
            } else {
                StringExpression::all()
            };
            let ref_expr = GitFetchRefExpression { bookmark, tag };
            if !crate::git_remote::capabilities(command).contains(&"jjosh-v1") {
                git::check_raw_fetch_selection(workspace_command.repo().view(), remote, &ref_expr).map_err(user_error)?;
            }
            let expanded = expand_fetch_refspecs(remote, ref_expr)?;
            expansions.push((remote, expanded));
        }
    }

    let git_settings = GitSettings::from_settings(workspace_command.settings())?;
    let import_options = load_git_import_options(ui, &git_settings, &remote_settings)?;
    let base_repo = workspace_command.repo().clone();
    let mut tx = workspace_command.start_transaction();
    let import_stats = if remote_sessions.is_empty() {
        let mut git_fetch = GitFetch::new(
            tx.repo_mut(),
            git_settings.to_subprocess_options(),
            &import_options,
        )?;
        for (completed, (remote, expanded)) in expansions.into_iter().enumerate() {
            let mut callback = GitSubprocessUi::new(ui);
            git_fetch.fetch(remote, expanded, &mut callback, fetch_options.depth)
                .map_err(|error| fetch_failure_context(error.into(), base_repo.view(), &matching_remotes, completed))?;
        }
        git_fetch.import_refs().await?
    } else {
        let mut observations = Vec::new();
        let mut selections = Vec::with_capacity(expansions.len());
        for (completed, (remote, expanded)) in expansions.into_iter().enumerate() {
            let expr = expanded.into_expression();
            let bookmarks = expr.bookmark.to_matcher();
            let tags = expr.tag.to_matcher();
            let session = &remote_sessions[remote];
            observations.extend(
                session
                    .fetch(ui, command, tx.repo_mut(), expr, &fetch_options)
                    .await
                    .map_err(|error| fetch_failure_context(error, base_repo.view(), &matching_remotes, completed))?,
            );
            // An empty selected result still observes a configured peer. Keep
            // it addressable for explicit tracking and first publication.
            tx.repo_mut().ensure_remote(remote);
            selections.push((*remote, bookmarks, tags));
        }
        git::import_remote_observations(
            tx.repo_mut(),
            &import_options,
            observations,
            |kind, symbol| {
                selections.iter().any(|(remote, bookmarks, tags)| {
                    *remote == symbol.remote
                        && remote_sessions[remote]
                            .source_name(symbol.name)
                            .is_some_and(|name| match kind {
                                git::GitRefKind::Bookmark => bookmarks.is_match(name),
                                git::GitRefKind::Tag => tags.is_match(name),
                            })
                })
            },
        )
        .await?
    };
    print_git_import_stats(ui, &tx, &import_stats)?;

    if let Some(bookmark_expr) = &common_bookmark_expr {
        warn_if_branches_not_found(ui, &tx, bookmark_expr, &matching_remotes, &remote_sessions)?;
    }
    // TODO: warn_if_tags_not_found()
    let description = format!(
        "fetch from git remote(s) {}",
        matching_remotes.iter().map(|n| tx.repo().view().remote_qualified_name(n)).join(","),
    );
    if let Some(git_lock) = git_lock {
        tx.finish_with_git_import_export_lock(ui, description, &git_lock)
            .await?;
    } else {
        tx.finish(ui, description).await?;
    }
    Ok(())
}

fn fetch_failure_context(
    error: CommandError,
    view: &jj_lib::view::View,
    remotes: &[&RemoteName],
    completed: usize,
) -> CommandError {
    if remotes.len() <= 1 {
        return error;
    }
    let received = if completed == 0 {
        "none".to_owned()
    } else {
        remotes[..completed].iter().map(|remote| view.remote_qualified_name(remote)).join(", ")
    };
    error.hinted(format!(
        "Fetch failed at {}. Completed transfers: {received}. No fetch operation was committed; local caches and Git refs may have changed.",
        view.remote_qualified_name(remotes[completed]),
    ))
}


fn warn_if_branches_not_found(
    ui: &mut Ui,
    tx: &WorkspaceCommandTransaction,
    bookmark_expr: &StringExpression,
    remotes: &[&RemoteName],
    remote_sessions: &std::collections::HashMap<
        &RemoteName,
        Box<dyn crate::git_remote::GitRemoteSession>,
    >,
) -> io::Result<()> {
    let bookmark_matcher = bookmark_expr.to_matcher();
    let mut missing_branches = bookmark_expr
        .exact_strings()
        .filter(|name| bookmark_matcher.is_match(name)) // exclude negative patterns
        .map(RefName::new)
        .filter(|name| {
            remotes.iter().all(|&remote| {
                let local_name = remote_sessions
                    .get(remote)
                    .map(|session| session.local_name(name.as_str()));
                let symbol = local_name
                    .as_deref()
                    .unwrap_or(name)
                    .to_remote_symbol(remote);
                let view = tx.repo().view();
                let base_view = tx.base_repo().view();
                view.get_remote_bookmark(symbol).is_absent()
                    && base_view.get_remote_bookmark(symbol).is_absent()
            })
        })
        .peekable();
    if missing_branches.peek().is_none() {
        return Ok(());
    }
    writeln!(
        ui.warning_default(),
        "No matching branches found on any specified/configured remote: {}",
        missing_branches.map(|name| name.as_symbol()).join(", ")
    )
}

fn warn_ignored_refspecs(
    ui: &Ui,
    remote_name: &str,
    IgnoredRefspecs(ignored_refspecs): IgnoredRefspecs,
) -> Result<(), CommandError> {
    for IgnoredRefspec { refspec, reason } in ignored_refspecs {
        writeln!(
            ui.warning_default(),
            "Ignored refspec `{refspec}` from `{remote_name}`: {reason}",
        )?;
    }

    Ok(())
}
