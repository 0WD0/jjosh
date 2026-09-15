use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use gix::ObjectId;
use gix::bstr::ByteSlice as _;
use gix::refs::transaction::{PreviousValue, RefEdit};
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
    let (endpoint, input): (String, String) =
        serde_json::from_str(value).context("Invalid source generation witness")?;
    Ok((endpoint, ObjectId::from_hex(input.as_bytes())?))
}

impl SourceRepo {
    pub fn open(main: &gix::Repository, endpoint: &str) -> Result<Self> {
        Self::install_new(main, endpoint, |_| Ok(()))?;
        let source = Self::open_existing(main, endpoint)?
            .context("Initialized source repository is missing")?;
        source.repair_normalization()?;
        Ok(source)
    }

    /// Populate a configured staging repository before publishing a new cache.
    /// Returns false if an existing or concurrently installed cache must be merged.
    pub(crate) fn install_new(
        main: &gix::Repository,
        endpoint: &str,
        populate: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<bool> {
        ensure!(
            main.object_hash() == gix::hash::Kind::Sha1,
            "Source projection requires SHA-1"
        );
        let path = Self::cache_path(main, endpoint)?;
        if path.try_exists()? {
            return Ok(false);
        }
        let parent = path
            .parent()
            .context("Source cache has no parent directory")?;
        fs::create_dir_all(parent)?;
        let temporary = tempfile::tempdir_in(parent)?;
        let initial = gix::init_bare(temporary.path())?;
        let config_path = main
            .config_path(gix::config::Source::Local)?
            .canonicalize()?;
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
        let staged = Self::open_at(main, temporary.path())?;
        populate(&staged)?;
        drop(staged);
        if path.try_exists()? {
            return Ok(false);
        }
        if let Err(error) = fs::rename(temporary.path(), &path) {
            // A configured repository is nonempty, so rename cannot replace a
            // concurrently installed cache. Its evidence must be checked instead.
            if path.try_exists()? {
                return Ok(false);
            }
            return Err(error).context("Installing source repository");
        }
        Ok(true)
    }

    fn open_at(main: &gix::Repository, path: &Path) -> Result<Self> {
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
        ensure!(
            git.object_hash() == main.object_hash(),
            "Source cache object format mismatch"
        );
        Ok(Self { git })
    }

    /// Open existing evidence without initializing or configuring the source.
    pub(crate) fn open_existing(main: &gix::Repository, endpoint: &str) -> Result<Option<Self>> {
        let path = Self::cache_path(main, endpoint)?;
        if !path.try_exists()? {
            return Ok(None);
        }
        Self::open_at(main, &path).map(Some)
    }

    /// Historical cut edges remain necessary after a source has been deepened.
    pub(crate) fn import_boundaries(&self) -> Result<HashSet<ObjectId>> {
        let mut boundaries = self.boundaries()?;
        for (input, raw) in self.persisted_inverse()? {
            let object = self.git.find_object(input)?;
            ensure!(
                object.kind == gix_object::Kind::Commit,
                "Normalization input is not a commit"
            );
            ensure!(
                self.git.find_object(raw)?.kind == gix_object::Kind::Commit,
                "Normalization raw original is not a commit"
            );
            if gix_object::CommitRef::from_bytes(&object.data, input.kind())?
                .parents()
                .next()
                .is_none()
            {
                boundaries.insert(raw);
            }
        }
        Ok(boundaries)
    }

    /// Check an observation against retained provenance. The import capture
    /// validates all retained complete-generation closures in one shared walk.
    pub(crate) fn verify_import_generation_witness(
        &self,
        raw: ObjectId,
        input: ObjectId,
    ) -> Result<()> {
        let inverse = self.persisted_inverse()?;
        ensure!(
            inverse.get(&input).copied().unwrap_or(input) == raw,
            "Source generation does not identify the observed raw input"
        );
        let name = format!("refs/jjosh/generations/{input}");
        let reference = self
            .git
            .find_reference(name.as_str())
            .context("Observed source generation is unavailable")?;
        ensure!(
            reference.target().try_id() == Some(input.as_ref()),
            "Source generation retention was modified"
        );
        Ok(())
    }

    pub fn git(&self) -> &gix::Repository {
        &self.git
    }

    pub fn path(&self) -> &Path {
        self.git.git_dir()
    }

    /// Recover the exact complete input graph witnessed by an older operation.
    pub fn witnessed_generation(
        &self,
        tx: &Transaction,
        raw: ObjectId,
        input: ObjectId,
    ) -> Result<Normalized> {
        self.check_transaction(tx)?;
        let inverse = self.persisted_inverse()?;
        ensure!(
            inverse.get(&input).copied().unwrap_or(input) == raw,
            "Source generation does not identify the observed raw input"
        );
        let retained = tx.resolve_ref(&format!("refs/jjosh/generations/{input}"))?;
        ensure!(
            retained == Some(input),
            "Observed source generation is unavailable; fetch cannot replace historical conversion evidence"
        );
        let mut visited = HashSet::new();
        let mut pending = vec![input];
        let mut pairs = Vec::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            let commit = CommitData::read(tx.odb(), id)?;
            pending.extend(commit.parsed()?.parents());
            pairs.push((inverse.get(&id).copied().unwrap_or(id), id));
        }
        copy_objects(tx.odb(), None, &[input], &HashSet::new(), false)?;
        Ok(Normalized {
            tips: BTreeMap::from([(raw, input)]),
            pairs,
        })
    }

    pub fn retain_observations(&self, tx: &Transaction, objects: &[ObjectId]) -> Result<()> {
        self.retain_raw(tx, objects)?;
        for id in objects {
            let name = format!("refs/jjosh/observed-objects/{id}");
            let old = tx.resolve_ref(&name)?;
            ensure!(
                old.is_none_or(|old| old == *id),
                "Observed object retention was modified"
            );
            tx.update_ref(
                &name,
                old.map_or(
                    josh_core::cache::Expected::Absent,
                    josh_core::cache::Expected::At,
                ),
                *id,
                "retain immutable source observation",
            )?;
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
            tx.update_ref(
                &name,
                josh_core::cache::Expected::Absent,
                id,
                "isolate source context",
            )?;
        }
        let marker = josh_core::objects::write_blob(tx.odb(), b"source-context-v1\n")?;
        tx.update_ref(
            INITIALIZED_REF,
            josh_core::cache::Expected::Absent,
            marker,
            "initialize isolated source",
        )?;
        tx.flush_mem_odb()
    }

    /// Source refs must remain valid even if canonical objects are later collected.
    pub fn retain_raw(&self, tx: &Transaction, tips: &[ObjectId]) -> Result<()> {
        self.check_transaction(tx)?;
        self.repair_normalization()?;
        copy_objects(
            tx.odb(),
            Some(&self.git),
            tips,
            &self.retention_boundaries(tx)?,
            false,
        )
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
        copy_objects(
            tx.odb(),
            Some(&self.git),
            &complete_roots,
            &HashSet::new(),
            false,
        )?;
        for input in &complete_roots {
            let name = format!("refs/jjosh/generations/{input}");
            let old = tx.resolve_ref(&name)?;
            ensure!(
                old.is_none_or(|old| old == *input),
                "Source generation retention was modified"
            );
            tx.update_ref(
                &name,
                old.map_or(
                    josh_core::cache::Expected::Absent,
                    josh_core::cache::Expected::At,
                ),
                *input,
                "retain source normalization generation",
            )?;
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
        let roots: Vec<_> = inverse
            .iter()
            .flat_map(|(&input, &raw)| [input, raw])
            .collect();
        // Do not skip alternates here: source retention must not rely on a main
        // repository GC retaining objects only reachable from source-side refs.
        copy_objects(
            tx.odb(),
            Some(&self.git),
            &roots,
            &self.retention_boundaries(tx)?,
            false,
        )?;
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
    pub fn denormalize(
        &self,
        tx: &Transaction,
        tip: ObjectId,
        normalized: &Normalized,
    ) -> Result<Normalized> {
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
                    let has_provenance = parsed
                        .extra_headers
                        .iter()
                        .any(|(key, _)| *key == PROVENANCE);
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
            pairs: mapped
                .into_iter()
                .map(|(input, raw)| (raw, input))
                .collect(),
        })
    }

    /// Copy a complete canonical closure after the source transaction is flushed.
    /// A destination-present object is assumed to already have a complete closure.
    pub fn copy_complete_to(&self, destination: &gix::Repository, tips: &[ObjectId]) -> Result<()> {
        ensure!(
            self.git.object_hash() == destination.object_hash(),
            "Object format mismatch"
        );
        copy_objects(&self.git, Some(destination), tips, &HashSet::new(), true)
    }

    fn cache_path(main: &gix::Repository, endpoint: &str) -> Result<PathBuf> {
        let key = gix_object::compute_hash(
            main.object_hash(),
            gix_object::Kind::Blob,
            endpoint.as_bytes(),
        )?;
        Ok(main.git_dir().join("jjosh/sources").join(key.to_string()))
    }

    fn check_transaction(&self, tx: &Transaction) -> Result<()> {
        ensure!(
            tx.repo().git_dir().canonicalize()? == self.path().canonicalize()?,
            "Source transaction must be opened on the endpoint repository"
        );
        Ok(())
    }

    fn boundaries(&self) -> Result<HashSet<ObjectId>> {
        // Read directly so permission/stat failures cannot masquerade as unshallow.
        let bytes = match fs::read(self.path().join("shallow")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
            Err(error) => return Err(error).context("Reading source shallow boundaries"),
        };
        bytes
            .lines()
            .map(|line| {
                let id = ObjectId::from_hex(line).context("Invalid source shallow boundary")?;
                ensure!(
                    id.kind() == self.git.object_hash(),
                    "Shallow boundary object format mismatch"
                );
                Ok(id)
            })
            .collect()
    }

    /// Old normalized roots still witness cut edges after the live depth changes.
    /// This union is only for retaining raw objects, never for new normalization.
    fn retention_boundaries(&self, tx: &Transaction) -> Result<HashSet<ObjectId>> {
        let mut boundaries = self.boundaries()?;
        for (input, raw) in self.persisted_inverse()? {
            if CommitData::read(tx.odb(), input)?
                .parsed()?
                .parents()
                .next()
                .is_none()
            {
                boundaries.insert(raw);
            }
        }
        Ok(boundaries)
    }

    /// Complete only provable interrupted pair writes, before a writer needs the
    /// strict inverse index. Opening existing evidence for capture never calls this.
    fn repair_normalization(&self) -> Result<()> {
        let edits = self.normalization_repairs()?;
        if !edits.is_empty() {
            self.git.edit_references(edits)?;
        }
        Ok(())
    }

    fn normalization_repairs(&self) -> Result<Vec<RefEdit>> {
        let (mut inverse, retained) = self.normalization_refs()?;
        if inverse.keys().copied().collect::<BTreeSet<_>>() == retained {
            return Ok(Vec::new());
        }
        let inputs: BTreeSet<_> = inverse.keys().copied().chain(retained.iter().copied()).collect();
        let raw_refs: BTreeSet<_> = inverse.keys().copied().collect();
        // Exported descendants need not carry a provenance header. Reconstruct
        // their raw IDs from retained parent relations, children only after parents.
        let mut pending: Vec<_> = retained.difference(&raw_refs).map(|&input| (input, false)).collect();
        let mut active = HashSet::new();
        while let Some((input, finish)) = pending.pop() {
            if inverse.contains_key(&input) {
                continue;
            }
            let object = self.git.find_object(input)?;
            ensure!(object.kind == gix_object::Kind::Commit, "Normalization input is not a commit");
            let commit = gix_object::CommitRef::from_bytes(&object.data, input.kind())?;
            let mut provenance = commit.extra_headers.iter().filter(|(key, _)| *key == PROVENANCE);
            if let Some((_, value)) = provenance.next() {
                ensure!(provenance.next().is_none(), "Ambiguous source normalization provenance");
                inverse.insert(input, ObjectId::from_hex(value.as_ref())?);
            } else if finish {
                let parents: Vec<_> = commit.parents()
                    .map(|parent| inverse.get(&parent).copied().unwrap_or(parent))
                    .collect();
                ensure!(
                    !commit.parents().eq(parents.iter().copied()),
                    "Missing raw normalization ref has no retained provenance or changed parent relation"
                );
                let parent_hex: Vec<_> = parents.iter().map(ToString::to_string).collect();
                let raw_commit = rewritten_commit(commit, &parent_hex, None);
                let mut bytes = Vec::new();
                gix_object::WriteTo::write_to(&raw_commit, &mut bytes)?;
                let raw = gix_object::compute_hash(input.kind(), gix_object::Kind::Commit, &bytes)?;
                inverse.insert(input, raw);
                active.remove(&input);
            } else {
                ensure!(active.insert(input), "Cycle in source normalization provenance");
                pending.push((input, true));
                pending.extend(commit.parents().filter(|parent| inputs.contains(parent) && !inverse.contains_key(parent)).map(|parent| (parent, false)));
            }
        }
        let mut edits = Vec::new();
        for &input in &inputs {
            let raw = inverse[&input];
            ensure!(input != raw && raw.kind() == self.git.object_hash(), "Invalid source normalization identity");
            for (suffix, target, exists) in [
                ("raw", raw, raw_refs.contains(&input)),
                ("normalized", input, retained.contains(&input)),
            ] {
                let name = format!("{MAP_PREFIX}{input}/{suffix}").try_into()?;
                edits.push(if exists {
                    RefEdit::verify(name, PreviousValue::MustExistAndMatch(target.into()))
                } else {
                    RefEdit::update(name, target, PreviousValue::MustNotExist, "repair source normalization retention")
                });
            }
        }

        let mut boundaries = HashSet::new();
        let mut identity_parents = BTreeSet::new();
        for (&input, &raw) in &inverse {
            let input_object = self.git.find_object(input)?;
            let raw_object = self.git.find_object(raw)?;
            ensure!(
                input_object.kind == gix_object::Kind::Commit && raw_object.kind == gix_object::Kind::Commit,
                "Normalization relation must identify commits"
            );
            let input_commit = gix_object::CommitRef::from_bytes(&input_object.data, input.kind())?;
            let raw_commit = gix_object::CommitRef::from_bytes(&raw_object.data, raw.kind())?;
            let parents: Vec<_> = input_commit.parents().map(|parent| {
                inverse.get(&parent).copied().unwrap_or_else(|| {
                    identity_parents.insert(parent);
                    parent
                })
            }).collect();
            let same_parents = raw_commit.parents().eq(parents.iter().copied());

            let mut expected_input = raw_commit.clone();
            expected_input.parents = input_commit.parents.clone();
            strip_rewritten_headers(&mut expected_input);
            expected_input.extra_headers.push((PROVENANCE.as_bstr(), Cow::Owned(raw.to_string().into())));
            let normalized = expected_input == input_commit
                && (input_commit.parents.is_empty() || same_parents);

            let mut expected_raw = input_commit.clone();
            expected_raw.parents = raw_commit.parents.clone();
            strip_rewritten_headers(&mut expected_raw);
            ensure!(
                normalized || (same_parents && expected_raw == raw_commit),
                "Contradictory source normalization relation for {input}"
            );
            if input_commit.parents.is_empty() {
                boundaries.insert(raw);
            }
        }
        // Identity edges also participate in the proof: a concurrently introduced
        // mapping must not change the parent relation after it was checked.
        for parent in identity_parents {
            for suffix in ["raw", "normalized"] {
                edits.push(RefEdit::verify(
                    format!("{MAP_PREFIX}{parent}/{suffix}").try_into()?,
                    PreviousValue::MustNotExist,
                ));
            }
        }
        copy_objects(&self.git, None, &inputs.into_iter().collect::<Vec<_>>(), &HashSet::new(), false)?;
        copy_objects(&self.git, None, &inverse.values().copied().collect::<Vec<_>>(), &boundaries, false)?;
        Ok(edits)
    }

    fn persisted_inverse(&self) -> Result<BTreeMap<ObjectId, ObjectId>> {
        let (inverse, retained) = self.normalization_refs()?;
        ensure!(
            inverse.keys().copied().collect::<BTreeSet<_>>() == retained,
            "Incomplete persisted normalization mapping"
        );
        Ok(inverse)
    }

    fn normalization_refs(&self) -> Result<(BTreeMap<ObjectId, ObjectId>, BTreeSet<ObjectId>)> {
        let platform = self.git.references()?;
        let mut inverse = BTreeMap::new();
        let mut retained = BTreeSet::new();
        for reference in platform.prefixed(MAP_PREFIX)? {
            let reference = reference.map_err(anyhow::Error::from_boxed)?;
            let name = std::str::from_utf8(reference.name().as_bstr())?;
            let suffix = name
                .strip_prefix(MAP_PREFIX)
                .context("Invalid normalization ref namespace")?;
            let (input, kind) = suffix
                .split_once('/')
                .context("Invalid normalization ref name")?;
            let input = ObjectId::from_hex(input.as_bytes())?;
            ensure!(input.kind() == self.git.object_hash() && !input.is_null(), "Invalid normalization input identity");
            let gix::refs::TargetRef::Object(target) = reference.target() else {
                bail!("Normalization ref {name} must not be symbolic");
            };
            match kind {
                "raw" => insert_mapping(&mut inverse, input, target.to_owned())?,
                "normalized" => {
                    ensure!(
                        target == input.as_ref(),
                        "Normalization ref {name} has an inconsistent target"
                    );
                    retained.insert(input);
                }
                _ => bail!("Invalid normalization ref {name}"),
            }
        }
        Ok((inverse, retained))
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
    fs::write(
        repository.git_dir().join("objects/info/alternates"),
        alternates,
    )?;
    drop(repository);
    let repository = gix::open(directory.path())?;
    Ok((directory, repository))
}
enum CommitWork {
    Visit(ObjectId),
    Finish(CommitData),
}

fn insert_mapping(
    inverse: &mut BTreeMap<ObjectId, ObjectId>,
    input: ObjectId,
    raw: ObjectId,
) -> Result<()> {
    if let Some(previous) = inverse.insert(input, raw) {
        ensure!(
            previous == raw,
            "Ambiguous source normalization for {input}: {previous} and {raw}"
        );
    }
    Ok(())
}

fn rewrite(
    tx: &Transaction,
    commit: gix_object::CommitRef<'_>,
    parents: &[ObjectId],
    raw: Option<ObjectId>,
) -> Result<ObjectId> {
    let parent_hex: Vec<_> = parents.iter().map(ToString::to_string).collect();
    let commit = rewritten_commit(commit, &parent_hex, raw);
    gix_object::Write::write(tx.odb(), &commit)
        .map_err(|error| anyhow::anyhow!("Writing source history commit: {error}"))
}

fn rewritten_commit<'a>(
    commit: gix_object::CommitRef<'a>,
    parent_hex: &'a [String],
    raw: Option<ObjectId>,
) -> gix_object::CommitRef<'a> {
    let mut commit = gix_object::CommitRef {
        parents: parent_hex
            .iter()
            .map(|hex| hex.as_bytes().as_bstr())
            .collect(),
        ..commit
    };
    strip_rewritten_headers(&mut commit);
    if let Some(raw) = raw {
        commit
            .extra_headers
            .push((PROVENANCE.as_bstr(), Cow::Owned(raw.to_string().into())));
    }
    commit
}

fn strip_rewritten_headers(commit: &mut gix_object::CommitRef<'_>) {
    // Signatures no longer attest this commit. Mergetags describe the original
    // parent relationship, which is also no longer the relationship being written.
    commit.extra_headers.retain(|(key, _)| {
        *key != PROVENANCE && *key != b"gpgsig" && *key != b"gpgsig-sha256" && *key != b"mergetag"
    });
}

/// Quote both config paths and alternates with Git's C-style path quoting.
fn quoted_path(path: &Path) -> Vec<u8> {
    let path = gix::path::into_bstr(path);
    let mut quoted = Vec::with_capacity(path.len() + 2);
    quoted.push(b'"');
    for &byte in path.iter() {
        match byte {
            b'"' | b'\\' => {
                quoted.push(b'\\');
                quoted.push(byte);
            }
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
pub(crate) fn copy_objects(
    source: &impl gix_object::Find,
    destination: Option<&gix::Repository>,
    tips: &[ObjectId],
    shallow: &HashSet<ObjectId>,
    skip_present: bool,
) -> Result<()> {
    let mut complete = HashMap::new();
    let mut active = HashSet::new();
    let mut pending: Vec<_> = tips
        .iter()
        .rev()
        .map(|id| ObjectWork::Visit(*id, None))
        .collect();
    let mut destination_buffer = Vec::new();
    while let Some(work) = pending.pop() {
        match work {
            ObjectWork::Visit(id, expected) => {
                if let Some(&kind) = complete.get(&id) {
                    ensure!(
                        expected.is_none_or(|expected| expected == kind),
                        "Object {id} has an inconsistent type"
                    );
                    continue;
                }
                if skip_present
                    && let Some(destination) = destination
                    && let Some(object) = destination
                        .objects
                        .try_find(&id, &mut destination_buffer)
                        .map_err(|error| {
                            anyhow::anyhow!("Reading destination object {id}: {error}")
                        })?
                {
                    ensure!(
                        expected.is_none_or(|expected| expected == object.kind),
                        "Destination object {id} has an inconsistent type"
                    );
                    complete.insert(id, object.kind);
                    continue;
                }
                ensure!(active.insert(id), "Cycle in object graph at {id}");
                let mut bytes = Vec::new();
                let object = source
                    .try_find(&id, &mut bytes)
                    .map_err(|error| anyhow::anyhow!("Reading source object {id}: {error}"))?
                    .with_context(|| format!("Missing source object {id}"))?;
                let kind = object.kind;
                ensure!(
                    expected.is_none_or(|expected| expected == kind),
                    "Source object {id} has an inconsistent type"
                );
                let mut children = Vec::new();
                match kind {
                    gix_object::Kind::Commit => {
                        let commit = gix_object::CommitRef::from_bytes(object.data, id.kind())?;
                        children.push((commit.tree(), Some(gix_object::Kind::Tree)));
                        if !shallow.contains(&id) {
                            children.extend(
                                commit
                                    .parents()
                                    .map(|parent| (parent, Some(gix_object::Kind::Commit))),
                            );
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
                pending.extend(
                    children
                        .into_iter()
                        .rev()
                        .map(|(id, kind)| ObjectWork::Visit(id, kind)),
                );
            }
            ObjectWork::Finish(id, kind, bytes) => {
                if let Some(destination) = destination {
                    destination
                        .objects
                        .write_buf_with_known_id(kind, &bytes, id)
                        .map_err(|error| {
                            anyhow::anyhow!("Retaining source object {id}: {error}")
                        })?;
                }
                active.remove(&id);
                complete.insert(id, kind);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalization_fixture() -> Result<(tempfile::TempDir, gix::Repository, SourceRepo, ObjectId, ObjectId)> {
        let temporary = tempfile::tempdir()?;
        let main = gix::init_bare(temporary.path().join("main.git"))?;
        let source = SourceRepo::open(&main, "test-source")?;
        let tree = source.git.write_buf(gix_object::Kind::Tree, b"")?;
        let parent = source.git.write_buf(
            gix_object::Kind::Commit,
            format!("tree {tree}\nauthor A <a@example.org> 1 +0000\ncommitter A <a@example.org> 1 +0000\n\nparent\n").as_bytes(),
        )?;
        let raw = source.git.write_buf(
            gix_object::Kind::Commit,
            format!("tree {tree}\nparent {parent}\nauthor A <a@example.org> 2 +0000\ncommitter A <a@example.org> 2 +0000\ngpgsig retained signature\n\nraw\n").as_bytes(),
        )?;
        fs::write(source.path().join("shallow"), format!("{raw}\n"))?;
        let tx = crate::interop::open_josh_transaction(source.path(), false)
            .map_err(|error| anyhow::anyhow!(error.error))?;
        let normalized = source.normalize(&tx, &[raw])?;
        let input = normalized.tips[&raw];
        source.record_normalized(&tx, &normalized)?;
        tx.flush_mem_odb()?;
        drop(tx);
        Ok((temporary, main, source, raw, input))
    }

    fn remove_partner(source: &SourceRepo, input: ObjectId, suffix: &str, target: ObjectId) -> Result<()> {
        source.git.edit_reference(RefEdit::delete(
            format!("{MAP_PREFIX}{input}/{suffix}").try_into()?,
            PreviousValue::MustExistAndMatch(target.into()),
        ))?;
        Ok(())
    }

    #[test]
    fn writer_repairs_either_missing_partner_without_changing_historical_bytes() -> Result<()> {
        let (_temporary, main, source, raw, input) = normalization_fixture()?;
        let bytes = source.git.find_object(raw)?.data.to_vec();
        // Deepening must not erase the historical cut used to prove this pair.
        fs::remove_file(source.path().join("shallow"))?;
        for (suffix, target) in [("normalized", input), ("raw", raw)] {
            remove_partner(&source, input, suffix, target)?;
            let captured = SourceRepo::open_existing(&main, "test-source")?.unwrap();
            assert!(captured.import_boundaries().is_err());
            assert!(source.git.try_find_reference(format!("{MAP_PREFIX}{input}/{suffix}"))?.is_none());

            let writer = SourceRepo::open(&main, "test-source")?;
            assert_eq!(writer.persisted_inverse()?, BTreeMap::from([(input, raw)]));
            assert_eq!(writer.git.find_object(raw)?.data, bytes);
            assert_eq!(writer.git.find_reference(format!("refs/jjosh/generations/{input}"))?.id().detach(), input);
            let tx = crate::interop::open_josh_transaction(writer.path(), false)
                .map_err(|error| anyhow::anyhow!(error.error))?;
            assert_eq!(writer.witnessed_generation(&tx, raw, input)?.tips[&raw], input);
            writer.retain_raw(&tx, &[raw])?;
        }
        Ok(())
    }

    #[test]
    fn repair_rejects_contradictory_or_missing_raw_objects() -> Result<()> {
        let (_temporary, main, source, raw, input) = normalization_fixture()?;
        remove_partner(&source, input, "normalized", input)?;
        let raw_ref = format!("{MAP_PREFIX}{input}/raw");
        let unrelated = source.git.find_object(raw)?.into_commit().parent_ids().next().unwrap().detach();
        source.git.reference(raw_ref.as_str(), unrelated, PreviousValue::MustExistAndMatch(raw.into()), "conflicting writer")?;
        assert!(SourceRepo::open(&main, "test-source").is_err());
        assert!(source.git.try_find_reference(format!("{MAP_PREFIX}{input}/normalized"))?.is_none());
        assert_eq!(source.git.find_reference(raw_ref.as_str())?.id().detach(), unrelated);

        let missing = ObjectId::from_hex(b"1111111111111111111111111111111111111111")?;
        source.git.reference(raw_ref.as_str(), missing, PreviousValue::MustExistAndMatch(unrelated.into()), "missing raw object")?;
        assert!(SourceRepo::open(&main, "test-source").is_err());
        assert!(source.git.try_find_reference(format!("{MAP_PREFIX}{input}/normalized"))?.is_none());
        assert_eq!(source.git.find_reference(raw_ref.as_str())?.id().detach(), missing);
        Ok(())
    }

    #[test]
    fn repair_verifies_surviving_witness_and_does_not_absorb_concurrent_creation() -> Result<()> {
        let (_temporary, _main, source, raw, input) = normalization_fixture()?;
        remove_partner(&source, input, "normalized", input)?;
        let edits = source.normalization_repairs()?;
        remove_partner(&source, input, "raw", raw)?;
        assert!(source.git.edit_references(edits).is_err());
        assert!(source.git.try_find_reference(format!("{MAP_PREFIX}{input}/normalized"))?.is_none());

        source.git.reference(format!("{MAP_PREFIX}{input}/raw"), raw, PreviousValue::MustNotExist, "restore test witness")?;
        let edits = source.normalization_repairs()?;
        source.git.reference(format!("{MAP_PREFIX}{input}/normalized"), input, PreviousValue::MustNotExist, "concurrent repair")?;
        assert!(source.git.edit_references(edits).is_err());
        assert_eq!(source.persisted_inverse()?, BTreeMap::from([(input, raw)]));
        Ok(())
    }
    #[test]
    fn missing_raw_partner_without_provenance_is_not_guessed() -> Result<()> {
        let (_temporary, main, source, raw, _input) = normalization_fixture()?;
        source.git.reference(
            format!("{MAP_PREFIX}{raw}/normalized"),
            raw,
            PreviousValue::MustNotExist,
            "unproven normalization input",
        )?;
        assert!(SourceRepo::open(&main, "test-source").is_err());
        assert!(source.git.try_find_reference(format!("{MAP_PREFIX}{raw}/raw"))?.is_none());
        Ok(())
    }

    #[test]
    fn writer_reconstructs_exported_raw_partners_from_retained_parent_relations() -> Result<()> {
        let (_temporary, main, source, boundary_raw, boundary_input) = normalization_fixture()?;
        let tree = source.git.find_object(boundary_input)?.into_commit().tree_id()?.detach();
        let child = source.git.write_buf(
            gix_object::Kind::Commit,
            format!("tree {tree}\nparent {boundary_input}\nauthor A <a@example.org> 3 +0000\ncommitter A <a@example.org> 3 +0000\ngpgsig export signature\n\nchild\n").as_bytes(),
        )?;
        let tip = source.git.write_buf(
            gix_object::Kind::Commit,
            format!("tree {tree}\nparent {child}\nauthor A <a@example.org> 4 +0000\ncommitter A <a@example.org> 4 +0000\n\ntip\n").as_bytes(),
        )?;
        let tx = crate::interop::open_josh_transaction(source.path(), false)
            .map_err(|error| anyhow::anyhow!(error.error))?;
        let publication = source.denormalize(&tx, tip, &Normalized::default())?;
        source.record_normalized(&tx, &publication)?;
        tx.flush_mem_odb()?;
        drop(tx);
        let inverse = source.persisted_inverse()?;
        let raw_tip = inverse[&tip];
        let raw_child = inverse[&child];
        let original_bytes: Vec<_> = [boundary_raw, raw_child, raw_tip].into_iter()
            .map(|id| Ok((id, source.git.find_object(id)?.data.to_vec())))
            .collect::<Result<_>>()?;
        remove_partner(&source, child, "raw", raw_child)?;
        remove_partner(&source, tip, "raw", raw_tip)?;
        // The complete retained generation and both normalized refs survive.
        fs::remove_file(source.path().join("shallow"))?;
        let writer = SourceRepo::open(&main, "test-source")?;
        assert_eq!(writer.persisted_inverse()?, inverse);
        for (id, bytes) in original_bytes {
            assert_eq!(writer.git.find_object(id)?.data, bytes);
        }
        let tx = crate::interop::open_josh_transaction(writer.path(), false)
            .map_err(|error| anyhow::anyhow!(error.error))?;
        assert_eq!(writer.witnessed_generation(&tx, raw_tip, tip)?.tips[&raw_tip], tip);
        assert_eq!(writer.witnessed_generation(&tx, boundary_raw, boundary_input)?.tips[&boundary_raw], boundary_input);
        assert_eq!(writer.denormalize(&tx, tip, &Normalized::default())?.tips, publication.tips);
        Ok(())
    }


    #[test]
    fn interrupted_cache_population_leaves_no_published_cache_and_can_retry() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let main = gix::init_bare(temporary.path().join("main.git"))?;
        let endpoint = "https://example.invalid/source.git";
        let marker = b"source-context-v1\n";
        let interrupted = SourceRepo::install_new(&main, endpoint, |staged| {
            staged.git().write_blob(marker)?;
            anyhow::bail!("interrupted before shallow metadata and refs were installed")
        });
        assert!(interrupted.is_err());
        assert!(SourceRepo::open_existing(&main, endpoint)?.is_none());

        SourceRepo::install_new(&main, endpoint, |staged| {
            let id = staged.git().write_blob(marker)?.detach();
            staged.git().reference(
                INITIALIZED_REF,
                id,
                gix::refs::transaction::PreviousValue::MustNotExist,
                "initialize source context",
            )?;
            Ok(())
        })?;
        let installed = SourceRepo::open_existing(&main, endpoint)?
            .context("Retried cache was not published")?;
        let reference = installed.git().find_reference(INITIALIZED_REF)?;
        let object = reference.id().object()?;
        assert_eq!(object.data, marker);
        Ok(())
    }
}
