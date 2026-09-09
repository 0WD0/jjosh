use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use jj_lib::backend::Backend;
use jj_lib::backend::ChangeId;
use jj_lib::backend::CommitId;
use jj_lib::backend::TreeId;
use jj_lib::backend::TreeValue;
use jj_lib::backend::{self};
use jj_lib::git_backend::GitBackend;
use jj_lib::merge::Merge;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::RefTarget;
use jj_lib::op_store::RemoteRef;
use jj_lib::op_store::RemoteRefState;
use jj_lib::op_store::RemoteView;
use jj_lib::op_store::View;
use jj_lib::op_store::WorkingCopyPatternsId;
use jj_lib::ref_name::RefNameBuf;
use jj_lib::repo::ReadonlyRepo;
use jj_lib::repo::Repo as _;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::repo_path::RepoPathComponentBuf;
use jj_lib::settings::UserSettings;
use jj_lib::signing::Signer;
use jj_lib::working_copy_patterns::WorkingCopyPatterns;
use serde::Deserialize;
use serde::Serialize;
use tempfile::NamedTempFile;
use tempfile::TempDir;

const FORMAT: &str = "jjosh-native";
const VERSION: u32 = 1;
const ROOT: &str = "0000000000000000000000000000000000000000";

/// The manifest is authoritative for native identities; the temporary backend
/// supplies only immutable Git tree/blob contents, never synthesized metadata.
pub(crate) struct Bundle {
    pub repo: Arc<ReadonlyRepo>,
    pub view: View,
    pub commits: HashMap<CommitId, backend::Commit>,
    pub source_operation: String,
    _temp: TempDir,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    source_operation: String,
    root_commit: String,
    #[serde(deserialize_with = "unique_map")]
    commits: BTreeMap<String, CommitData>,
    view: ViewData,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "unique_map"
    )]
    working_copy_patterns: BTreeMap<String, WorkingCopyPatterns>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitData {
    parents: Vec<String>,
    // Terms alternate add/remove/add, without simplification or sorting.
    root_tree: Vec<String>,
    conflict_labels: Vec<String>,
    change_id: String,
    description: String,
    author: SignatureData,
    committer: SignatureData,
    secure_sig: Option<SecureSigData>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignatureData {
    name: String,
    email: String,
    millis_since_epoch: i64,
    tz_offset: i32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecureSigData {
    data: Vec<u8>,
    sig: Vec<u8>,
}

type RefData = Vec<Option<String>>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewData {
    head_ids: Vec<String>,
    #[serde(deserialize_with = "unique_map")]
    local_bookmarks: BTreeMap<String, RefData>,
    #[serde(deserialize_with = "unique_map")]
    local_tags: BTreeMap<String, RefData>,
    #[serde(deserialize_with = "unique_map")]
    remote_views: BTreeMap<String, RemoteViewData>,
    #[serde(deserialize_with = "unique_map")]
    git_refs: BTreeMap<String, RefData>,
    #[serde(deserialize_with = "unique_map")]
    git_heads: BTreeMap<String, RefData>,
    #[serde(deserialize_with = "unique_map")]
    wc_commit_ids: BTreeMap<String, String>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "unique_map"
    )]
    wc_sparse_patterns: BTreeMap<String, Vec<Option<String>>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteViewData {
    #[serde(deserialize_with = "unique_map")]
    bookmarks: BTreeMap<String, RemoteRefData>,
    #[serde(deserialize_with = "unique_map")]
    tags: BTreeMap<String, RemoteRefData>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteRefData {
    target: RefData,
    state: TrackingState,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TrackingState {
    New,
    Tracked,
}

// JSON's default map decoder silently overwrites duplicate identities/names.
fn unique_map<'de, D, T>(deserializer: D) -> std::result::Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Visitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a map with unique keys")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut access: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = access.next_entry::<String, T>()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate map key"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Visitor(std::marker::PhantomData))
}

fn validate_hex(value: &str, bytes: usize, kind: &str) -> Result<()> {
    ensure!(
        value.len() == bytes * 2
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid {kind}: expected {} lowercase hexadecimal digits",
        bytes * 2
    );
    Ok(())
}

fn commit_id(value: &str) -> Result<CommitId> {
    validate_hex(value, 20, "commit ID")?;
    CommitId::try_from_hex(value).context("invalid commit ID")
}

fn merge<T>(terms: Vec<T>, kind: &str) -> Result<Merge<T>> {
    ensure!(
        terms.len() % 2 == 1,
        "{kind} must have a nonempty odd number of signed terms"
    );
    Ok(Merge::from_vec(terms))
}

impl From<&backend::Signature> for SignatureData {
    fn from(value: &backend::Signature) -> Self {
        Self {
            name: value.name.clone(),
            email: value.email.clone(),
            millis_since_epoch: value.timestamp.timestamp.0,
            tz_offset: value.timestamp.tz_offset,
        }
    }
}

impl SignatureData {
    fn decode(self) -> Result<backend::Signature> {
        ensure!(
            self.tz_offset.checked_mul(60).is_some(),
            "signature timezone offset overflows seconds"
        );
        Ok(backend::Signature {
            name: self.name,
            email: self.email,
            timestamp: backend::Timestamp {
                timestamp: backend::MillisSinceEpoch(self.millis_since_epoch),
                tz_offset: self.tz_offset,
            },
        })
    }
}

impl From<&backend::Commit> for CommitData {
    fn from(value: &backend::Commit) -> Self {
        Self {
            parents: value.parents.iter().map(|id| id.hex()).collect(),
            root_tree: value.root_tree.iter().map(|id| id.hex()).collect(),
            conflict_labels: value.conflict_labels.iter().cloned().collect(),
            change_id: value.change_id.hex(),
            description: value.description.clone(),
            author: (&value.author).into(),
            committer: (&value.committer).into(),
            secure_sig: value.secure_sig.as_ref().map(|sig| SecureSigData {
                data: sig.data.clone(),
                sig: sig.sig.clone(),
            }),
        }
    }
}

impl CommitData {
    fn decode(self) -> Result<backend::Commit> {
        validate_hex(&self.change_id, 16, "change ID")?;
        let trees = self
            .root_tree
            .into_iter()
            .map(|id| {
                validate_hex(&id, 20, "tree ID")?;
                TreeId::try_from_hex(&id).context("invalid tree ID")
            })
            .collect::<Result<Vec<_>>>()?;
        let root_tree = merge(trees, "root tree")?;
        let conflict_labels = merge(self.conflict_labels, "conflict labels")?;
        ensure!(
            conflict_labels.as_resolved().is_some_and(String::is_empty)
                || (!root_tree.is_resolved()
                    && conflict_labels.num_sides() == root_tree.num_sides()
                    && conflict_labels.iter().all(|label| !label.contains('\n'))),
            "conflict labels must be resolved empty or match the root tree terms without newlines"
        );
        Ok(backend::Commit {
            parents: self
                .parents
                .iter()
                .map(|id| commit_id(id))
                .collect::<Result<_>>()?,
            predecessors: vec![], // Old evolution archives deliberately are not transported.
            root_tree,
            conflict_labels,
            change_id: ChangeId::try_from_hex(&self.change_id).context("invalid change ID")?,
            description: self.description,
            author: self.author.decode()?,
            committer: self.committer.decode()?,
            secure_sig: self.secure_sig.map(|sig| backend::SecureSig {
                data: sig.data,
                sig: sig.sig,
            }),
        })
    }
}

fn encode_ref(target: &RefTarget) -> RefData {
    target
        .as_merge()
        .iter()
        .map(|term| term.as_ref().map(|id| id.hex()))
        .collect()
}

fn decode_ref(terms: RefData) -> Result<RefTarget> {
    let terms = terms
        .into_iter()
        .map(|term| term.map(|id| commit_id(&id)).transpose())
        .collect::<Result<Vec<_>>>()?;
    Ok(RefTarget::from_merge(merge(terms, "reference")?))
}

fn encode_refs<'a, N: 'a>(
    refs: impl IntoIterator<Item = (&'a N, &'a RefTarget)>,
    name: impl Fn(&N) -> &str,
) -> BTreeMap<String, RefData> {
    refs.into_iter()
        .map(|(key, target)| (name(key).to_owned(), encode_ref(target)))
        .collect()
}

fn decode_refs<N: From<String> + Ord>(
    refs: BTreeMap<String, RefData>,
) -> Result<BTreeMap<N, RefTarget>> {
    refs.into_iter()
        .map(|(name, target)| Ok((name.into(), decode_ref(target)?)))
        .collect()
}

impl From<&RemoteView> for RemoteViewData {
    fn from(value: &RemoteView) -> Self {
        let refs = |refs: &BTreeMap<RefNameBuf, RemoteRef>| {
            refs.iter()
                .map(|(name, remote)| {
                    (
                        name.as_str().to_owned(),
                        RemoteRefData {
                            target: encode_ref(&remote.target),
                            state: match remote.state {
                                RemoteRefState::New => TrackingState::New,
                                RemoteRefState::Tracked => TrackingState::Tracked,
                            },
                        },
                    )
                })
                .collect()
        };
        Self {
            bookmarks: refs(&value.bookmarks),
            tags: refs(&value.tags),
        }
    }
}

impl RemoteViewData {
    fn decode(self) -> Result<RemoteView> {
        let refs = |refs: BTreeMap<String, RemoteRefData>| {
            refs.into_iter()
                .map(|(name, remote)| {
                    Ok((
                        name.into(),
                        RemoteRef {
                            target: decode_ref(remote.target)?,
                            state: match remote.state {
                                TrackingState::New => RemoteRefState::New,
                                TrackingState::Tracked => RemoteRefState::Tracked,
                            },
                        },
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()
        };
        Ok(RemoteView {
            bookmarks: refs(self.bookmarks)?,
            tags: refs(self.tags)?,
        })
    }
}

impl From<&View> for ViewData {
    fn from(value: &View) -> Self {
        let mut head_ids: Vec<_> = value.head_ids.iter().map(|id| id.hex()).collect();
        head_ids.sort_unstable();
        Self {
            head_ids,
            local_bookmarks: encode_refs(&value.local_bookmarks, |name| name.as_str()),
            local_tags: encode_refs(&value.local_tags, |name| name.as_str()),
            remote_views: value
                .remote_views
                .iter()
                .map(|(name, remote)| (name.as_str().to_owned(), remote.into()))
                .collect(),
            git_refs: encode_refs(&value.git_refs, |name| name.as_str()),
            git_heads: encode_refs(&value.git_heads, |name| name.as_str()),
            wc_commit_ids: value
                .wc_commit_ids
                .iter()
                .map(|(name, id)| (name.as_str().to_owned(), id.hex()))
                .collect(),
            wc_sparse_patterns: value
                .wc_sparse_patterns
                .iter()
                .map(|(name, target)| {
                    (
                        name.as_str().to_owned(),
                        target
                            .iter()
                            .map(|term| term.as_ref().map(|id| id.hex()))
                            .collect(),
                    )
                })
                .collect(),
        }
    }
}

impl ViewData {
    fn decode(self) -> Result<View> {
        let mut head_ids = HashSet::new();
        for id in self.head_ids {
            ensure!(
                head_ids.insert(commit_id(&id)?),
                "duplicate visible head {id}"
            );
        }
        ensure!(!head_ids.is_empty(), "native view has no heads");
        let wc_sparse_patterns = self
            .wc_sparse_patterns
            .into_iter()
            .map(|(name, terms)| {
                ensure!(
                    self.wc_commit_ids.contains_key(&name),
                    "sparse selection references unknown workspace {name}"
                );
                let terms = terms
                    .into_iter()
                    .map(|term| {
                        term.map(|id| {
                            validate_hex(&id, 64, "working-copy patterns ID")?;
                            WorkingCopyPatternsId::try_from_hex(&id)
                                .context("invalid working-copy patterns ID")
                        })
                        .transpose()
                    })
                    .collect::<Result<_>>()?;
                Ok((name.into(), merge(terms, "sparse configuration")?))
            })
            .collect::<Result<_>>()?;
        Ok(View {
            head_ids,
            local_bookmarks: decode_refs(self.local_bookmarks)?,
            local_tags: decode_refs(self.local_tags)?,
            remote_views: self
                .remote_views
                .into_iter()
                .map(|(name, remote)| Ok((name.into(), remote.decode()?)))
                .collect::<Result<_>>()?,
            git_refs: decode_refs(self.git_refs)?,
            git_heads: decode_refs(self.git_heads)?,
            wc_commit_ids: self
                .wc_commit_ids
                .into_iter()
                .map(|(name, id)| Ok((name.into(), commit_id(&id)?)))
                .collect::<Result<_>>()?,
            wc_sparse_patterns,
        })
    }
}

fn git_backend(repo: &ReadonlyRepo) -> Result<&GitBackend> {
    let backend = repo.store().backend();
    ensure!(
        backend.commit_id_length() == 20,
        "native bundles require a Git SHA-1 backend"
    );
    backend
        .downcast_ref::<GitBackend>()
        .context("native bundles require a Git backend")
}

/// No inherited Git environment, global config, replace refs, lazy network
/// fetches, or hooks may influence transport. The explicit source Git directory
/// is read only; load commands use a newly initialized private Git directory.
fn git_command(path: &Path) -> Command {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .arg("--git-dir")
        .arg(path)
        .args(["-c", "core.hooksPath=/dev/null"]);
    command
}

fn run_git(command: &mut Command, action: &str) -> Result<()> {
    let output = command
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("{action}: running git"))?;
    ensure!(
        output.status.success(),
        "{action}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

fn object_roots(commits: &HashMap<CommitId, backend::Commit>) -> BTreeMap<String, &'static str> {
    let mut roots = BTreeMap::new();
    for (id, commit) in commits {
        if id.hex() == ROOT {
            // The root and its empty tree are native synthetic objects. Every
            // real commit's terms, including empty terms, are physical roots.
            continue;
        }
        roots.insert(id.hex(), "commit");
        for tree in commit.root_tree.iter() {
            roots.insert(tree.hex(), "tree");
        }
    }
    roots
}

/// Validate the entire current parent closure, including hidden ref terms and
/// Git observations. Neither dangling extras nor cycles are legitimate bundles.
fn validate_graph(
    view: &View,
    commits: &HashMap<CommitId, backend::Commit>,
    root: &backend::Commit,
) -> Result<()> {
    let root_id = commit_id(ROOT)?;
    ensure!(
        commits.get(&root_id) == Some(root),
        "bundle must contain the canonical synthetic root commit {ROOT}"
    );
    let native_view = jj_lib::view::View::new(view.clone(), false);
    let mut pending: Vec<_> = native_view
        .all_referenced_commit_ids()
        .map(|id| (id.clone(), false))
        .collect();
    pending.push((root_id.clone(), false));
    let mut active = HashSet::new();
    let mut done = HashSet::new();
    while let Some((id, exit)) = pending.pop() {
        if exit {
            active.remove(&id);
            done.insert(id);
            continue;
        }
        if done.contains(&id) {
            continue;
        }
        ensure!(
            active.insert(id.clone()),
            "cyclic native parent graph at {id}"
        );
        let commit = commits
            .get(&id)
            .with_context(|| format!("missing referenced native commit {id}"))?;
        if id != root_id {
            ensure!(
                !commit.parents.is_empty(),
                "non-root commit {id} has no parents"
            );
            ensure!(
                commit.parents.len() == 1 || !commit.parents.contains(&root_id),
                "commit {id} mixes synthetic root with other parents"
            );
        }
        pending.push((id, true));
        pending.extend(commit.parents.iter().rev().map(|id| (id.clone(), false)));
    }
    ensure!(
        done.len() == commits.len(),
        "manifest contains commits outside the recorded view's parent closure"
    );
    Ok(())
}

pub(crate) async fn export(repo: &Arc<ReadonlyRepo>, path: &Path) -> Result<usize> {
    let backend = git_backend(repo)?;
    let view = repo.view().store_view();
    let mut pending: Vec<_> = repo.view().all_referenced_commit_ids().cloned().collect();
    pending.push(repo.store().root_commit_id().clone());
    let mut commits = HashMap::new();
    while let Some(id) = pending.pop() {
        if commits.contains_key(&id) {
            continue;
        }
        let mut commit = backend
            .read_commit(&id)
            .await
            .with_context(|| format!("reading recorded native commit {id}"))?;
        commit.predecessors.clear();
        pending.extend(commit.parents.iter().cloned());
        commits.insert(id, commit);
    }
    let root = backend.read_commit(repo.store().root_commit_id()).await?;
    validate_graph(view, &commits, &root)?;
    validate_trees(repo, &commits).await?;
    let mut working_copy_patterns = BTreeMap::new();
    for target in view.wc_sparse_patterns.values() {
        for id in target.iter().flatten() {
            if !working_copy_patterns.contains_key(&id.hex()) {
                let patterns = repo.op_store().read_working_copy_patterns(id).await?;
                patterns.validate()?;
                ensure!(
                    patterns.id() == *id,
                    "working-copy patterns ID does not match its contents"
                );
                working_copy_patterns.insert(id.hex(), patterns);
            }
        }
    }
    let manifest = Manifest {
        format: FORMAT.to_owned(),
        version: VERSION,
        source_operation: repo.operation().id().hex(),
        root_commit: ROOT.to_owned(),
        commits: commits
            .iter()
            .map(|(id, commit)| (id.hex(), commit.into()))
            .collect(),
        view: view.into(),
        working_copy_patterns,
    };
    let mut roots = tempfile::tempfile()?;
    for id in object_roots(&commits).keys() {
        writeln!(roots, "{id}")?;
    }
    roots.rewind()?;
    let mut pack = tempfile::tempfile()?;
    run_git(
        git_command(backend.git_repo_path())
            .args(["pack-objects", "--stdout", "--revs", "--no-reuse-delta"])
            .stdin(Stdio::from(roots))
            .stdout(Stdio::from(pack.try_clone()?)),
        "packing native objects",
    )?;
    pack.rewind()?;
    // In particular, shallow repositories must not publish an apparently
    // self-contained pack whose raw commit parents are actually unavailable.
    let verification_dir = tempfile::tempdir()?;
    let verification_repo = empty_repo(verification_dir.path(), repo.settings()).await?;
    install_pack(&verification_repo, &mut pack, &commits)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut output = NamedTempFile::new_in(parent).context("creating bundle output")?;
    {
        let mut archive = tar::Builder::new(output.as_file_mut());
        let manifest = serde_json::to_vec(&manifest)?;
        append_entry(
            &mut archive,
            "manifest.json",
            manifest.len() as u64,
            manifest.as_slice(),
        )?;
        append_entry(
            &mut archive,
            "objects.pack",
            pack.metadata()?.len(),
            &mut pack,
        )?;
        archive.finish()?;
    }
    output.as_file().sync_all()?;
    output
        .persist_noclobber(path)
        .with_context(|| format!("publishing bundle {} without overwriting", path.display()))?;
    Ok(commits.len() - 1)
}

fn append_entry<W: Write>(
    archive: &mut tar::Builder<W>,
    path: &str,
    size: u64,
    contents: impl Read,
) -> Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_size(size);
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_cksum();
    archive.append_data(&mut header, path, contents)?;
    Ok(())
}

async fn empty_repo(path: &Path, settings: &UserSettings) -> Result<Arc<ReadonlyRepo>> {
    ReadonlyRepo::init(
        settings,
        path,
        &|settings, store_path| {
            Ok(Box::new(GitBackend::init_internal(
                settings,
                store_path,
                gix_hash::Kind::Sha1,
            )?))
        },
        Signer::new(None, vec![]),
        ReadonlyRepo::default_op_store_initializer(),
        ReadonlyRepo::default_op_heads_store_initializer(),
        ReadonlyRepo::default_index_store_initializer(),
        ReadonlyRepo::default_submodule_store_initializer(),
    )
    .await
    .map_err(Into::into)
}

pub(crate) async fn load(path: &Path, settings: &UserSettings) -> Result<Bundle> {
    let temp = tempfile::tempdir()?;
    let mut pack = tempfile::tempfile()?;
    let mut manifest = None;
    let mut have_pack = false;
    let input =
        File::open(path).with_context(|| format!("opening native bundle {}", path.display()))?;
    let length = input.metadata()?.len();
    ensure!(
        length % 512 == 0,
        "truncated native bundle: TAR length is not block aligned"
    );
    let mut archive = tar::Archive::new(input);
    // Raw iteration exposes extension headers rather than trusting a PAX/GNU
    // path rewrite. Nothing is unpacked to a filesystem-selected path.
    for entry in archive.entries()?.raw(true) {
        let mut entry = entry.context("reading native bundle entry")?;
        ensure!(
            entry.header().entry_type().is_file(),
            "native bundle entries must be regular files"
        );
        match entry.path_bytes().as_ref() {
            b"manifest.json" => {
                ensure!(
                    manifest.is_none(),
                    "duplicate manifest.json in native bundle"
                );
                manifest = Some(
                    serde_json::from_reader::<_, Manifest>(&mut entry)
                        .context("decoding native bundle manifest")?,
                );
            }
            b"objects.pack" => {
                ensure!(!have_pack, "duplicate objects.pack in native bundle");
                std::io::copy(&mut entry, &mut pack).context("reading native object pack")?;
                have_pack = true;
            }
            _ => bail!("unexpected native bundle entry {:?}", entry.path_bytes()),
        }
    }
    let mut input = archive.into_inner();
    // tar accepts a missing end marker; our format requires both zero blocks
    // and rejects concatenated archives or other nonzero trailing data.
    ensure!(
        length.saturating_sub(input.stream_position()?) >= 512,
        "truncated native bundle: missing TAR end marker"
    );
    let mut trailing = [0; 8192];
    loop {
        let count = input.read(&mut trailing)?;
        if count == 0 {
            break;
        }
        ensure!(
            trailing[..count].iter().all(|byte| *byte == 0),
            "unexpected trailing data in native bundle"
        );
    }
    let manifest = manifest.context("native bundle is missing manifest.json")?;
    ensure!(have_pack, "native bundle is missing objects.pack");
    ensure!(manifest.format == FORMAT, "not a jjosh native bundle");
    ensure!(
        manifest.version == VERSION,
        "unsupported native bundle version {} (supported: {VERSION})",
        manifest.version
    );
    ensure!(
        manifest.root_commit == ROOT,
        "invalid synthetic root ID in native bundle"
    );
    validate_hex(&manifest.source_operation, 64, "source operation ID")?;
    let commits = manifest
        .commits
        .into_iter()
        .map(|(id, commit)| {
            Ok((
                commit_id(&id)?,
                commit
                    .decode()
                    .with_context(|| format!("decoding native commit {id}"))?,
            ))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    let view = manifest.view.decode()?;
    let referenced_patterns: BTreeSet<_> = view
        .wc_sparse_patterns
        .values()
        .flat_map(|target| target.iter().flatten())
        .map(|id| id.hex())
        .collect();
    ensure!(
        referenced_patterns == manifest.working_copy_patterns.keys().cloned().collect(),
        "native bundle sparse configuration objects do not match the view references"
    );
    for (id, patterns) in &manifest.working_copy_patterns {
        patterns.validate()?;
        ensure!(
            patterns.id().hex() == *id,
            "working-copy patterns ID {id} does not match its contents"
        );
    }
    let repo = empty_repo(temp.path(), settings).await?;
    for patterns in manifest.working_copy_patterns.values() {
        repo.op_store()
            .write_working_copy_patterns(patterns)
            .await?;
    }
    let backend = git_backend(&repo)?;
    backend.disable_lazy_commit_imports();
    let root = backend.read_commit(repo.store().root_commit_id()).await?;
    validate_graph(&view, &commits, &root)?;
    install_pack(&repo, &mut pack, &commits)?;
    validate_trees(&repo, &commits).await?;
    Ok(Bundle {
        repo,
        view,
        commits,
        source_operation: manifest.source_operation,
        _temp: temp,
    })
}

fn install_pack(
    repo: &ReadonlyRepo,
    pack: &mut File,
    commits: &HashMap<CommitId, backend::Commit>,
) -> Result<()> {
    let backend = git_backend(repo)?;
    pack.rewind()?;
    run_git(
        git_command(backend.git_repo_path())
            .args(["index-pack", "--stdin", "--strict"])
            .stdin(Stdio::from(pack.try_clone()?))
            .stdout(Stdio::null()),
        "validating native object pack",
    )?;
    validate_object_roots(backend.git_repo_path(), commits)?;
    pack.rewind()?;
    Ok(())
}

fn validate_object_roots(path: &Path, commits: &HashMap<CommitId, backend::Commit>) -> Result<()> {
    let roots = object_roots(commits);
    let mut input = tempfile::tempfile()?;
    for id in roots.keys() {
        writeln!(input, "{id}")?;
    }
    input.rewind()?;
    let output = git_command(path)
        .arg("cat-file")
        .arg("--batch-check=%(objectname) %(objecttype)")
        .stdin(Stdio::from(input))
        .output()
        .context("checking native manifest object roots")?;
    ensure!(
        output.status.success(),
        "checking native object roots: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = std::str::from_utf8(&output.stdout)?;
    let mut lines = output.lines();
    for (id, kind) in roots {
        ensure!(
            lines.next() == Some(format!("{id} {kind}").as_str()),
            "bundle is missing required {kind} object {id}"
        );
    }
    ensure!(
        lines.next().is_none(),
        "unexpected object verification output"
    );
    Ok(())
}

async fn validate_trees(
    repo: &ReadonlyRepo,
    commits: &HashMap<CommitId, backend::Commit>,
) -> Result<()> {
    let backend = git_backend(repo)?;
    let raw_repo = backend.git_repo();
    let mut pending: Vec<_> = commits
        .values()
        .flat_map(|commit| commit.root_tree.iter())
        .map(|id| (RepoPathBuf::root(), id.clone()))
        .collect();
    let mut seen = HashSet::new();
    let mut blobs = BTreeSet::new();
    while let Some((path, id)) = pending.pop() {
        if !seen.insert(id.clone()) || id == *backend.empty_tree_id() {
            continue;
        }
        // Validate components before the backend's infallible native conversion.
        let raw_tree = raw_repo
            .find_object(gix_hash::ObjectId::from_hex(id.hex().as_bytes())?)?
            .try_into_tree()?;
        let mut names = HashSet::new();
        for entry in raw_tree.iter() {
            let entry = entry?;
            let name = std::str::from_utf8(entry.filename())?;
            RepoPathComponentBuf::new(name)?;
            ensure!(
                names.insert(name.to_owned()),
                "duplicate tree entry at {path:?}"
            );
        }
        let tree = backend.read_tree(&path, &id).await?;
        for entry in tree.entries() {
            let entry_path = path.join(entry.name());
            match entry.value() {
                TreeValue::Tree(child) => pending.push((entry_path, child.clone())),
                TreeValue::File { id, copy_id, .. } => {
                    ensure!(
                        copy_id.as_bytes().is_empty(),
                        "tracked copy metadata at {entry_path:?} cannot be preserved"
                    );
                    if blobs.insert(id.hex()) {
                        let object = raw_repo
                            .find_object(gix_hash::ObjectId::from_hex(id.hex().as_bytes())?)?;
                        object
                            .try_into_blob()
                            .with_context(|| format!("file at {entry_path:?} is not a blob"))?;
                    }
                }
                TreeValue::Symlink(id) => {
                    // Native symlinks require UTF-8, unlike ordinary blob data.
                    backend.read_symlink(&entry_path, id).await?;
                }
                TreeValue::GitSubmodule(_) => {} // Foreign object, not a missing local commit.
            }
        }
    }
    Ok(())
}
