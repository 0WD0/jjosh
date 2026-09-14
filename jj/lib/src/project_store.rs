// Copyright 2026 The Jujutsu Authors
// SPDX-License-Identifier: Apache-2.0

//! Strict codecs for operation-owned project state.

use std::collections::BTreeMap;

use crate::backend::CommitId;
use crate::merge::Merge;
use crate::object_id::ObjectId;
use crate::op_store::View;
use crate::project::*;
use crate::protos::simple_op_store as proto;
use crate::repo_path::RepoPathBuf;

fn id(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if bytes.len() != 16 { return Err(format!("Invalid project metadata ID length {} (expected 16)", bytes.len())); }
    Ok(bytes)
}
fn terms<T>(values: impl IntoIterator<Item = Result<Option<T>, String>>) -> Result<Merge<Option<T>>, String> {
    let values: Vec<_> = values.into_iter().collect::<Result<_, _>>()?;
    if values.len().is_multiple_of(2) { return Err(format!("Invalid metadata merge term count {}", values.len())); }
    Ok(Merge::from_vec(values))
}
fn insert<K: Ord, V>(map: &mut BTreeMap<K, V>, key: K, value: V) -> Result<(), String> {
    if map.insert(key, value).is_some() { return Err("Duplicate project metadata key".into()); }
    Ok(())
}
fn valid_label(label: &str) -> bool {
    !label.is_empty() && !label.contains(['#', '/', '\\']) && !label.chars().any(char::is_whitespace)
}
fn project_to_proto(value: &ProjectRecord) -> proto::ProjectRecord {
    proto::ProjectRecord { name: value.name.clone(), canonical_root: value.canonical_root.as_internal_file_string().to_owned() }
}
fn project_from_proto(value: proto::ProjectRecord) -> Result<ProjectRecord, String> {
    let root = RepoPathBuf::from_internal_string(value.canonical_root).map_err(|err| err.to_string())?;
    if root.is_root() || !valid_label(&value.name) { return Err("Invalid project name or canonical root".into()); }
    Ok(ProjectRecord { name: value.name, canonical_root: root })
}
fn binding_to_proto(value: &BindingRecord) -> proto::BindingRecord {
    let (project_id, repository_view) = match &value.target {
        BindingTarget::Project(id) => (Some(id.to_bytes()), false),
        BindingTarget::RepositoryView => (None, true),
    };
    let (representation, expression) = match &value.representation {
        Representation::Whole => (1, String::new()),
        Representation::JoshFilter(text) => (2, text.clone()),
        Representation::JoshView(text) => (3, text.clone()),
    };
    proto::BindingRecord { project_id, repository_view, connection_id: value.connection_id.to_bytes(), provider: "jjosh".into(), version: 1, representation, expression, base: value.base.clone() }
}
fn binding_from_proto(value: proto::BindingRecord) -> Result<BindingRecord, String> {
    if value.provider != "jjosh" || value.version != 1 { return Err(format!("Unsupported representation provider/version {:?}/{}", value.provider, value.version)); }
    let target = match (value.project_id, value.repository_view) {
        (Some(bytes), false) => BindingTarget::Project(ProjectId::new(id(bytes)?)),
        (None, true) => BindingTarget::RepositoryView,
        _ => return Err("Invalid binding target".into()),
    };
    let representation = match value.representation {
        1 if value.expression.is_empty() => Representation::Whole,
        2 if !value.expression.is_empty() => Representation::JoshFilter(value.expression),
        3 if !value.expression.is_empty() => Representation::JoshView(value.expression),
        _ => return Err("Unknown or malformed representation".into()),
    };
    Ok(BindingRecord { target, connection_id: ConnectionId::new(id(value.connection_id)?), representation, base: value.base })
}
fn observation_to_proto(value: &ConversionObservation) -> proto::ConversionObservation {
    proto::ConversionObservation {
        binding_id: value.binding_id.to_bytes(), binding: Some(binding_to_proto(&value.binding)),
        connection_id: value.connection_id.to_bytes(), endpoint: value.endpoint.clone(), raw_ref: value.raw_ref.clone(),
        terms: value.terms.iter().map(|term| proto::ConversionTerm { canonical: term.canonical.as_ref().map(ObjectId::to_bytes), raw: term.raw.clone() }).collect(),
        base: value.base.clone(), generation: value.generation.clone(),
    }
}
fn observation_from_proto(value: proto::ConversionObservation) -> Result<ConversionObservation, String> {
    if value.terms.len().is_multiple_of(2) || value.endpoint.is_empty() {
        return Err("Malformed conversion observation".into());
    }
    let terms = value.terms.into_iter().map(|term| {
        if (term.canonical.is_some() && term.raw.is_none()) || term.canonical.as_ref().is_some_and(Vec::is_empty) || term.raw.as_ref().is_some_and(|raw| raw.is_empty() || !raw.len().is_multiple_of(2) || !raw.bytes().all(|c| c.is_ascii_hexdigit())) {
            return Err("Malformed conversion witness object ID".into());
        }
        Ok(ConversionTerm { canonical: term.canonical.map(CommitId::new), raw: term.raw })
    }).collect::<Result<_, String>>()?;
    Ok(ConversionObservation {
        binding_id: BindingId::new(id(value.binding_id)?),
        binding: binding_from_proto(value.binding.ok_or("Missing observation binding")?)?,
        connection_id: ConnectionId::new(id(value.connection_id)?), endpoint: value.endpoint, raw_ref: value.raw_ref,
        terms, base: value.base, generation: value.generation,
    })
}

pub(crate) fn encode(view: &View) -> Option<proto::ProjectMetadata> {
    if view.project_state.is_empty() && view.remote_connections.is_empty() && view.project_observations.is_empty() { return None; }
    Some(proto::ProjectMetadata {
        version: 1,
        projects: view.project_state.projects.iter().map(|(id, target)| proto::ProjectEntry {
            id: id.to_bytes(), terms: target.iter().map(|value| proto::ProjectTerm { value: value.as_ref().map(project_to_proto) }).collect(),
        }).collect(),
        bindings: view.project_state.bindings.iter().map(|(id, target)| proto::BindingEntry {
            id: id.to_bytes(), terms: target.iter().map(|value| proto::BindingTerm { value: value.as_ref().map(binding_to_proto) }).collect(),
        }).collect(),
        labels: view.project_state.labels.iter().map(|(label, target)| proto::LabelEntry {
            label: label.clone(), terms: target.iter().map(|value| proto::IdTerm { value: value.as_ref().map(ObjectId::to_bytes) }).collect(),
        }).collect(),
        connections: view.remote_connections.iter().map(|(remote, target)| proto::ConnectionEntry {
            remote: remote.as_str().to_owned(), terms: target.iter().map(|value| proto::IdTerm { value: value.as_ref().map(ObjectId::to_bytes) }).collect(),
        }).collect(),
        observations: view.project_observations.iter().map(|(key, target)| proto::ObservationEntry {
            remote: key.remote.as_str().to_owned(), name: key.name.as_str().to_owned(),
            kind: match key.kind { ObservationKind::Bookmark => 1, ObservationKind::Tag => 2, ObservationKind::Revision => 3 },
            terms: target.iter().map(|value| proto::ObservationTerm { value: value.as_ref().map(observation_to_proto) }).collect(),
        }).collect(),
    })
}

pub(crate) fn decode(value: proto::ProjectMetadata, view: &mut View) -> Result<(), String> {
    if value.version != 1 { return Err(format!("Unsupported project metadata version {}", value.version)); }
    for entry in value.projects {
        let target = terms(entry.terms.into_iter().map(|term| term.value.map(project_from_proto).transpose()))?;
        let mut roots = target.iter().flatten().map(|r| &r.canonical_root);
        if let Some(root) = roots.next() && roots.any(|other| root != other) { return Err("Project identity has inconsistent immutable canonical roots".into()); }
        insert(&mut view.project_state.projects, ProjectId::new(id(entry.id)?), target)?;
    }
    for entry in value.bindings {
        let target = terms(entry.terms.into_iter().map(|term| term.value.map(binding_from_proto).transpose()))?;
        insert(&mut view.project_state.bindings, BindingId::new(id(entry.id)?), target)?;
    }
    for entry in value.labels {
        if !valid_label(&entry.label) { return Err("Invalid project label".into()); }
        let target = terms(entry.terms.into_iter().map(|term| term.value.map(|bytes| id(bytes).map(ProjectId::new)).transpose()))?;
        insert(&mut view.project_state.labels, entry.label, target)?;
    }
    for entry in value.connections {
        if entry.remote.is_empty() { return Err("Empty connection remote name".into()); }
        let target = terms(entry.terms.into_iter().map(|term| term.value.map(|bytes| id(bytes).map(ConnectionId::new)).transpose()))?;
        insert(&mut view.remote_connections, entry.remote.into(), target)?;
    }
    for entry in value.observations {
        if entry.remote.is_empty() || entry.name.is_empty() { return Err("Empty observation name".into()); }
        let target = terms(entry.terms.into_iter().map(|term| term.value.map(observation_from_proto).transpose()))?;
        let kind = match entry.kind {
            1 => ObservationKind::Bookmark,
            2 => ObservationKind::Tag,
            3 => ObservationKind::Revision,
            _ => return Err(format!("Unknown observation kind {}", entry.kind)),
        };
        let key = ObservationKey { remote: entry.remote.into(), name: entry.name.into(), kind };
        insert(&mut view.project_observations, key, target)?;
    }
    validate(view)
}

pub(crate) fn validate(view: &View) -> Result<(), String> {
    let check_id = |id: &dyn ObjectId| {
        if id.as_bytes().len() == 16 { Ok(()) } else { Err("Invalid project metadata ID length".to_owned()) }
    };
    for (id, target) in &view.project_state.projects {
        check_id(id)?;
        let mut root = None;
        for record in target.iter().flatten() {
            if !valid_label(&record.name) || record.canonical_root.is_root() {
                return Err("Invalid project name or canonical root".into());
            }
            if root.is_some_and(|previous| previous != &record.canonical_root) {
                return Err("Project identity has inconsistent immutable canonical roots".into());
            }
            root = Some(&record.canonical_root);
        }
    }
    for (label, target) in &view.project_state.labels {
        if !valid_label(label) { return Err("Invalid project label".into()); }
        for id in target.iter().flatten() { check_id(id)?; }
    }
    for (remote, target) in &view.remote_connections {
        if remote.as_str().is_empty() { return Err("Empty connection remote name".into()); }
        for id in target.iter().flatten() { check_id(id)?; }
    }
    // An immutable identity must have exactly one definition even in negative
    // terms and detached observation snapshots. Active-set conflicts are legal.
    let mut definitions = BTreeMap::new();
    for (id, target) in &view.project_state.bindings {
        check_id(id)?;
        for record in target.iter().flatten() {
            validate_binding(record)?;
            check_binding(&mut definitions, id, record)?;
        }
    }
    for (key, target) in &view.project_observations {
        if key.remote.as_str().is_empty() || key.name.as_str().is_empty() { return Err("Empty observation name".into()); }
        if key.kind == ObservationKind::Revision
            && (!key.name.as_str().len().is_multiple_of(2) || !key.name.as_str().bytes().all(|c| c.is_ascii_hexdigit())) {
            return Err("Revision observation key must be a raw object ID".into());
        }
        for observation in target.iter().flatten() {
            check_id(&observation.binding_id)?;
            check_id(&observation.connection_id)?;
            validate_binding(&observation.binding)?;
            if observation.terms.len().is_multiple_of(2) || observation.endpoint.is_empty() {
                return Err("Malformed conversion observation".into());
            }
            match key.kind {
                ObservationKind::Bookmark | ObservationKind::Tag if !observation.raw_ref.starts_with("refs/") => return Err("Reference observation requires a wire ref".into()),
                ObservationKind::Revision => {
                    if !observation.raw_ref.is_empty() || !observation.terms.iter().step_by(2).any(|term| term.raw.as_deref() == Some(key.name.as_str())) {
                        return Err("Revision observation requires its pinned raw OID and no wire ref".into());
                    }
                }
                _ => {}
            }
            for term in &observation.terms {
                if (term.canonical.is_some() && term.raw.is_none())
                    || term.canonical.as_ref().is_some_and(|id| id.as_bytes().is_empty())
                    || term.raw.as_ref().is_some_and(|raw| raw.is_empty() || !raw.len().is_multiple_of(2) || !raw.bytes().all(|c| c.is_ascii_hexdigit())) {
                    return Err("Malformed conversion witness object ID".into());
                }
            }
            check_binding(&mut definitions, &observation.binding_id, &observation.binding)?;
        }
    }
    Ok(())
}
fn check_binding<'a>(definitions: &mut BTreeMap<&'a BindingId, &'a BindingRecord>, id: &'a BindingId, record: &'a BindingRecord) -> Result<(), String> {
    if let Some(previous) = definitions.insert(id, record) && previous != record {
        return Err(format!("Binding {id} has inconsistent immutable definitions"));
    }
    Ok(())
}

fn validate_binding(record: &BindingRecord) -> Result<(), String> {
    if record.connection_id.as_bytes().len() != 16
        || matches!(&record.target, BindingTarget::Project(id) if id.as_bytes().len() != 16) {
        return Err("Invalid binding identity length".into());
    }
    if matches!(&record.representation, Representation::JoshFilter(text) | Representation::JoshView(text) if text.is_empty()) {
        return Err("Empty conversion representation".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> View {
        let mut view = View::make_root(CommitId::from_hex("00"));
        let project = ProjectId::generate();
        let connection = ConnectionId::generate();
        let binding = BindingId::generate();
        let record = BindingRecord { target: BindingTarget::Project(project.clone()), connection_id: connection.clone(), representation: Representation::Whole, base: None };
        view.project_state.projects.insert(project.clone(), Merge::normal(ProjectRecord { name: "lib".into(), canonical_root: RepoPathBuf::from_internal_string("lib").unwrap() }));
        view.project_state.bindings.insert(binding.clone(), Merge::normal(record.clone()));
        view.project_state.labels.insert("lib".into(), Merge::normal(project));
        view.remote_connections.insert("origin".into(), Merge::normal(connection.clone()));
        let observation = ConversionObservation { binding_id: binding, binding: record, connection_id: connection, endpoint: "file:///source".into(), raw_ref: "refs/heads/main".into(),
            terms: vec![ConversionTerm { canonical: Some(CommitId::from_hex("11")), raw: Some("aa".into()) }, ConversionTerm { canonical: Some(CommitId::from_hex("22")), raw: Some("bb".into()) }, ConversionTerm { canonical: None, raw: None }],
            base: Some("cc".into()), generation: Some("generation-1".into()) };
        view.project_observations.insert(ObservationKey { remote: "origin".into(), name: "main#lib".into(), kind: ObservationKind::Bookmark }, Merge::from_vec(vec![Some(observation.clone()), None, Some(ConversionObservation { endpoint: "file:///other".into(), ..observation })]));
        view
    }

    #[test]
    fn evidence_roundtrip_retains_all_signed_witness_roots() {
        let source = view();
        let mut restored = View::make_root(CommitId::from_hex("00"));
        decode(encode(&source).unwrap(), &mut restored).unwrap();
        assert_eq!(restored, source);
        let wrapped = crate::view::View::new(restored, true);
        let roots: std::collections::HashSet<_> = wrapped.all_referenced_commit_ids().cloned().collect();
        assert!(roots.contains(&CommitId::from_hex("11")));
        assert!(roots.contains(&CommitId::from_hex("22")));
    }

    #[test]
    fn rejects_unknown_versions_duplicate_keys_and_even_terms() {
        let source = view();
        let mut encoded = encode(&source).unwrap();
        encoded.version = 2;
        assert!(decode(encoded, &mut View::make_root(CommitId::from_hex("00"))).is_err());
        let mut encoded = encode(&source).unwrap();
        encoded.projects.push(encoded.projects[0].clone());
        assert!(decode(encoded, &mut View::make_root(CommitId::from_hex("00"))).is_err());
        let mut encoded = encode(&source).unwrap();
        encoded.observations[0].terms.clear();
        assert!(decode(encoded, &mut View::make_root(CommitId::from_hex("00"))).is_err());
        let mut encoded = encode(&source).unwrap();
        encoded.bindings[0].terms[0].value.as_mut().unwrap().version = 2;
        assert!(decode(encoded, &mut View::make_root(CommitId::from_hex("00"))).is_err());
        let mut encoded = encode(&source).unwrap();
        encoded.observations[0].kind = 0;
        assert!(decode(encoded, &mut View::make_root(CommitId::from_hex("00"))).is_err());
    }

    #[test]
    fn rejects_mutated_binding_definition_in_historical_evidence() {
        let mut source = view();
        let key = source.project_observations.keys().next().unwrap().clone();
        let mut observation = source.project_observations[&key].adds().flatten().next().unwrap().clone();
        observation.binding.base = Some("changed".into());
        source.project_observations.insert(key, Merge::normal(observation));
        assert!(validate(&source).is_err());
    }

    #[test]
    fn revision_witness_is_persisted_without_a_remote_reference() {
        let mut source = view();
        let mut observation = source.project_observations.values().next().unwrap()
            .adds().flatten().next().unwrap().clone();
        observation.raw_ref.clear();
        let key = ObservationKey { remote: "origin".into(), name: "aa".into(), kind: ObservationKind::Revision };
        source.project_observations.clear();
        source.project_observations.insert(key.clone(), Merge::normal(observation));
        let mut restored = View::make_root(CommitId::from_hex("00"));
        decode(encode(&source).unwrap(), &mut restored).unwrap();
        assert_eq!(source, restored);
        let wrapped = crate::view::View::new(restored, true);
        assert!(wrapped.validate_project_observation(&key).is_ok());
        assert!(wrapped.all_referenced_commit_ids().any(|id| id == &CommitId::from_hex("22")));
        // A removed raw term is historical evidence, not the revision requested.
        let evidence = source.project_observations.remove(&key).unwrap();
        source.project_observations.insert(ObservationKey { name: "bb".into(), ..key }, evidence);
        assert!(validate(&source).is_err());
    }
}
