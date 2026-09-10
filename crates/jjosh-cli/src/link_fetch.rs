use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;

use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;
use jj_cli::ui::Ui;
use jj_lib::backend::CommitId;
use jj_lib::git::GitFetchRefExpression;
use jj_lib::git::GitRefKind;
use jj_lib::object_id::ObjectId as _;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::MutableRepo;
use jj_lib::repo::Repo as _;
use jj_lib::str_util::StringExpression;
use josh_core::cache::Expected;
use josh_core::cache::Transaction;
use josh_core::filter::Filter;

pub(crate) fn has_native_history(
    transaction: &Transaction,
    project: &str,
) -> Result<bool, CommandError> {
    let mut found = false;
    transaction
        .for_each_ref_prefixed(
            &crate::native_project::project_ref_prefix(project),
            |_, _| {
                found = true;
                Ok(())
            },
        )
        .map_err(user_error)?;
    Ok(found)
}

pub(crate) fn remote_name(path: &Path, label: &str) -> Result<RemoteNameBuf, CommandError> {
    crate::native_project::validate_project(label).map_err(user_error)?;
    let encoded = crate::link_refs::source_name(path)?;
    Ok(format!("{encoded}-{label}").into())
}

pub(crate) async fn fetch(
    ui: &mut Ui,
    command: &CommandHelper,
    repo: &mut MutableRepo,
    transaction: &Transaction,
    path: &Path,
    link: Filter,
    branches: Option<&[String]>,
    tags: Option<&[String]>,
    options: &jj_lib::git::GitImportOptions,
) -> Result<(), CommandError> {
    let project = path
        .to_str()
        .ok_or_else(|| user_error("Link path must be UTF-8"))?;
    let url = link
        .get_meta("remote")
        .ok_or_else(|| user_error("Link has no source URL"))?;
    let url = jj_cli::git_util::absolute_git_url(command.cwd(), &url)?;
    let label = link
        .get_meta("source-remote")
        .unwrap_or_else(|| "upstream".to_owned());
    let remote = remote_name(path, &label)?;
    let encoded = crate::link_refs::source_name(path)?;
    let scope = encoded.replace("%2F", "/");
    if jj_lib::git::get_git_repo(repo.store())?
        .remote_names()
        .iter()
        .any(|name| &name[..] == remote.as_str().as_bytes())
    {
        return Err(user_error(
            "Link observation name collides with a configured Git remote",
        ));
    }
    let native = has_native_history(transaction, project)?;
    if native && link.peel() != Filter::new().prefix(path) {
        return Err(user_error(
            "An imported whole project cannot change its source filter during fetch",
        ));
    }
    let known = if native {
        crate::native_project::anchors(repo, transaction, project)
            .await
            .map_err(user_error)?
    } else {
        HashMap::new()
    };

    // Advertise first so absent selected refs can be imported as deletions, and
    // an empty selection never falls back to fetching HEAD or default refspecs.
    let advertised = transaction
        .git_command(&["ls-remote", "--symref", "--", &url], &[])
        .map_err(user_error)?
        .with_stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(user_error)?;
    let advertised = std::str::from_utf8(&advertised.stdout).map_err(user_error)?;
    let target = link.get_meta("target").unwrap_or_else(|| "HEAD".to_owned());
    let specific = branches.is_some() || tags.is_some();
    let default_branch = if target == "HEAD" && !specific {
        advertised
            .lines()
            .find_map(|line| {
                line.strip_prefix("ref: refs/heads/")
                    .and_then(|line| line.strip_suffix("\tHEAD"))
            })
            .map(str::to_owned)
    } else {
        Some(
            target
                .strip_prefix("refs/heads/")
                .unwrap_or(&target)
                .to_owned(),
        )
    };
    let parse = |values: &[String]| jj_cli::revset_util::parse_union_name_patterns(ui, values);
    let bookmark = match branches {
        Some(values) => parse(values)?,
        None if specific => StringExpression::none(),
        None => default_branch
            .as_deref()
            .map(StringExpression::exact)
            .unwrap_or_else(StringExpression::none),
    };
    let tag = match tags {
        Some(values) => parse(values)?,
        None if specific => StringExpression::none(),
        None => StringExpression::all(),
    };
    // Share jj's supported ref-pattern grammar, including negative selections.
    jj_lib::git::expand_fetch_refspecs(
        &remote,
        GitFetchRefExpression {
            bookmark: bookmark.clone(),
            tag: tag.clone(),
        },
    )?;
    let bookmark_matcher = bookmark.to_matcher();
    let tag_matcher = tag.to_matcher();
    let selected = |kind, name: &str| match kind {
        GitRefKind::Bookmark => bookmark_matcher.is_match(name),
        GitRefKind::Tag => tag_matcher.is_match(name),
    };
    let key =
        josh_core::objects::write_blob(transaction.odb(), url.as_bytes()).map_err(user_error)?;
    let raw_prefix = format!("refs/jjosh/link-fetch/{encoded}/{key}/");
    let mut fetched = Vec::new();
    for line in advertised.lines() {
        let Some((_, reference)) = line.split_once('\t') else {
            continue;
        };
        let (kind, name) = if let Some(name) = reference.strip_prefix("refs/heads/") {
            (GitRefKind::Bookmark, name)
        } else if let Some(name) = reference.strip_prefix("refs/tags/") {
            if name.ends_with("^{}") {
                continue;
            }
            (GitRefKind::Tag, name)
        } else {
            continue;
        };
        if selected(kind, name) {
            fetched.push((
                kind,
                name.to_owned(),
                reference.to_owned(),
                format!("{raw_prefix}{reference}"),
            ));
        }
    }
    if !fetched.is_empty() {
        let refspecs: Vec<_> = fetched
            .iter()
            .map(|(_, _, source, dest)| format!("+{source}:{dest}"))
            .collect();
        let mut args = vec![
            "fetch",
            "--porcelain",
            "--no-tags",
            "--no-prune-tags",
            "--refmap=",
            "--",
            &url,
        ];
        args.extend(refspecs.iter().map(String::as_str));
        transaction
            .git_command(&args, &[])
            .map_err(user_error)?
            .with_stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(user_error)?;
    }
    let mut received = Vec::with_capacity(fetched.len());
    for (kind, name, source, raw_ref) in &fetched {
        let raw = transaction
            .resolve_ref(raw_ref)
            .map_err(user_error)?
            .ok_or_else(|| user_error("Fetched link reference disappeared"))?;
        let raw = josh_core::objects::peel_to_commit(transaction.odb(), raw).map_err(user_error)?;
        let raw = CommitId::from_bytes(raw.as_bytes());
        let new_boundary = !known.contains_key(&raw);
        received.push((*kind, name, source, raw, new_boundary));
    }
    let mut mapped = BTreeMap::new();
    let ids = if native && !received.is_empty() {
        let raw_ids: Vec<_> = received.iter().map(|(_, _, _, id, _)| id).collect();
        jj_lib::git::get_git_backend(repo.store())?.import_head_commits(raw_ids.iter().copied())?;
        let mut view = jj_lib::op_store::View::make_root(repo.store().root_commit_id().clone());
        view.head_ids.extend(raw_ids.into_iter().cloned());
        let source = crate::native_source::NativeSource::read_view(
            repo.store().clone(),
            repo.op_store().clone(),
            view,
            String::new(),
        )
        .await
        .map_err(user_error)?;
        crate::native_import::import_source(&source, repo, project, known)
            .await
            .map_err(user_error)?
            .ids
    } else {
        HashMap::new()
    };
    for (kind, name, _, raw, new_boundary) in &received {
        let canonical = if native {
            let canonical = ids[raw].clone();
            if *new_boundary {
                crate::native_project::record_anchor(
                    transaction,
                    project,
                    "origin",
                    raw,
                    &canonical,
                )
                .map_err(user_error)?;
            }
            canonical
        } else {
            let raw = gix_hash::ObjectId::try_from(raw.as_bytes()).map_err(user_error)?;
            let filtered =
                josh_core::filter_commit(transaction, link.peel(), raw).map_err(user_error)?;
            if filtered.is_null() {
                continue;
            }
            CommitId::from_bytes(filtered.as_bytes())
        };
        let prefix = match kind {
            GitRefKind::Bookmark => "refs/remotes/",
            GitRefKind::Tag => jj_lib::git::REMOTE_TAG_REF_NAMESPACE,
        };
        mapped.insert(
            format!("{prefix}{}/{scope}/{name}", remote.as_str()),
            canonical,
        );
    }
    for (kind, prefix) in [
        (GitRefKind::Bookmark, "refs/remotes/"),
        (GitRefKind::Tag, jj_lib::git::REMOTE_TAG_REF_NAMESPACE),
    ] {
        let prefix = format!("{prefix}{}/{scope}/", remote.as_str());
        transaction
            .for_each_ref_prefixed(&prefix, |name, old| {
                if selected(kind, &name[prefix.len()..]) && !mapped.contains_key(name) {
                    transaction.delete_ref(name, Expected::At(old))?;
                }
                Ok(())
            })
            .map_err(user_error)?;
    }
    for (name, id) in &mapped {
        let old = transaction.resolve_ref(name).map_err(user_error)?;
        transaction
            .update_ref(
                name,
                old.map_or(Expected::Absent, Expected::At),
                gix_hash::ObjectId::try_from(id.as_bytes()).map_err(user_error)?,
                "project fetched link reference",
            )
            .map_err(user_error)?;
    }
    // Only matching disappeared backing refs are pruned. Unselected branches
    // and tags remain intact on both sides of the projection boundary.
    let raw_names: HashSet<_> = fetched
        .iter()
        .map(|(_, _, _, name)| name.as_str())
        .collect();
    transaction
        .for_each_ref_prefixed(&raw_prefix, |name, old| {
            let suffix = &name[raw_prefix.len()..];
            let matches = suffix
                .strip_prefix("refs/heads/")
                .is_some_and(|n| selected(GitRefKind::Bookmark, n))
                || suffix
                    .strip_prefix("refs/tags/")
                    .is_some_and(|n| selected(GitRefKind::Tag, n));
            if matches && !raw_names.contains(name) {
                transaction.delete_ref(name, Expected::At(old))?;
            }
            Ok(())
        })
        .map_err(user_error)?;
    transaction.flush_mem_odb().map_err(user_error)?;
    let scope_prefix = format!("{scope}/");
    jj_lib::git::import_fetched_refs(repo, options, |kind, symbol| {
        symbol.remote == remote
            && symbol
                .name
                .as_str()
                .strip_prefix(&scope_prefix)
                .is_some_and(|name| selected(kind, name))
    })
    .await?;
    for (kind, _, source, raw, _) in received {
        if kind == GitRefKind::Bookmark {
            crate::link_refs::record_observation(
                transaction,
                &url,
                source,
                gix_hash::ObjectId::try_from(raw.as_bytes()).map_err(user_error)?,
            )?;
        }
    }
    transaction.flush_mem_odb().map_err(user_error)?;
    writeln!(
        ui.status(),
        "Fetched {} linked reference(s) for {project}; integrate with ordinary jj commands.",
        mapped.len()
    )?;
    Ok(())
}
