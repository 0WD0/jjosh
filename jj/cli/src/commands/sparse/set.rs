// Copyright 2020 The Jujutsu Authors
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

use jj_lib::fileset::FilesetExpression;
use tracing::instrument;

use super::update_sparse_patterns_with;
use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;

/// Update the fileset expression selecting paths present in the working copy
///
/// Positional filesets replace the current selection. For example, to include
/// only `README.md` and the `lib/` directory, use `jj sparse set README.md lib`.
/// To exclude the `lib` directory, use `jj sparse set --remove lib`.
///
/// All input filesets are relative to the current directory. After replacing or
/// clearing the selection, `--add` filesets are included, then `--remove`
/// filesets are excluded, regardless of argument order.
#[derive(clap::Args, Clone, Debug)]
pub struct SparseSetArgs {
    /// Filesets to replace the current selection with
    #[arg(value_name = "FILESETS", value_hint = clap::ValueHint::AnyPath)]
    paths: Vec<String>,

    /// Filesets to include in the working copy
    #[arg(long, value_name = "FILESETS", value_hint = clap::ValueHint::AnyPath)]
    add: Vec<String>,

    /// Filesets to exclude after applying --add
    #[arg(long, value_name = "FILESETS", value_hint = clap::ValueHint::AnyPath)]
    remove: Vec<String>,

    /// Include no files before applying --add and --remove
    #[arg(long, conflicts_with = "paths")]
    clear: bool,
}

#[instrument(skip_all)]
pub async fn cmd_sparse_set(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &SparseSetArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command.workspace_helper(ui).await?;
    // Parse before starting the mutation, and distinguish omitted arguments from
    // parse_file_patterns()'s default of matching everything.
    let replacement = if args.clear {
        Some(FilesetExpression::none())
    } else if args.paths.is_empty() {
        None
    } else {
        Some(workspace_command.parse_file_patterns(ui, &args.paths)?)
    };
    let added = if args.add.is_empty() {
        None
    } else {
        Some(workspace_command.parse_file_patterns(ui, &args.add)?)
    };
    let removed = if args.remove.is_empty() {
        None
    } else {
        Some(workspace_command.parse_file_patterns(ui, &args.remove)?)
    };
    update_sparse_patterns_with(ui, &mut workspace_command, |_ui, old_patterns| {
        let mut new_patterns = replacement.unwrap_or_else(|| old_patterns.clone());
        if let Some(added) = added {
            new_patterns = FilesetExpression::union_all(vec![new_patterns, added]);
        }
        if let Some(removed) = removed {
            new_patterns = new_patterns.difference(removed);
        }
        Ok(new_patterns)
    })
    .await
}
