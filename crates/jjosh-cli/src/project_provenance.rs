use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::Write as _;

use anyhow::{Context, Result, bail, ensure};
use gix::ObjectId;
use gix::refs::transaction::{PreviousValue, RefEdit};
use jj_lib::backend::CommitId;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::View;
use jj_lib::project::Representation;

use crate::source_repo::{SourceRepo, copy_objects};

/// A read-only snapshot of the evidence needed by the imported operation.
/// Canonical anchors are graph roots; raw objects never become JJ graph roots.
pub(crate) struct Capture {
    source: gix::Repository,
    prefixes: BTreeMap<String, BTreeMap<String, ObjectId>>,
    selected: BTreeMap<String, Option<ObjectId>>,
    canonical_refs: BTreeSet<String>,
    raw_roots: BTreeSet<ObjectId>,
    raw_boundaries: HashSet<ObjectId>,
    caches: BTreeMap<String, Cache>,
    pub canonical_roots: Vec<CommitId>,
}

struct Cache {
    source: SourceRepo,
    refs: BTreeMap<String, ObjectId>,
    shallow: Option<Vec<u8>>,
    boundaries: HashSet<ObjectId>,
    generations: Vec<ObjectId>,
}

pub(crate) struct Prepared {
    pub refs: Vec<RefEdit>,
}

fn refs(git: &gix::Repository, prefix: &str) -> Result<BTreeMap<String, ObjectId>> {
    let mut result = BTreeMap::new();
    for reference in git.references()?.prefixed(prefix)? {
        let reference = reference.map_err(anyhow::Error::from_boxed)?;
        let name = std::str::from_utf8(reference.name().as_bstr())?.to_owned();
        let gix::refs::TargetRef::Object(id) = reference.target() else {
            bail!("Preserved evidence ref {name} must not be symbolic");
        };
        result.insert(name, id.to_owned());
    }
    Ok(result)
}

fn target(git: &gix::Repository, name: &str) -> Result<Option<ObjectId>> {
    let Some(reference) = git.try_find_reference(name)? else {
        return Ok(None);
    };
    let gix::refs::TargetRef::Object(id) = reference.target() else {
        bail!("Preserved evidence ref {name} must not be symbolic");
    };
    Ok(Some(id.to_owned()))
}

fn shallow(git: &gix::Repository) -> Result<Option<Vec<u8>>> {
    match fs::read(git.git_dir().join("shallow")) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("Reading preserved source shallow boundaries"),
    }
}

fn oid(value: &str) -> Result<ObjectId> {
    let id =
        ObjectId::from_hex(value.as_bytes()).context("Malformed preserved raw object identity")?;
    ensure!(
        id.kind() == gix::hash::Kind::Sha1 && !id.is_null(),
        "Invalid preserved raw object identity"
    );
    Ok(id)
}

fn blob(git: &gix::Repository, id: ObjectId) -> Result<Vec<u8>> {
    let object = git.find_object(id)?;
    ensure!(
        object.kind == gix_object::Kind::Blob,
        "Preserved evidence {id} must be a blob"
    );
    Ok(object.data.to_vec())
}

fn edit(git: &gix::Repository, name: &str, id: ObjectId) -> Result<RefEdit> {
    let old = target(git, name)?;
    ensure!(
        old.is_none_or(|old| old == id),
        "Conflicting destination provenance ref {name}"
    );
    Ok(RefEdit::update(
        name.try_into()?,
        id,
        old.map_or(PreviousValue::MustNotExist, |old| {
            PreviousValue::MustExistAndMatch(old.into())
        }),
        "preserve imported project provenance",
    ))
}

impl Cache {
    fn read(source: SourceRepo) -> Result<Self> {
        let shallow = shallow(source.git())?;
        let refs = refs(source.git(), "refs/jjosh/")?;
        let mut generations = Vec::new();
        for (name, id) in &refs {
            if name == crate::source_repo::INITIALIZED_REF {
                ensure!(
                    blob(source.git(), *id)? == b"source-context-v1\n",
                    "Unsupported source cache initialization marker"
                );
            } else if let Some(value) = name.strip_prefix("refs/jjosh/generations/") {
                ensure!(oid(value)? == *id, "Modified source generation ref {name}");
                let object = source.git().find_object(*id)?;
                ensure!(
                    object.kind == gix_object::Kind::Commit,
                    "Source generation is not a commit"
                );
                generations.push(*id);
            } else if let Some(value) = name.strip_prefix("refs/jjosh/observed-objects/") {
                ensure!(oid(value)? == *id, "Modified source observation ref {name}");
            } else if name.starts_with("refs/jjosh/normalized/") {
                // import_boundaries validates the complete paired inverse mapping.
            } else if let Some(value) = name.strip_prefix("refs/jjosh/remote/") {
                let (endpoint, wire) = value
                    .split_once('/')
                    .context("Malformed source endpoint ref")?;
                oid(endpoint)?;
                ensure!(
                    wire.starts_with("refs/heads/")
                        || wire.starts_with("refs/tags/")
                        || wire.starts_with("pins/"),
                    "Unsupported source endpoint ref {name}"
                );
            } else {
                bail!("Unsupported source cache evidence ref {name}");
            }
        }
        // Complete generations must never inherit raw-history shallow cutoffs.
        // Validate their shared ancestry once, even when every historical tip is retained.
        copy_objects(source.git(), None, &generations, &HashSet::new(), false)?;
        let boundaries = source.import_boundaries()?;
        copy_objects(
            source.git(),
            None,
            &refs.values().copied().collect::<Vec<_>>(),
            &boundaries,
            false,
        )?;
        Ok(Self {
            source,
            refs,
            shallow,
            boundaries,
            generations,
        })
    }

    fn verify(&self) -> Result<()> {
        ensure!(
            refs(self.source.git(), "refs/jjosh/")? == self.refs
                && shallow(self.source.git())? == self.shallow,
            "Source cache evidence changed during project import"
        );
        Ok(())
    }

    fn preflight(&self, main: &gix::Repository, endpoint: &str) -> Result<()> {
        if let Some(destination) = SourceRepo::open_existing(main, endpoint)? {
            self.check_destination(&destination)?;
            for (name, id) in &self.refs {
                edit(destination.git(), name, *id)?;
            }
        }
        Ok(())
    }

    fn check_destination(&self, destination: &SourceRepo) -> Result<Option<Vec<u8>>> {
        let current = shallow(destination.git())?;
        ensure!(
            current == self.shallow
                || (current.is_none() && refs(destination.git(), "refs/")?.is_empty()),
            "Conflicting destination source cache shallow boundaries"
        );
        Ok(current)
    }

    fn install(&self, main: &gix::Repository, endpoint: &str) -> Result<()> {
        if SourceRepo::install_new(main, endpoint, |destination| self.populate(destination))? {
            return Ok(());
        }
        let destination = SourceRepo::open_existing(main, endpoint)?
            .context("Destination source cache disappeared during import")?;
        self.populate(&destination)
    }

    fn populate(&self, destination: &SourceRepo) -> Result<()> {
        // Cooperate with Git's shallow-file lock, including concurrent fetches.
        let mut lock = gix::lock::File::acquire_to_update_resource(
            destination.path().join("shallow"),
            gix::lock::acquire::Fail::Immediately,
            None,
        )
        .context("Locking destination source cache shallow boundaries")?;
        // An initialized cache with no refs or shallow file has no history to
        // conflict with. Never replace a populated cache's boundary policy.
        let current = self.check_destination(destination)?;
        let edits = self
            .refs
            .iter()
            .map(|(name, id)| edit(destination.git(), name, *id))
            .collect::<Result<Vec<_>>>()?;
        copy_objects(
            self.source.git(),
            Some(destination.git()),
            &self.generations,
            &HashSet::new(),
            false,
        )?;
        copy_objects(
            self.source.git(),
            Some(destination.git()),
            &self.refs.values().copied().collect::<Vec<_>>(),
            &self.boundaries,
            false,
        )?;
        if current != self.shallow {
            lock.write_all(self.shallow.as_deref().unwrap_or_default())?;
            lock.with_mut(|file| file.sync_all())?;
            lock.commit()?;
        }
        destination.git().edit_references(edits)?;
        Ok(())
    }
}

impl Capture {
    pub(crate) fn read(source_git: &gix::Repository, source_view: &View) -> Result<Self> {
        ensure!(
            source_git.object_hash() == gix::hash::Kind::Sha1,
            "Preserving project provenance requires SHA-1"
        );
        let mut source = gix::open(source_git.git_dir())?;
        source.clear_namespace();
        let mut capture = Self {
            source,
            prefixes: BTreeMap::new(),
            selected: BTreeMap::new(),
            canonical_refs: BTreeSet::new(),
            raw_roots: BTreeSet::new(),
            raw_boundaries: HashSet::new(),
            caches: BTreeMap::new(),
            canonical_roots: Vec::new(),
        };
        // Publication authority exists independently of canonical observations:
        // old leases and published-but-untracked branches still belong to the
        // recorded source. Never infer their absence from the operation view.
        for prefix in crate::remote_refs::PUBLICATION_PREFIXES {
            let values = refs(&capture.source, prefix)?;
            for (name, id) in &values {
                crate::remote_refs::validate_publication_record(&capture.source, name, *id)?;
            }
            capture.raw_roots.extend(values.values().copied());
            capture.prefixes.insert(prefix.to_owned(), values);
        }
        for prefix in crate::remote_refs::PUBLICATION_PREFIXES {
            for name in capture.prefixes[prefix].keys() {
                let key = &name[prefix.len()..];
                for counterpart in crate::remote_refs::PUBLICATION_PREFIXES {
                    let name = format!("{counterpart}{key}");
                    if !capture.prefixes[counterpart].contains_key(&name) {
                        capture.selected.insert(name, None);
                    }
                }
            }
        }
        let bindings: BTreeSet<_> = source_view
            .project_state
            .bindings
            .keys()
            .cloned()
            .chain(
                source_view
                    .project_observations
                    .values()
                    .flat_map(|values| {
                        values
                            .iter()
                            .flatten()
                            .map(|observation| observation.binding_id.clone())
                    }),
            )
            .collect();
        for binding in bindings {
            let prefix = crate::native_project::binding_ref_prefix(&binding);
            let values = refs(&capture.source, &prefix)?;
            for (name, id) in &values {
                let suffix = name
                    .strip_prefix(&prefix)
                    .expect("selected binding namespace");
                if suffix == "offline" {
                    ensure!(
                        blob(&capture.source, *id)? == b"offline-native-import-v1\n",
                        "Unsupported offline binding marker"
                    );
                    capture.raw_roots.insert(*id);
                } else if let Some((kind, raw)) = suffix.split_once('/') {
                    let raw = oid(raw)?;
                    match kind {
                        "origin" | "graft" | "published" => {
                            ensure!(
                                values.get(&format!("{prefix}{raw}")) == Some(&raw),
                                "Missing raw retention for native anchor {name}"
                            );
                            ensure!(
                                capture.source.find_object(*id)?.kind == gix_object::Kind::Commit,
                                "Native anchor {name} does not target a commit"
                            );
                            capture.canonical_refs.insert(name.clone());
                            capture
                                .canonical_roots
                                .push(CommitId::from_bytes(id.as_bytes()));
                        }
                        "observed" => {
                            ensure!(raw == *id, "Modified native observed-object ref {name}");
                            capture.raw_roots.insert(raw);
                        }
                        _ => bail!("Unsupported native evidence ref {name}"),
                    }
                } else {
                    ensure!(
                        oid(suffix)? == *id,
                        "Modified native raw retention ref {name}"
                    );
                    ensure!(
                        capture.source.find_object(*id)?.kind == gix_object::Kind::Commit,
                        "Native raw retention is not a commit"
                    );
                    capture.raw_roots.insert(*id);
                }
            }
            capture.prefixes.insert(prefix, values);
        }
        for observation in source_view
            .project_observations
            .values()
            .flat_map(|values| values.iter().flatten())
        {
            ensure!(
                !observation.endpoint.is_empty(),
                "Missing source observation endpoint"
            );
            let generation = observation
                .generation
                .as_deref()
                .map(crate::source_repo::parse_generation)
                .transpose()?;
            let cache_endpoint = generation
                .as_ref()
                .map_or(observation.endpoint.as_str(), |(endpoint, _)| {
                    endpoint.as_str()
                });
            let uses_cache = generation.is_some()
                || !matches!(observation.binding.representation, Representation::Whole);
            if uses_cache
                && !capture.caches.contains_key(cache_endpoint)
                && let Some(source) = SourceRepo::open_existing(&capture.source, cache_endpoint)?
            {
                capture
                    .caches
                    .insert(cache_endpoint.to_owned(), Cache::read(source)?);
            }
            let cache = uses_cache
                .then(|| capture.caches.get(cache_endpoint))
                .flatten();
            let raw: Vec<_> = observation
                .terms
                .iter()
                .filter_map(|term| term.raw.as_deref())
                .map(oid)
                .collect::<Result<_>>()?;
            if let Some((_, input)) = generation {
                let cache = cache.context("Observed source generation cache is missing")?;
                ensure!(!raw.is_empty(), "Source generation has no raw observation");
                for id in &raw {
                    let peeled = cache.source.git().find_object(*id)?.peel_tags_to_end()?;
                    ensure!(
                        peeled.kind == gix_object::Kind::Commit,
                        "Observed source object does not peel to a commit"
                    );
                    cache
                        .source
                        .verify_import_generation_witness(peeled.id, input)?;
                }
            } else if !matches!(observation.binding.representation, Representation::Whole)
                && !raw.is_empty()
            {
                bail!("Filtered source observation lacks an immutable source generation");
            }
            if let Some(cache) = cache {
                copy_objects(cache.source.git(), None, &raw, &cache.boundaries, false)?;
                for id in &raw {
                    ensure!(
                        cache.refs.values().any(|retained| retained == id),
                        "Source observation {id} lacks durable cache retention"
                    );
                }
            } else {
                capture.raw_roots.extend(raw);
            }
            let prefix = crate::git_remote::raw_ref_prefix(&capture.source, &observation.endpoint)?;
            if !capture.prefixes.contains_key(&prefix) {
                let values = refs(&capture.source, &prefix)?;
                for name in values.keys() {
                    let wire = name
                        .strip_prefix(&prefix)
                        .expect("selected endpoint namespace");
                    ensure!(
                        wire.starts_with("refs/heads/")
                            || wire.starts_with("refs/tags/")
                            || wire.starts_with("pins/"),
                        "Unsupported raw endpoint evidence {name}"
                    );
                }
                capture.raw_roots.extend(values.values().copied());
                capture.prefixes.insert(prefix, values);
            }
            if !observation.raw_ref.is_empty() {
                ensure!(
                    observation.raw_ref.starts_with("refs/heads/")
                        || observation.raw_ref.starts_with("refs/tags/"),
                    "Unsupported source publication ref"
                );
                let key = gix_object::compute_hash(
                    capture.source.object_hash(),
                    gix_object::Kind::Blob,
                    format!("{}\0{}", observation.endpoint, observation.raw_ref).as_bytes(),
                )?;
                for prefix in crate::remote_refs::PUBLICATION_PREFIXES {
                    let name = format!("{prefix}{key}");
                    if !capture.prefixes[prefix].contains_key(&name) {
                        capture.selected.insert(name, None);
                    }
                }
            }
        }
        capture.raw_boundaries.extend(
            capture
                .caches
                .values()
                .flat_map(|cache| cache.boundaries.iter().copied()),
        );
        capture.canonical_roots.sort();
        capture.canonical_roots.dedup();
        copy_objects(
            &capture.source,
            None,
            &capture.raw_roots.iter().copied().collect::<Vec<_>>(),
            &capture.raw_boundaries,
            false,
        )?;
        capture.verify_source()?;
        Ok(capture)
    }

    pub(crate) fn verify_source(&self) -> Result<()> {
        for (prefix, expected) in &self.prefixes {
            ensure!(
                refs(&self.source, prefix)? == *expected,
                "Source provenance changed during project import: {prefix}"
            );
        }
        for (name, expected) in &self.selected {
            ensure!(
                target(&self.source, name)? == *expected,
                "Source publication evidence changed during project import: {name}"
            );
        }
        for cache in self.caches.values() {
            cache.verify()?;
        }
        Ok(())
    }

    /// Materialize immutable objects and complete source caches, without publishing
    /// the returned main-repository ref edits. The import journal owns those edits.
    pub(crate) fn materialize(
        &self,
        destination_git: &gix::Repository,
        ids: &HashMap<CommitId, CommitId>,
    ) -> Result<Prepared> {
        self.verify_source()?;
        ensure!(
            destination_git.object_hash() == self.source.object_hash(),
            "Provenance object format mismatch"
        );
        let mut prepared = Vec::new();
        for (name, old) in self
            .prefixes
            .values()
            .flat_map(|values| values.iter())
            .chain(
                self.selected
                    .iter()
                    .filter_map(|(name, value)| value.as_ref().map(|id| (name, id))),
            )
        {
            let id = if self.canonical_refs.contains(name) {
                let canonical = CommitId::from_bytes(old.as_bytes());
                ObjectId::try_from(
                    ids.get(&canonical)
                        .with_context(|| format!("Missing imported canonical anchor {canonical}"))?
                        .as_bytes(),
                )?
            } else {
                *old
            };
            prepared.push(edit(destination_git, name, id)?);
        }
        for (name, value) in &self.selected {
            if value.is_none() {
                ensure!(
                    target(destination_git, name)?.is_none(),
                    "Conflicting destination publication evidence {name}"
                );
            }
        }
        for (endpoint, cache) in &self.caches {
            cache.preflight(destination_git, endpoint)?;
        }
        copy_objects(
            &self.source,
            Some(destination_git),
            &self.raw_roots.iter().copied().collect::<Vec<_>>(),
            &self.raw_boundaries,
            false,
        )?;
        for (endpoint, cache) in &self.caches {
            cache.install(destination_git, endpoint)?;
        }
        self.verify_source()?;
        Ok(Prepared { refs: prepared })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn population_preserves_an_unknown_shallow_lock_and_retries_after_release() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let main = gix::init_bare(temporary.path().join("main.git"))?;
        let cache = Cache::read(SourceRepo::open(&main, "source")?)?;
        let destination = SourceRepo::open(&main, "destination")?;
        let lock_path = destination.path().join("shallow.lock");
        let pending = b"another writer's pending shallow boundaries\n";
        fs::write(&lock_path, pending)?;
        assert!(cache.populate(&destination).is_err());
        assert_eq!(fs::read(&lock_path)?, pending);
        assert!(!destination.path().join("shallow").exists());

        fs::remove_file(&lock_path)?;
        cache.populate(&destination)?;
        assert!(!lock_path.exists());
        assert!(!destination.path().join("shallow").exists());
        Ok(())
    }
}
