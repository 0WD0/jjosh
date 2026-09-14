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

mod advance;
mod create;
mod delete;
mod forget;
mod list;
mod r#move;
mod rename;
mod set;
mod track;
mod untrack;

use std::io;

use itertools::Itertools as _;
use jj_lib::backend::CommitId;
use jj_lib::iter_util::fallible_any;
use jj_lib::op_store::RefTarget;
use jj_lib::op_store::RemoteRef;
use jj_lib::ref_name::RefName;
use jj_lib::ref_name::RemoteName;
use jj_lib::ref_name::RemoteRefSymbol;
use jj_lib::ref_name::RemoteRefSymbolBuf;
use jj_lib::repo::Repo;
use jj_lib::str_util::StringExpression;
use jj_lib::str_util::StringMatcher;
use jj_lib::view::View;

use self::advance::BookmarkAdvanceArgs;
use self::advance::cmd_bookmark_advance;
use self::create::BookmarkCreateArgs;
use self::create::cmd_bookmark_create;
use self::delete::BookmarkDeleteArgs;
use self::delete::cmd_bookmark_delete;
use self::forget::BookmarkForgetArgs;
use self::forget::cmd_bookmark_forget;
use self::list::BookmarkListArgs;
use self::list::cmd_bookmark_list;
use self::r#move::BookmarkMoveArgs;
use self::r#move::cmd_bookmark_move;
use self::rename::BookmarkRenameArgs;
use self::rename::cmd_bookmark_rename;
use self::set::BookmarkSetArgs;
use self::set::cmd_bookmark_set;
use self::track::BookmarkTrackArgs;
use self::track::cmd_bookmark_track;
use self::untrack::BookmarkUntrackArgs;
use self::untrack::cmd_bookmark_untrack;
use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;

// Unlike most other aliases, `b` is defined in the config and can be overridden
// by the user.

/// Manage bookmarks [default alias: b]
///
/// See [`jj help -k bookmarks`] for more information.
///
/// [`jj help -k bookmarks`]:
///     https://docs.jj-vcs.dev/latest/bookmarks
#[derive(clap::Subcommand, Clone, Debug)]
pub enum BookmarkCommand {
    #[command(visible_alias("a"))]
    Advance(BookmarkAdvanceArgs),
    #[command(visible_alias("c"))]
    Create(BookmarkCreateArgs),
    #[command(visible_alias("d"))]
    Delete(BookmarkDeleteArgs),
    #[command(visible_alias("f"))]
    Forget(BookmarkForgetArgs),
    #[command(visible_alias("l"))]
    List(BookmarkListArgs),
    #[command(visible_alias("m"))]
    Move(BookmarkMoveArgs),
    #[command(visible_alias("r"))]
    Rename(BookmarkRenameArgs),
    #[command(visible_alias("s"))]
    Set(BookmarkSetArgs),
    #[command(visible_alias("t"))]
    Track(BookmarkTrackArgs),
    Untrack(BookmarkUntrackArgs),
}

pub async fn cmd_bookmark(
    ui: &mut Ui,
    command: &CommandHelper,
    subcommand: &BookmarkCommand,
) -> Result<(), CommandError> {
    match subcommand {
        BookmarkCommand::Advance(args) => cmd_bookmark_advance(ui, command, args).await,
        BookmarkCommand::Create(args) => cmd_bookmark_create(ui, command, args).await,
        BookmarkCommand::Delete(args) => cmd_bookmark_delete(ui, command, args).await,
        BookmarkCommand::Forget(args) => cmd_bookmark_forget(ui, command, args).await,
        BookmarkCommand::List(args) => cmd_bookmark_list(ui, command, args).await,
        BookmarkCommand::Move(args) => cmd_bookmark_move(ui, command, args).await,
        BookmarkCommand::Rename(args) => cmd_bookmark_rename(ui, command, args).await,
        BookmarkCommand::Set(args) => cmd_bookmark_set(ui, command, args).await,
        BookmarkCommand::Track(args) => cmd_bookmark_track(ui, command, args).await,
        BookmarkCommand::Untrack(args) => cmd_bookmark_untrack(ui, command, args).await,
    }
}

fn resolve_trackable_remote_bookmarks<'a>(
    ui: &Ui,
    view: &'a View,
    symbols: &'a [RemoteRefSymbolBuf],
) -> Result<Vec<(RemoteRefSymbol<'a>, &'a RemoteRef)>, CommandError> {
    let mut trackable_refs = vec![];
    let mut unmatched_symbols = vec![];
    for symbol in symbols {
        let symbol = symbol.as_ref();
        let remote_ref = view.get_remote_bookmark(symbol);
        if remote_ref.is_present()
            || view.get_local_bookmark(symbol.name).is_present()
                && view.get_remote_view(symbol.remote).is_some()
        {
            trackable_refs.push((symbol, remote_ref));
        } else {
            unmatched_symbols.push(symbol);
        }
    }
    trackable_refs.sort_unstable_by_key(|(sym, _)| *sym);
    trackable_refs.dedup_by(|(sym1, _), (sym2, _)| sym1 == sym2);
    if !unmatched_symbols.is_empty() {
        writeln!(
            ui.warning_default(),
            "No matching remote bookmarks for names: {}",
            unmatched_symbols.iter().map(|symbol| view.remote_ref_symbol(*symbol)).join(", ")
        )?;
    }
    Ok(trackable_refs)
}

fn trackable_remote_bookmarks_matching<'a>(
    view: &'a View,
    bookmark_matcher: &StringMatcher,
    remote_matcher: &StringMatcher,
) -> Result<Vec<(RemoteRefSymbol<'a>, &'a RemoteRef)>, CommandError> {
    let mut matches = Vec::new();
    for (remote, remote_view) in view.remote_views() {
        if !remote_matcher.is_match(remote.as_str()) {
            continue;
        }
        for (name, remote_ref) in &remote_view.bookmarks {
            let symbol = name.to_remote_symbol(remote);
            if bookmark_matcher.is_match(name.as_str())
                && jj_lib::revset::remote_ref_is_visible(view, symbol).map_err(crate::command_error::user_error)?
            {
                matches.push((symbol, remote_ref));
            }
        }
        for (name, _) in view.local_bookmarks_matching(bookmark_matcher) {
            let symbol = name.to_remote_symbol(remote);
            if !remote_view.bookmarks.contains_key(name)
                && jj_lib::revset::remote_ref_matches_scope(view, symbol).map_err(crate::command_error::user_error)?
            {
                matches.push((symbol, RemoteRef::absent_ref()));
            }
        }
    }
    Ok(matches)
}

async fn is_fast_forward(
    repo: &dyn Repo,
    old_target: &RefTarget,
    new_target_id: &CommitId,
) -> Result<bool, CommandError> {
    if old_target.is_present() {
        // Strictly speaking, "all" old targets should be ancestors, but we allow
        // conflict resolution by setting bookmark to "any" of the old target
        // descendants.
        let found = fallible_any(old_target.added_ids(), async |old| {
            repo.index().is_ancestor(old, new_target_id).await
        })
        .await?;
        Ok(found)
    } else {
        Ok(true)
    }
}

/// Warns about exact patterns that don't match local bookmarks.
fn warn_unmatched_local_bookmarks(
    ui: &Ui,
    view: &View,
    name_expr: &StringExpression,
) -> io::Result<()> {
    let mut names = name_expr
        .exact_strings()
        .map(RefName::new)
        .filter(|name| view.get_local_bookmark(name).is_absent())
        .peekable();
    if names.peek().is_none() {
        return Ok(());
    }
    writeln!(
        ui.warning_default(),
        "No matching bookmarks for names: {}",
        names.map(|name| name.as_symbol()).join(", ")
    )
}

/// Warns about exact patterns that don't match local or remote bookmarks.
fn warn_unmatched_local_or_remote_bookmarks(
    ui: &Ui,
    view: &View,
    name_expr: &StringExpression,
) -> io::Result<()> {
    let mut names = name_expr
        .exact_strings()
        .map(RefName::new)
        .filter(|&name| {
            view.get_local_bookmark(name).is_absent()
                && view
                    .remote_views()
                    .all(|(_, remote_view)| !remote_view.bookmarks.contains_key(name))
        })
        .peekable();
    if names.peek().is_none() {
        return Ok(());
    }
    writeln!(
        ui.warning_default(),
        "No matching bookmarks for names: {}",
        names.map(|name| name.as_symbol()).join(", ")
    )
}

/// Warns about exact patterns that don't match remotes.
fn warn_unmatched_remotes(ui: &Ui, view: &View, name_expr: &StringExpression) -> io::Result<()> {
    let mut names = name_expr
        .exact_strings()
        .map(RemoteName::new)
        .filter(|name| !view.remote_views().any(|(remote, _)| view.remote_local_name(remote) == *name || view.remote_qualified_name(remote) == name.as_str() || view.remote_in_scope(remote, None).unwrap_or(false) && name.as_str().strip_suffix('#') == Some(view.remote_local_name(remote).as_str())))
        .peekable();
    if names.peek().is_none() {
        return Ok(());
    }
    writeln!(
        ui.warning_default(),
        "No matching remotes for names: {}",
        names.map(|name| name.as_symbol()).join(", ")
    )
}
