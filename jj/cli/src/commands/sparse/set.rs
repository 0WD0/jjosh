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
use jj_lib::repo::Repo as _;
use jj_lib::working_copy_patterns::SparseRule;
use jj_lib::working_copy_patterns::WorkingCopyPatterns;
use tracing::instrument;

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::command_error::user_error;
use crate::ui::Ui;

/// Update ordered fileset rules selecting paths present in the working copy
///
/// Positional filesets replace the current selection. For example, to include
/// only `README.md` and the `lib/` directory, use `jj sparse set README.md lib`.
/// To exclude the `lib` directory, use `jj sparse set --remove lib`.
///
/// Ordinary input paths are physical paths relative to the current directory;
/// use root: filesets for canonical paths, including paths outside the mapping.
/// Selection changes preserve existing mappings. After replacing or
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
    if replacement.is_none()
        && workspace_command
            .repo()
            .view()
            .get_wc_sparse_patterns(workspace_command.workspace_name())
            .is_some_and(|target| !target.is_resolved())
    {
        return Err(user_error(
            "Sparse selection is conflicted; specify replacement filesets or --clear, or use `jj \
             sparse edit` or `jj sparse reset`.",
        ));
    }
    let mut conflict_mappings = None;
    if let Some(desired) = workspace_command
        .repo()
        .view()
        .get_wc_sparse_patterns(workspace_command.workspace_name())
        .filter(|desired| !desired.is_resolved())
    {
        for side in desired.adds() {
            let Some(id) = side else {
                return Err(user_error(
                    "Sparse mapping state is not recorded on one conflict side; use `jj sparse \
                     edit` or `jj sparse reset` to resolve the whole configuration.",
                ));
            };
            let patterns = workspace_command
                .repo()
                .op_store()
                .read_working_copy_patterns(id)
                .await?;
            if conflict_mappings
                .as_ref()
                .is_some_and(|mappings| mappings != &patterns.mappings)
            {
                return Err(user_error(
                    "Path mappings are conflicted; use `jj sparse edit` or `jj sparse reset` to \
                     resolve the whole configuration.",
                ));
            }
            conflict_mappings = Some(patterns.mappings);
        }
    }
    workspace_command
        .update_sparse_patterns_with(ui, |_ui, old_patterns| {
            let mut new_patterns = old_patterns
                .cloned()
                .unwrap_or_else(WorkingCopyPatterns::none);
            if let Some(mappings) = conflict_mappings {
                new_patterns.mappings = mappings;
            }
            if let Some(replacement) = replacement {
                new_patterns.rules = WorkingCopyPatterns::from(replacement).rules;
            } else if old_patterns.is_none() {
                return Err(user_error(
                    "No resolved sparse selection at this operation; specify replacement filesets \
                     or --clear.",
                ));
            }
            if let Some(added) = added {
                new_patterns.rules.push(SparseRule {
                    include: true,
                    expression: added.into(),
                });
            }
            if let Some(removed) = removed {
                new_patterns.rules.push(SparseRule {
                    include: false,
                    expression: removed.into(),
                });
            }
            Ok(new_patterns)
        })
        .await
}
