// Copyright 2026 The Jujutsu Authors
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

use jj_lib::local_state;
use jj_lib::local_state::RecoveryOutcome;

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;

/// Recover an interrupted local state change without network access
///
/// A durable commit decision determines whether to complete the change or restore
/// its saved local state. The selected operation does not determine recovery.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct UtilRecoverArgs {}

pub(crate) async fn cmd_util_recover(
    ui: &mut Ui,
    command: &CommandHelper,
    _args: &UtilRecoverArgs,
) -> Result<(), CommandError> {
    // Do not load or merge the selected operation before inspecting publication
    // evidence. Recovery must also work with --at-op selecting historical state.
    let workspace = command.load_workspace()?;
    let extra_paths: Vec<_> = command
        .config_env()
        .maybe_repo_config_path(ui)?
        .into_iter()
        .collect();
    match local_state::recover(workspace.repo_loader(), &extra_paths).await? {
        RecoveryOutcome::NoPending => writeln!(ui.status(), "No interrupted local state change."),
        RecoveryOutcome::RolledBack => {
            writeln!(ui.status(), "Rolled back interrupted local state change.")
        }
        RecoveryOutcome::Completed => {
            writeln!(ui.status(), "Completed interrupted local state change.")
        }
    }?;
    Ok(())
}
