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

use std::io::Write as _;

use jj_lib::repo::Repo as _;
use tracing::instrument;

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;

/// Show ordered selection rules and mappings recorded at this operation
///
/// Rule paths are relative to the canonical repository root; mapping
/// destinations are relative to the physical working-copy root.
#[derive(clap::Args, Clone, Debug)]
pub struct SparseListArgs {}

#[instrument(skip_all)]
pub async fn cmd_sparse_list(
    ui: &mut Ui,
    command: &CommandHelper,
    _args: &SparseListArgs,
) -> Result<(), CommandError> {
    let workspace_command = command.workspace_helper(ui).await?;
    let desired = workspace_command
        .repo()
        .view()
        .get_wc_sparse_patterns(workspace_command.workspace_name());
    if let Some(desired) = desired.filter(|value| !value.is_resolved()) {
        writeln!(ui.stdout(), "Conflicted sparse selection:")?;
        for (label, side) in desired
            .removes()
            .map(|side| ("base", side))
            .chain(desired.adds().map(|side| ("side", side)))
        {
            writeln!(ui.stdout(), "  {label}:")?;
            if let Some(id) = side {
                let patterns = workspace_command
                    .repo()
                    .op_store()
                    .read_working_copy_patterns(id)
                    .await?;
                write!(ui.stdout(), "{}", super::format_patterns(&patterns))?;
            } else {
                writeln!(ui.stdout(), "(not recorded)")?;
            }
        }
        if command.is_working_copy_writable() {
            writeln!(ui.stdout(), "Actual working-copy selection:")?;
            write!(
                ui.stdout(),
                "{}",
                super::format_patterns(workspace_command.working_copy().sparse_patterns()?)
            )?;
        }
    } else if let Some(patterns) = workspace_command.sparse_patterns()? {
        write!(ui.stdout(), "{}", super::format_patterns(&patterns))?;
    } else {
        writeln!(
            ui.stdout(),
            "Sparse selection is not recorded at this operation."
        )?;
    }
    Ok(())
}
