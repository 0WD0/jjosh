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

mod edit;
mod list;
mod map;
mod reset;
mod set;

use clap::Subcommand;
use jj_lib::working_copy_patterns::WorkingCopyPatterns;
use tracing::instrument;

use self::edit::SparseEditArgs;
use self::edit::cmd_sparse_edit;
use self::list::SparseListArgs;
use self::list::cmd_sparse_list;
use self::reset::SparseResetArgs;
use self::reset::cmd_sparse_reset;
use self::set::SparseSetArgs;
use self::set::cmd_sparse_set;
use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;

/// Manage which paths from the working-copy commit are present in the working
/// copy
#[derive(Subcommand, Clone, Debug)]
pub(crate) enum SparseCommand {
    Edit(SparseEditArgs),
    List(SparseListArgs),
    #[command(subcommand)]
    Map(map::SparseMapCommand),
    Reset(SparseResetArgs),
    Set(SparseSetArgs),
}

#[instrument(skip_all)]
pub(crate) async fn cmd_sparse(
    ui: &mut Ui,
    command: &CommandHelper,
    subcommand: &SparseCommand,
) -> Result<(), CommandError> {
    match subcommand {
        SparseCommand::Edit(args) => cmd_sparse_edit(ui, command, args).await,
        SparseCommand::List(args) => cmd_sparse_list(ui, command, args).await,
        SparseCommand::Map(args) => map::cmd_sparse_map(ui, command, args).await,
        SparseCommand::Reset(args) => cmd_sparse_reset(ui, command, args).await,
        SparseCommand::Set(args) => cmd_sparse_set(ui, command, args).await,
    }
}

/// Round-trippable, root-coordinate representation of the complete configuration.
fn format_patterns(patterns: &WorkingCopyPatterns) -> String {
    let mut output = String::new();
    for rule in &patterns.rules {
        output.push_str(if rule.include { "+ " } else { "- " });
        output.push_str(&jj_lib::fileset::format_expression(
            &rule.expression.to_expression(),
        ));
        output.push('\n');
    }
    for mapping in &patterns.mappings {
        output.push_str(if mapping.recursive {
            "map "
        } else {
            "map-file "
        });
        output.push_str(
            &serde_json::to_string(&[
                mapping.source.as_internal_file_string(),
                mapping.destination.as_repo_path().as_internal_file_string(),
            ])
            .unwrap(),
        );
        output.push('\n');
    }
    output
}
