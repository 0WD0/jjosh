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

//! Read-only remote identity projection, independent of live topology validation.
//!
//! Current membership shadows history, including deleted/conflicted entries.
//! Historical names are read only from historical records; they do not acquire
//! today's logical alias or activate an old project. Importers can inspect this
//! projection before applying their own legacy adoption and mount policies.

use std::collections::BTreeMap;

use crate::merge::Merge;
use crate::op_store;
use crate::project::BindingTarget;
use crate::project::ConnectionId;
use crate::project::ProjectId;
use crate::project::ScopedRemoteName;
use crate::ref_name::RemoteName;

/// The authority that supplied a remote identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentitySource {
    /// Current logical connection membership.
    Logical,
    /// Retained observations, not an active connection.
    Historical,
}

/// Resolved identity without assuming an active project, binding, or endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteIdentity<'a> {
    /// Current membership or retained history, never inferred from a name.
    pub source: IdentitySource,
    /// The explicitly recorded owner of the physical key.
    pub connection: &'a ConnectionId,
    /// Project-local name, or `None` for a root/legacy unnamed connection.
    pub scoped_name: Option<&'a ScopedRemoteName>,
}

impl RemoteIdentity<'_> {
    /// Display a recorded alias with its stable project label when available.
    /// Missing labels retain the physical key, not a guessed scope.
    pub fn qualified_name(
        &self,
        state: &crate::project::ProjectState,
        physical: &RemoteName,
    ) -> String {
        let Some(name) = self.scoped_name else {
            return physical.as_str().to_owned();
        };
        if let Some((label, _)) = state.labels.iter().find(|(_, target)| {
            target.as_resolved().and_then(Option::as_ref) == Some(&name.project)
        }) {
            format!("{}#{label}", name.name.as_str())
        } else {
            physical.as_str().to_owned()
        }
    }
}

struct IdentityRecords<'a> {
    source: IdentitySource,
    owners: &'a Merge<Option<ConnectionId>>,
    names: &'a BTreeMap<ConnectionId, Merge<Option<ScopedRemoteName>>>,
}

fn records<'a>(view: &'a op_store::View, remote: &RemoteName) -> Option<IdentityRecords<'a>> {
    if let Some(owners) = view.remote_connections.get(remote) {
        Some(IdentityRecords {
            source: IdentitySource::Logical,
            owners,
            names: &view.project_state.remote_names,
        })
    } else {
        view.observed_remote_connections
            .get(remote)
            .map(|owners| IdentityRecords {
                source: IdentitySource::Historical,
                owners,
                names: &view.observed_remote_names,
            })
    }
}

/// Resolve recorded ownership and its corresponding name without validating
/// whether that project's definition or conversion binding is currently active.
/// No metadata is synthesized. `None` means no recorded owner, not a deleted one.
pub fn resolve<'a>(
    view: &'a op_store::View,
    remote: &RemoteName,
) -> Result<Option<RemoteIdentity<'a>>, String> {
    let Some(records) = records(view, remote) else {
        return Ok(None);
    };
    let owner = records.owners.as_resolved().ok_or_else(|| {
        format!(
            "Remote {} has unresolved connection ownership",
            remote.as_str()
        )
    })?;
    let connection = owner.as_ref().ok_or_else(|| {
        format!(
            "Remote {} has deleted connection ownership",
            remote.as_str()
        )
    })?;
    let scoped_name = records
        .names
        .get(connection)
        .map(|names| {
            names.as_resolved().and_then(Option::as_ref).ok_or_else(|| {
                let kind = match records.source {
                    IdentitySource::Logical => "scoped remote",
                    IdentitySource::Historical => "historical",
                };
                format!("Connection {connection} has an unresolved or deleted {kind} name")
            })
        })
        .transpose()?;
    Ok(Some(RemoteIdentity {
        source: records.source,
        connection,
        scoped_name,
    }))
}

/// Inspect signed candidates before resolving them, so broken metadata in an
/// unrelated scope does not prevent operations on a healthy scope.
pub(super) fn may_be_in_scope(
    view: &op_store::View,
    remote: &RemoteName,
    project: Option<&ProjectId>,
) -> bool {
    let Some(records) = records(view, remote) else {
        return project.is_none();
    };
    let mut scoped = false;
    let mut matching = false;
    for owner in records.owners.adds().flatten() {
        if let Some(names) = records.names.get(owner) {
            scoped = true;
            matching |= names
                .iter()
                .flatten()
                .any(|identity| Some(&identity.project) == project);
            matching |= records.source == IdentitySource::Logical
                && names.iter().flatten().next().is_none();
        }
        if records.source == IdentitySource::Historical {
            continue;
        }
        for binding in view
            .project_state
            .bindings
            .values()
            .flat_map(|target| target.adds().flatten())
            .filter(|binding| &binding.connection_id == owner)
        {
            if let BindingTarget::Project(id) = &binding.target {
                scoped = true;
                matching |= Some(id) == project;
            }
        }
    }
    matching || (!scoped && project.is_none())
}

#[cfg(test)]
mod tests {
    use crate::backend::CommitId;

    use super::*;

    #[test]
    fn historical_names_do_not_borrow_current_aliases_or_require_live_projects() {
        let mut view = op_store::View::make_root(CommitId::from_hex("00"));
        let connection = ConnectionId::generate();
        let project = ProjectId::generate();
        let old_name = ScopedRemoteName {
            project: project.clone(),
            name: "old".into(),
        };
        let current_name = ScopedRemoteName {
            project,
            name: "current".into(),
        };
        view.observed_remote_connections
            .insert("physical".into(), Merge::normal(connection.clone()));
        view.observed_remote_names
            .insert(connection.clone(), Merge::normal(old_name.clone()));
        view.project_state
            .remote_names
            .insert(connection.clone(), Merge::normal(current_name.clone()));

        let historical = resolve(&view, "physical".as_ref()).unwrap().unwrap();
        assert_eq!(historical.source, IdentitySource::Historical);
        assert_eq!(historical.scoped_name, Some(&old_name));
        // Activating this physical key changes the authority, not the recorded
        // historical alias. No project definition is required by this query.
        view.remote_connections
            .insert("physical".into(), Merge::normal(connection));
        let logical = resolve(&view, "physical".as_ref()).unwrap().unwrap();
        assert_eq!(logical.source, IdentitySource::Logical);
        assert_eq!(logical.scoped_name, Some(&current_name));
    }
}
