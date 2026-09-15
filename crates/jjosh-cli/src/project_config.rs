use anyhow::{Result, ensure};
use jj_lib::merge::Merge;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::View;
use jj_lib::project::{BindingTarget, ProjectId, ProjectRecord};
use jj_lib::repo_path::RepoPath;

/// Every active suffix-bearing reference participates in a label claim, including
/// tracking and local Git mirrors. Never reinterpret a pre-existing literal name.
pub(crate) fn label_references(view: &View, label: &str) -> Vec<String> {
    let mut references = Vec::new();
    let matches = |name: &str| crate::ref_names::belongs_to_project(label, name);
    for (kind, refs) in [
        ("bookmark", &view.local_bookmarks),
        ("tag", &view.local_tags),
    ] {
        for (name, target) in refs {
            if target.is_present() && matches(name.as_str()) {
                references.push(format!("{kind} {}", name.as_str()));
            }
        }
    }
    for (remote, refs) in &view.remote_views {
        for (kind, refs) in [("bookmark", &refs.bookmarks), ("tag", &refs.tags)] {
            for (name, reference) in refs {
                if reference.target.is_present() && matches(name.as_str()) {
                    references.push(format!("{kind} {}@{}", name.as_str(), remote.as_str()));
                }
            }
        }
    }
    for (name, target) in &view.git_refs {
        if target.is_present() && matches(name.as_str()) {
            references.push(format!("Git ref {}", name.as_str()));
        }
    }
    for (key, observation) in &view.project_observations {
        if key.kind != jj_lib::project::ObservationKind::Revision
            && matches(key.name.as_str())
            && observation.adds().flatten().any(|observation| {
                observation
                    .terms
                    .iter()
                    .any(|term| term.canonical.is_some())
            })
        {
            references.push(format!(
                "observation {}@{}",
                key.name.as_str(),
                key.remote.as_str()
            ));
        }
    }
    if let Some(projects) = view.project_state.labels.get(label) {
        for (connection, names) in &view.project_state.remote_names {
            for name in names.adds().flatten() {
                if projects
                    .adds()
                    .flatten()
                    .any(|project| project == &name.project)
                {
                    references.push(format!(
                        "remote {}#{label} (connection {})",
                        name.name.as_str(),
                        connection.hex()
                    ));
                }
            }
        }
    }
    references.sort();
    references.dedup();
    references
}

pub(crate) fn validate_registration(view: &View, name: &str, root: &RepoPath) -> Result<()> {
    crate::native_project::validate_project(name)?;
    ensure!(
        !root.is_root(),
        "A project requires a non-root canonical directory"
    );
    for (id, definition) in &view.project_state.projects {
        for record in definition.adds().flatten() {
            ensure!(
                record.name != name,
                "Project name {name:?} is already claimed by {}",
                id.hex()
            );
            ensure!(
                root != record.canonical_root.as_ref(),
                "Project path {} is already claimed at {} ({}, {})",
                root.as_internal_file_string(),
                record.canonical_root.as_internal_file_string(),
                record.name,
                id.hex()
            );
        }
    }
    ensure!(
        view.project_state
            .labels
            .get(name)
            .is_none_or(Merge::is_absent),
        "Reference label {name:?} is already registered or unresolved"
    );
    let references = label_references(view, name);
    ensure!(
        references.is_empty(),
        "Reference label {name:?} is occupied by literal references: {}. Choose another label; \
         this jjosh version does not reinterpret existing names",
        references.join(", ")
    );
    Ok(())
}

pub(crate) fn register(view: &mut View, name: &str, root: &RepoPath) -> Result<ProjectId> {
    validate_registration(view, name, root)?;
    let id = ProjectId::generate();
    view.project_state.projects.insert(
        id.clone(),
        Merge::resolved(Some(ProjectRecord {
            name: name.to_owned(),
            canonical_root: root.to_owned(),
        })),
    );
    view.project_state
        .labels
        .insert(name.to_owned(), Merge::resolved(Some(id.clone())));
    Ok(id)
}

/// Removal never cascades through bindings, references, or unresolved labels.
pub(crate) fn validate_removal(view: &View, id: &ProjectId) -> Result<Vec<String>> {
    for (binding_id, definition) in &view.project_state.bindings {
        ensure!(
            !definition
                .adds()
                .flatten()
                .any(|record| record.target == BindingTarget::Project(id.clone())),
            "Project {} has active binding {}; remove its remote or explicitly retire the disconnected binding with project resolve --binding ID --delete first",
            id.hex(),
            binding_id.hex()
        );
    }
    for (connection, names) in &view.project_state.remote_names {
        ensure!(
            !names.adds().flatten().any(|name| &name.project == id),
            "Project {} still has remote connection {}; retire its scoped name before removing \
             the project",
            id.hex(),
            connection.hex()
        );
    }
    let mut labels = Vec::new();
    for (label, target) in &view.project_state.labels {
        if target.adds().flatten().any(|candidate| candidate == id) {
            ensure!(
                target.as_resolved().is_some(),
                "Reference label {label:?} is unresolved; resolve it before removing its project"
            );
            let references = label_references(view, label);
            ensure!(
                references.is_empty(),
                "Project label {label:?} still has references or tracking: {}",
                references.join(", ")
            );
            labels.push(label.clone());
        }
    }
    Ok(labels)
}
