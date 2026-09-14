use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use gix::ObjectId;
use gix::bstr::ByteSlice as _;
use gix_object::{Find as _, Write as _};
use josh_core::cache::Transaction;
use josh_core::objects::CommitData;

const PROVENANCE: &[u8] = b"jjosh-source-raw";
const MAP_PREFIX: &str = "refs/jjosh/normalized/";
pub(crate) const INITIALIZED_REF: &str = "refs/jjosh/source-initialized";

/// An endpoint's raw history and its complete, immutable Josh input graphs.
/// Transactions passed to this store must be opened on `path()`, not the main repo.
pub struct SourceRepo {
    git: gix::Repository,
}

#[derive(Debug, Default)]
pub struct Normalized {
    pub tips: BTreeMap<ObjectId, ObjectId>,
    /// Raw-to-complete correspondences, including unchanged ancestor identities.
    pub pairs: Vec<(ObjectId, ObjectId)>,
}

/// The generation locates its immutable graph, independently of publication endpoint.
pub(crate) fn generation(endpoint: &str, input: ObjectId) -> String {
    serde_json::to_string(&(endpoint, input.to_string())).expect("generation strings serialize")
}

pub(crate) fn parse_generation(value: &str) -> Result<(String, ObjectId)> {
    let (endpoint, input): (String, String) = serde_json::from_str(value).context("Invalid source generation witness")?;
    Ok((endpoint, ObjectId::from_hex(input.as_bytes())?))
}

impl SourceRepo {

    pub fn open(main: &gix::Repository, endpoint: &str) -> Result<Self> {
        ensure!(main.object_hash() == gix::hash::Kind::Sha1, "Source projection requires SHA-1");
        let path = Self::cache_path(main, endpoint)?;
        if !path.try_exists()? {
            let parent = path.parent().context("Source cache has no parent directory")?;
            fs::create_dir_all(parent)?;
            // Install a fully configured repository, never a partially initialized cache.
            let temporary = tempfile::tempdir_in(parent)?;
            let initial = gix::init_bare(temporary.path())?;
            let config_path = main.config_path(gix::config::Source::Local)?.canonicalize()?;
            let objects_path = main.objects.store_ref().path().canonicalize()?;
            let mut config = b"[include]\n\tpath = ".to_vec();
            config.extend(quoted_path(&config_path));
            config.extend_from_slice(b"\n[core]\n\trepositoryFormatVersion = 0\n\tbare = true\n\tlogAllRefUpdates = false\n[gitoxide \"core\"]\n\tshallowFile = shallow\n");
            fs::write(initial.git_dir().join("config"), config)?;
            fs::create_dir_all(initial.git_dir().join("objects/info"))?;
            let mut alternate = quoted_path(&objects_path);
            alternate.push(b'\n');
            fs::write(initial.git_dir().join("objects/info/alternates"), alternate)?;
            drop(initial);
            if let Err(error) = fs::rename(temporary.path(), &path) {
                // A concurrent initializer may have installed the same endpoint.
                if !path.try_exists()? {
                    return Err(error).context("Installing source repository");
                }
            }
        }
        let mut git = gix::open_opts(
            path,
            gix::open::Options::default().config_overrides([
                "core.bare=true",
                "core.logAllRefUpdates=false",
                "gitoxide.core.shallowFile=shallow",
            ]),
        )?;
        git.clear_namespace();
        ensure!(git.is_bare(), "Source cache must be bare");
        ensure!(git.object_hash() == main.object_hash(), "Source cache object format mismatch");
        Ok(Self { git })
    }

    pub fn git(&self) -> &gix::Repository {
        &self.git
    }

    pub fn path(&self) -> &Path {
        self.git.git_dir()
    }

    /// Recover the exact complete input graph witnessed by an older operation.
    pub fn witnessed_generation(&self, tx: &Transaction, raw: ObjectId, input: ObjectId) -> Result<Normalized> {
        self.check_transaction(tx)?;
        let inverse = self.persisted_inverse()?;
        ensure!(inverse.get(&input).copied().unwrap_or(input) == raw, "Source generation does not identify the observed raw input");
        let retained = tx.resolve_ref(&format!("refs/jjosh/generations/{input}"))?;
        ensure!(retained == Some(input), "Observed source generation is unavailable; fetch cannot replace historical conversion evidence");
        let mut visited = HashSet::new();
        let mut pending = vec![input];
        let mut pairs = Vec::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) { continue; }
            let commit = CommitData::read(tx.odb(), id)?;
            pending.extend(commit.parsed()?.parents());
            pairs.push((inverse.get(&id).copied().unwrap_or(id), id));
        }
        copy_objects(tx.odb(), None, &[input], &HashSet::new(), false)?;
        Ok(Normalized { tips: BTreeMap::from([(raw, input)]), pairs })
    }

    pub fn retain_observations(&self, tx: &Transaction, objects: &[ObjectId]) -> Result<()> {
        self.retain_raw(tx, objects)?;
        for id in objects {
            let name = format!("refs/jjosh/observed-objects/{id}");
            let old = tx.resolve_ref(&name)?;
            ensure!(old.is_none_or(|old| old == *id), "Observed object retention was modified");
            tx.update_ref(&name, old.map_or(josh_core::cache::Expected::Absent, josh_core::cache::Expected::At), *id, "retain immutable source observation")?;
        }
        Ok(())
    }

    /// Import the previous source context once, without changing canonical refs.
    /// Push previews use the old context read-only until a fetch completes this move.
    pub fn migrate_context(&self, main: &gix::Repository, prefix: &str) -> Result<()> {
        let tx = crate::interop::open_josh_transaction(self.path(), false)
            .map_err(|error| anyhow::anyhow!(error.error))?;
        if tx.resolve_ref(INITIALIZED_REF)?.is_some() {
            return Ok(());
        }
        let mut migrated = Vec::new();
        for reference in main.references()?.prefixed(prefix)? {
            let reference = reference.map_err(anyhow::Error::from_boxed)?;
            let name = std::str::from_utf8(reference.name().as_bstr())?.to_owned();
            let gix::refs::TargetRef::Object(id) = reference.target() else {
                bail!("Source context {name} must not be symbolic");
            };
            if tx.resolve_ref(&name)?.is_none() {
                migrated.push((name, id.to_owned()));
            }
        }
        self.retain_raw(&tx, &migrated.iter().map(|(_, id)| *id).collect::<Vec<_>>())?;
        for (name, id) in migrated {
            tx.update_ref(&name, josh_core::cache::Expected::Absent, id, "isolate source context")?;
        }
        let marker = josh_core::objects::write_blob(tx.odb(), b"source-context-v1\n")?;
        tx.update_ref(INITIALIZED_REF, josh_core::cache::Expected::Absent, marker, "initialize isolated source")?;
        tx.flush_mem_odb()
    }

    /// Source refs must remain valid even if canonical objects are later collected.
    pub fn retain_raw(&self, tx: &Transaction, tips: &[ObjectId]) -> Result<()> {
        self.check_transaction(tx)?;
        copy_objects(tx.odb(), Some(&self.git), tips, &self.retention_boundaries(tx)?, false)
    }

    /// Cut exactly the declared shallow edges, and rewrite only their descendants.
    /// The provenance header makes otherwise identical boundary snapshots injective.
    pub fn normalize(&self, tx: &Transaction, tips: &[ObjectId]) -> Result<Normalized> {
        self.check_transaction(tx)?;
        let shallow = self.boundaries()?;
        let mut mapped = HashMap::new();
        let mut active = HashSet::new();
        let mut pairs = Vec::new();
        let mut pending: Vec<_> = tips.iter().rev().copied().map(CommitWork::Visit).collect();
        while let Some(work) = pending.pop() {
            match work {
                CommitWork::Visit(id) => {
                    if mapped.contains_key(&id) {
                        continue;
                    }
                    ensure!(active.insert(id), "Cycle in source history at {id}");
                    let commit = CommitData::read(tx.odb(), id)?;
                    let parsed = commit.parsed()?;
                    let parents: Vec<_> = if shallow.contains(&id) {
                        Vec::new()
                    } else {
                        parsed.parents().collect()
                    };
                    drop(parsed);
                    pending.push(CommitWork::Finish(commit));
                    pending.extend(parents.into_iter().rev().map(CommitWork::Visit));
                }
                CommitWork::Finish(commit) => {
                    let id = commit.id();
                    let parsed = commit.parsed()?;
                    let parents: Vec<_> = if shallow.contains(&id) {
                        Vec::new()
                    } else {
                        parsed.parents().map(|parent| mapped[&parent]).collect()
                    };
                    let rewritten = if parsed.parents().eq(parents.iter().copied()) {
                        id
                    } else {
                        rewrite(tx, parsed, &parents, Some(id))?
                    };
                    mapped.insert(id, rewritten);
                    active.remove(&id);
                    pairs.push((id, rewritten));
                }
            }
        }
        // Verify trees and blobs as well as ancestry before exposing a Josh input.
        let normalized_tips: Vec<_> = tips.iter().map(|tip| mapped[tip]).collect();
        copy_objects(tx.odb(), None, &normalized_tips, &HashSet::new(), false)?;
        Ok(Normalized {
            tips: tips.iter().map(|tip| (*tip, mapped[tip])).collect(),
            pairs,
        })
    }

    /// Retain every nonidentity generation. Ref names are keyed by normalized ID,
    /// not raw ID: deepening must never overwrite an older inverse mapping.
    pub fn record_normalized(&self, tx: &Transaction, normalized: &Normalized) -> Result<()> {
        self.check_transaction(tx)?;
        let raw_roots: Vec<_> = normalized.tips.keys().copied().collect();
        self.retain_raw(tx, &raw_roots)?;
        let complete_roots: Vec<_> = normalized.tips.values().copied().collect();
        copy_objects(tx.odb(), Some(&self.git), &complete_roots, &HashSet::new(), false)?;
        for input in &complete_roots {
            let name = format!("refs/jjosh/generations/{input}");
            let old = tx.resolve_ref(&name)?;
            ensure!(old.is_none_or(|old| old == *input), "Source generation retention was modified");
            tx.update_ref(&name, old.map_or(josh_core::cache::Expected::Absent, josh_core::cache::Expected::At), *input, "retain source normalization generation")?;
        }
        let mut inverse = BTreeMap::new();
        for &(raw, input) in &normalized.pairs {
            if raw != input {
                insert_mapping(&mut inverse, input, raw)?;
            }
        }
        if inverse.is_empty() {
            return Ok(());
        }
        let roots: Vec<_> = inverse.iter().flat_map(|(&input, &raw)| [input, raw]).collect();
        // Do not skip alternates here: source retention must not rely on a main
        // repository GC retaining objects only reachable from source-side refs.
        copy_objects(tx.odb(), Some(&self.git), &roots, &self.retention_boundaries(tx)?, false)?;
        let mut edits = Vec::with_capacity(inverse.len() * 2);
        for (input, raw) in inverse {
            for (suffix, target) in [("raw", raw), ("normalized", input)] {
                edits.push(gix::refs::transaction::RefEdit::update(
                    format!("{MAP_PREFIX}{input}/{suffix}").try_into()?,
                    target,
                    gix::refs::transaction::PreviousValue::ExistingMustMatch(target.into()),
                    "retain source normalization",
                ));
            }
        }
        self.git.edit_references(edits)?;
        Ok(())
    }

    /// Reverse both the current normalization and every retained older generation.
    /// Known raw originals are terminal anchors, even if their parents are absent.
    pub fn denormalize(&self, tx: &Transaction, tip: ObjectId, normalized: &Normalized) -> Result<Normalized> {
        self.check_transaction(tx)?;
        let mut anchors = self.persisted_inverse()?;
        for &(raw, input) in &normalized.pairs {
            insert_mapping(&mut anchors, input, raw)?;
        }
        let raw_anchors: Vec<_> = anchors.values().copied().collect();
        for raw in raw_anchors {
            anchors.entry(raw).or_insert(raw);
        }
        let mut mapped = HashMap::new();
        let mut active = HashSet::new();
        let mut pending = vec![CommitWork::Visit(tip)];
        while let Some(work) = pending.pop() {
            match work {
                CommitWork::Visit(id) => {
                    if mapped.contains_key(&id) {
                        continue;
                    }
                    if let Some(&raw) = anchors.get(&id) {
                        // Check the anchor itself, but never walk its raw parents.
                        CommitData::read(tx.odb(), raw)?.parsed()?;
                        mapped.insert(id, raw);
                        continue;
                    }
                    ensure!(active.insert(id), "Cycle in exported history at {id}");
                    let commit = CommitData::read(tx.odb(), id)?;
                    let parents: Vec<_> = commit.parsed()?.parents().collect();
                    pending.push(CommitWork::Finish(commit));
                    pending.extend(parents.into_iter().rev().map(CommitWork::Visit));
                }
                CommitWork::Finish(commit) => {
                    let id = commit.id();
                    let parsed = commit.parsed()?;
                    let parents: Vec<_> = parsed.parents().map(|parent| mapped[&parent]).collect();
                    let has_provenance = parsed.extra_headers.iter().any(|(key, _)| *key == PROVENANCE);
                    let raw = if !has_provenance && parsed.parents().eq(parents.iter().copied()) {
                        id
                    } else {
                        rewrite(tx, parsed, &parents, None)?
                    };
                    mapped.insert(id, raw);
                    active.remove(&id);
                }
            }
        }
        Ok(Normalized {
            tips: BTreeMap::from([(mapped[&tip], tip)]),
            pairs: mapped.into_iter().map(|(input, raw)| (raw, input)).collect(),
        })
    }

    /// Copy a complete canonical closure after the source transaction is flushed.
    /// A destination-present object is assumed to already have a complete closure.
    pub fn copy_complete_to(&self, destination: &gix::Repository, tips: &[ObjectId]) -> Result<()> {
        ensure!(self.git.object_hash() == destination.object_hash(), "Object format mismatch");
        copy_objects(&self.git, Some(destination), tips, &HashSet::new(), true)
    }

    fn cache_path(main: &gix::Repository, endpoint: &str) -> Result<PathBuf> {
        let key = gix_object::compute_hash(main.object_hash(), gix_object::Kind::Blob, endpoint.as_bytes())?;
        Ok(main.git_dir().join("jjosh/sources").join(key.to_string()))
    }

    fn check_transaction(&self, tx: &Transaction) -> Result<()> {
        ensure!(tx.repo().git_dir().canonicalize()? == self.path().canonicalize()?, "Source transaction must be opened on the endpoint repository");
        Ok(())
    }

    fn boundaries(&self) -> Result<HashSet<ObjectId>> {
        // Read directly so permission/stat failures cannot masquerade as unshallow.
        let bytes = match fs::read(self.path().join("shallow")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
            Err(error) => return Err(error).context("Reading source shallow boundaries"),
        };
        bytes.lines().map(|line| {
            let id = ObjectId::from_hex(line).context("Invalid source shallow boundary")?;
            ensure!(id.kind() == self.git.object_hash(), "Shallow boundary object format mismatch");
            Ok(id)
        }).collect()
    }

    /// Old normalized roots still witness cut edges after the live depth changes.
    /// This union is only for retaining raw objects, never for new normalization.
    fn retention_boundaries(&self, tx: &Transaction) -> Result<HashSet<ObjectId>> {
        let mut boundaries = self.boundaries()?;
        for (input, raw) in self.persisted_inverse()? {
            if CommitData::read(tx.odb(), input)?.parsed()?.parents().next().is_none() {
                boundaries.insert(raw);
            }
        }
        Ok(boundaries)
    }

    fn persisted_inverse(&self) -> Result<BTreeMap<ObjectId, ObjectId>> {
        let platform = self.git.references()?;
        let mut inverse = BTreeMap::new();
        let mut retained = BTreeSet::new();
        for reference in platform.prefixed(MAP_PREFIX)? {
            let reference = reference.map_err(anyhow::Error::from_boxed)?;
            let name = std::str::from_utf8(reference.name().as_bstr())?;
            let suffix = name.strip_prefix(MAP_PREFIX).context("Invalid normalization ref namespace")?;
            let (input, kind) = suffix.split_once('/').context("Invalid normalization ref name")?;
            let input = ObjectId::from_hex(input.as_bytes())?;
            let gix::refs::TargetRef::Object(target) = reference.target() else {
                bail!("Normalization ref {name} must not be symbolic");
            };
            match kind {
                "raw" => insert_mapping(&mut inverse, input, target.to_owned())?,
                "normalized" => {
                    ensure!(target == input.as_ref(), "Normalization ref {name} has an inconsistent target");
                    retained.insert(input);
                }
                _ => bail!("Invalid normalization ref {name}"),
            }
        }
        ensure!(inverse.keys().copied().collect::<BTreeSet<_>>() == retained, "Incomplete persisted normalization mapping");
        Ok(inverse)
    }
}


/// A short-lived object lookup view for one publication spanning several sources.
/// It owns no refs or source history; the transport consumes it into a complete pack.
pub(crate) fn transport_repository(
    main: &gix::Repository,
    sources: &[&gix::Repository],
) -> Result<(tempfile::TempDir, gix::Repository)> {
    let directory = tempfile::tempdir()?;
    let repository = gix::init_bare(directory.path())?;
    let mut paths = BTreeSet::new();
    for source in std::iter::once(main).chain(sources.iter().copied()) {
        paths.insert(source.objects.store_ref().path().canonicalize()?);
    }
    let mut alternates = Vec::new();
    for path in paths {
        alternates.extend(quoted_path(&path));
        alternates.push(b'\n');
    }
    fs::create_dir_all(repository.git_dir().join("objects/info"))?;
    fs::write(repository.git_dir().join("objects/info/alternates"), alternates)?;
    drop(repository);
    let repository = gix::open(directory.path())?;
    Ok((directory, repository))
}
enum CommitWork {
    Visit(ObjectId),
    Finish(CommitData),
}

fn insert_mapping(inverse: &mut BTreeMap<ObjectId, ObjectId>, input: ObjectId, raw: ObjectId) -> Result<()> {
    if let Some(previous) = inverse.insert(input, raw) {
        ensure!(previous == raw, "Ambiguous source normalization for {input}: {previous} and {raw}");
    }
    Ok(())
}

fn rewrite(tx: &Transaction, commit: gix_object::CommitRef<'_>, parents: &[ObjectId], raw: Option<ObjectId>) -> Result<ObjectId> {
    let parent_hex: Vec<_> = parents.iter().map(ToString::to_string).collect();
    let mut commit = gix_object::CommitRef {
        parents: parent_hex.iter().map(|hex| hex.as_bytes().as_bstr()).collect(),
        ..commit
    };
    // Signatures no longer attest this commit. Mergetags describe the original
    // parent relationship, which is also no longer the relationship being written.
    commit.extra_headers.retain(|(key, _)| {
        *key != PROVENANCE && *key != b"gpgsig" && *key != b"gpgsig-sha256" && *key != b"mergetag"
    });
    if let Some(raw) = raw {
        commit.extra_headers.push((PROVENANCE.as_bstr(), Cow::Owned(raw.to_string().into())));
    }
    gix_object::Write::write(tx.odb(), &commit).map_err(|error| anyhow::anyhow!("Writing source history commit: {error}"))
}

/// Quote both config paths and alternates with Git's C-style path quoting.
fn quoted_path(path: &Path) -> Vec<u8> {
    let path = gix::path::into_bstr(path);
    let mut quoted = Vec::with_capacity(path.len() + 2);
    quoted.push(b'"');
    for &byte in path.iter() {
        match byte {
            b'"' | b'\\' => { quoted.push(b'\\'); quoted.push(byte); }
            b'\n' => quoted.extend_from_slice(b"\\n"),
            b'\t' => quoted.extend_from_slice(b"\\t"),
            _ => quoted.push(byte),
        }
    }
    quoted.push(b'"');
    quoted
}

enum ObjectWork {
    Visit(ObjectId, Option<gix_object::Kind>),
    Finish(ObjectId, gix_object::Kind, Vec<u8>),
}

/// Children are written first: a failed copy cannot leave a destination root
/// whose incomplete closure a subsequent copy would incorrectly skip.
fn copy_objects(source: &impl gix_object::Find, destination: Option<&gix::Repository>, tips: &[ObjectId], shallow: &HashSet<ObjectId>, skip_present: bool) -> Result<()> {
    let mut complete = HashMap::new();
    let mut active = HashSet::new();
    let mut pending: Vec<_> = tips.iter().rev().map(|id| ObjectWork::Visit(*id, None)).collect();
    let mut destination_buffer = Vec::new();
    while let Some(work) = pending.pop() {
        match work {
            ObjectWork::Visit(id, expected) => {
                if let Some(&kind) = complete.get(&id) {
                    ensure!(expected.is_none_or(|expected| expected == kind), "Object {id} has an inconsistent type");
                    continue;
                }
                if skip_present
                    && let Some(destination) = destination
                    && let Some(object) = destination.objects.try_find(&id, &mut destination_buffer).map_err(|error| anyhow::anyhow!("Reading destination object {id}: {error}"))?
                {
                    ensure!(expected.is_none_or(|expected| expected == object.kind), "Destination object {id} has an inconsistent type");
                    complete.insert(id, object.kind);
                    continue;
                }
                ensure!(active.insert(id), "Cycle in object graph at {id}");
                let mut bytes = Vec::new();
                let object = source.try_find(&id, &mut bytes).map_err(|error| anyhow::anyhow!("Reading source object {id}: {error}"))?.with_context(|| format!("Missing source object {id}"))?;
                let kind = object.kind;
                ensure!(expected.is_none_or(|expected| expected == kind), "Source object {id} has an inconsistent type");
                let mut children = Vec::new();
                match kind {
                    gix_object::Kind::Commit => {
                        let commit = gix_object::CommitRef::from_bytes(object.data, id.kind())?;
                        children.push((commit.tree(), Some(gix_object::Kind::Tree)));
                        if !shallow.contains(&id) {
                            children.extend(commit.parents().map(|parent| (parent, Some(gix_object::Kind::Commit))));
                        }
                    }
                    gix_object::Kind::Tree => {
                        for entry in gix_object::TreeRefIter::from_bytes(object.data, id.kind()) {
                            let entry = entry?;
                            let kind = match entry.mode.kind() {
                                gix_object::tree::EntryKind::Commit => continue,
                                gix_object::tree::EntryKind::Tree => gix_object::Kind::Tree,
                                _ => gix_object::Kind::Blob,
                            };
                            children.push((entry.oid.to_owned(), Some(kind)));
                        }
                    }
                    gix_object::Kind::Tag => {
                        let tag = gix_object::TagRef::from_bytes(object.data, id.kind())?;
                        children.push((tag.target(), Some(tag.target_kind)));
                    }
                    gix_object::Kind::Blob => {}
                }
                // A validation-only walk need not retain object bytes on its stack.
                if destination.is_none() {
                    bytes = Vec::new();
                }
                pending.push(ObjectWork::Finish(id, kind, bytes));
                pending.extend(children.into_iter().rev().map(|(id, kind)| ObjectWork::Visit(id, kind)));
            }
            ObjectWork::Finish(id, kind, bytes) => {
                if let Some(destination) = destination {
                    destination.objects.write_buf_with_known_id(kind, &bytes, id).map_err(|error| anyhow::anyhow!("Retaining source object {id}: {error}"))?;
                }
                active.remove(&id);
                complete.insert(id, kind);
            }
        }
    }
    Ok(())
}
