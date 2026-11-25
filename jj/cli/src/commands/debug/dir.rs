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

use std::collections::BTreeSet;
use std::io::Write as _;

use jj_lib::backend::TreeValue;

use crate::cli_util::CommandHelper;
use crate::cli_util::RevisionArg;
use crate::command_error::CommandError;
use crate::ui::Ui;

/// List the entries directly in a directory of a revision
///
/// Each entry name is printed on its own line, directories with a trailing
/// slash. A directory that is a conflict lists the entries of all its sides.
#[derive(clap::Args, Clone, Debug)]
pub struct DebugDirArgs {
    #[arg(long, short = 'r', default_value = "@", value_name = "REVSET")]
    revision: RevisionArg,

    /// The directory, relative to the current directory
    #[arg(default_value = ".")]
    dir: String,
}

pub async fn cmd_debug_dir(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &DebugDirArgs,
) -> Result<(), CommandError> {
    let workspace_command = command.workspace_helper(ui).await?;
    let commit = workspace_command
        .resolve_single_rev(ui, &args.revision)
        .await?;
    let dir = workspace_command.parse_file_path(&args.dir)?;
    let store = commit.store();
    let mut entries = BTreeSet::new();
    for value in commit.tree().path_value(&dir).await?.iter().flatten() {
        let TreeValue::Tree(id) = value else {
            continue;
        };
        let tree = store.get_tree(dir.clone(), id).await?;
        for entry in tree.entries_non_recursive() {
            let is_dir = matches!(entry.value(), TreeValue::Tree(_));
            entries.insert((entry.name().as_internal_str().to_owned(), is_dir));
        }
    }
    for (name, is_dir) in entries {
        let slash = if is_dir { "/" } else { "" };
        writeln!(ui.stdout(), "{name}{slash}")?;
    }
    Ok(())
}
