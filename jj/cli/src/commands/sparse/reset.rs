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

use jj_lib::working_copy_patterns::WorkingCopyPatterns;
use tracing::instrument;

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;

/// Reset selection and mappings to include all files at their canonical paths
#[derive(clap::Args, Clone, Debug)]
pub struct SparseResetArgs {}

#[instrument(skip_all)]
pub async fn cmd_sparse_reset(
    ui: &mut Ui,
    command: &CommandHelper,
    _args: &SparseResetArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command.workspace_helper(ui).await?;
    workspace_command
        .update_sparse_patterns_with(ui, |_ui, _old_patterns| Ok(WorkingCopyPatterns::all()))
        .await
}
