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

use itertools::Itertools as _;
use jj_lib::fileset;
use jj_lib::fileset::FilesetDiagnostics;
use jj_lib::repo::Repo as _;
use jj_lib::working_copy_patterns::SparseRule;
use jj_lib::working_copy_patterns::WorkingCopyMapping;
use jj_lib::working_copy_patterns::WorkingCopyPathBuf;
use jj_lib::working_copy_patterns::WorkingCopyPatterns;
use tracing::instrument;

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::command_error::print_parse_diagnostics;
use crate::command_error::user_error;
use crate::description_util::TextEditor;
use crate::ui::Ui;

/// Edit the ordered selection rules and path mappings
///
/// Each selection line starts with + (include) or - (exclude), followed by a
/// fileset expression. Rules are applied in order, starting with no files.
/// Paths are canonical workspace-root coordinates. Mapping lines have the form
/// `map ["source","destination"]`; use `map-file` for a non-recursive mapping.
/// Empty text selects no files with identity mapping. Lines starting with JJ:
/// are ignored.
#[derive(clap::Args, Clone, Debug)]
pub struct SparseEditArgs {}

#[instrument(skip_all)]
pub async fn cmd_sparse_edit(
    ui: &mut Ui,
    command: &CommandHelper,
    _args: &SparseEditArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command.workspace_helper(ui).await?;
    let editor = workspace_command.text_editor()?;
    let desired = workspace_command
        .repo()
        .view()
        .get_wc_sparse_patterns(workspace_command.workspace_name());
    let initial = if let Some(desired) = desired.filter(|value| !value.is_resolved()) {
        // Deliberately invalid fileset syntax: the editor must explicitly choose
        // a resolution rather than silently selecting one side of the merge.
        let mut text = String::from("JJ: Resolve the sparse selection conflict below.\n");
        for side in desired.adds() {
            text.push_str("<<<<<<< sparse selection\n");
            if let Some(id) = side {
                let patterns = workspace_command
                    .repo()
                    .op_store()
                    .read_working_copy_patterns(id)
                    .await?;
                text.push_str(&super::format_patterns(&patterns));
            } else {
                text.push_str("JJ: Selection not recorded");
            }
            text.push('\n');
        }
        text.push_str(">>>>>>> end sparse selection conflict\n");
        text
    } else {
        let patterns = workspace_command
            .sparse_patterns()?
            .ok_or_else(|| user_error("Sparse selection is not recorded at this operation."))?;
        super::format_patterns(&patterns)
    };
    let content = edit_sparse(&editor, &initial)?;
    if desired.is_none_or(|value| value.is_resolved()) && content.trim() == initial.trim() {
        return Ok(());
    }
    let mut new_patterns = WorkingCopyPatterns::none();
    let mut diagnostics = FilesetDiagnostics::new();
    let root_converter = jj_lib::ui_path::RepoPathUiConverter::Fs {
        cwd: std::path::PathBuf::new(),
        base: std::path::PathBuf::new(),
    };
    let context = workspace_command.env().fileset_parse_context();
    let context = jj_lib::fileset::FilesetParseContext {
        path_converter: &root_converter,
        ..context
    };
    for (index, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(expression) = line.strip_prefix('+').or_else(|| line.strip_prefix('-')) {
            new_patterns.rules.push(SparseRule {
                include: line.starts_with('+'),
                expression: fileset::parse(&mut diagnostics, expression.trim(), &context)?.into(),
            });
        } else if let Some(mapping) = line
            .strip_prefix("map ")
            .or_else(|| line.strip_prefix("map-file "))
        {
            let [source, destination]: [String; 2] =
                serde_json::from_str(mapping).map_err(user_error)?;
            new_patterns.mappings.push(WorkingCopyMapping {
                source: super::map::parse_mapping_path(if source.is_empty() {
                    "."
                } else {
                    &source
                })?,
                destination: WorkingCopyPathBuf::from_repo_path(super::map::parse_mapping_path(
                    if destination.is_empty() {
                        "."
                    } else {
                        &destination
                    },
                )?),
                recursive: line.starts_with("map "),
            });
        } else {
            return Err(user_error(format!(
                "Invalid sparse rule on line {}: expected + FILESET, - FILESET, or map \
                 [\"source\",\"destination\"].",
                index + 1
            )));
        }
    }
    print_parse_diagnostics(ui, "In fileset expression", &diagnostics)?;
    workspace_command
        .update_sparse_patterns_with(ui, |_ui, _old_patterns| Ok(new_patterns))
        .await
}

fn edit_sparse(editor: &TextEditor, content: &str) -> Result<String, CommandError> {
    let content = editor
        .edit_str(content, Some(".jjsparse"))
        .map_err(|err| err.with_name("sparse patterns"))?;
    Ok(content
        .lines()
        .filter(|line| !line.starts_with("JJ:"))
        .join("\n"))
}
