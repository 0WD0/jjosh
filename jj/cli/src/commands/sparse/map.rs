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

use std::io::Write as _;
use std::path::Path;

use jj_lib::repo_path::RepoPathBuf;
use jj_lib::working_copy_patterns::WorkingCopyMapping;
use jj_lib::working_copy_patterns::WorkingCopyPathBuf;

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::command_error::user_error;
use crate::ui::Ui;

/// Map canonical source paths to physical working-copy destinations
#[derive(clap::Subcommand, Clone, Debug)]
pub enum SparseMapCommand {
    /// Replace all mappings atomically; paths are workspace-root coordinates
    Set(SparseMapSetArgs),
    /// Show mappings recorded at this operation
    List,
    /// Restore identity mapping without changing selection rules
    Reset,
}

#[derive(clap::Args, Clone, Debug)]
pub struct SparseMapSetArgs {
    /// SOURCE=DEST mappings ('.' denotes root); directories map recursively
    #[arg(required = true, value_name = "SOURCE=DEST")]
    mappings: Vec<String>,
}

pub(super) fn parse_mapping_path(input: &str) -> Result<RepoPathBuf, CommandError> {
    if input.is_empty() || Path::new(input).is_absolute() {
        return Err(user_error(
            "Mapping paths must be explicit workspace-root coordinates; use '.' for root.",
        ));
    }
    let normalized = jj_lib::file_util::normalize_path(Path::new(input));
    RepoPathBuf::from_relative_path(normalized).map_err(user_error)
}

pub async fn cmd_sparse_map(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &SparseMapCommand,
) -> Result<(), CommandError> {
    let mut workspace_command = command.workspace_helper(ui).await?;
    if matches!(args, SparseMapCommand::List) {
        if workspace_command
            .repo()
            .view()
            .get_wc_sparse_patterns(workspace_command.workspace_name())
            .is_some_and(|value| !value.is_resolved())
        {
            return Err(user_error(
                "Sparse configuration is conflicted; use `jj sparse list` to inspect all sides.",
            ));
        }
        let patterns = workspace_command
            .sparse_patterns()?
            .ok_or_else(|| user_error("Sparse configuration is not recorded at this operation."))?;
        for mapping in &patterns.mappings {
            let source = mapping.source.as_internal_file_string();
            let destination = mapping.destination.as_repo_path().as_internal_file_string();
            writeln!(
                ui.stdout(),
                "{}={}{}",
                if source.is_empty() { "." } else { source },
                if destination.is_empty() {
                    "."
                } else {
                    destination
                },
                if mapping.recursive { "" } else { " (file)" }
            )?;
        }
        return Ok(());
    }
    if workspace_command
        .repo()
        .view()
        .get_wc_sparse_patterns(workspace_command.workspace_name())
        .is_some_and(|value| !value.is_resolved())
    {
        return Err(user_error(
            "Sparse configuration is conflicted; use `jj sparse edit` or `jj sparse reset` to \
             resolve the whole configuration.",
        ));
    }
    let mappings = match args {
        SparseMapCommand::Set(args) => args
            .mappings
            .iter()
            .map(|text| {
                let (source, destination) = text
                    .split_once('=')
                    .ok_or_else(|| user_error("Expected SOURCE=DEST; use '.' for root."))?;
                Ok(WorkingCopyMapping {
                    source: parse_mapping_path(source)?,
                    destination: WorkingCopyPathBuf::from_repo_path(parse_mapping_path(
                        destination,
                    )?),
                    recursive: true,
                })
            })
            .collect::<Result<Vec<_>, CommandError>>()?,
        SparseMapCommand::Reset => Vec::new(),
        SparseMapCommand::List => unreachable!(),
    };
    workspace_command
        .update_sparse_patterns_with(ui, |_ui, old| {
            let mut patterns = old.cloned().ok_or_else(|| {
                user_error("Sparse configuration is not recorded at this operation.")
            })?;
            patterns.mappings = mappings;
            Ok(patterns)
        })
        .await
}
