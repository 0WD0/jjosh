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

//! Crash recovery for transactions spanning operation publication and local files.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs::File;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use bstr::ByteSlice as _;
use futures::TryStreamExt as _;
use gix::refs::FullName;
use gix::refs::FullNameRef;
use gix::refs::Namespace;
use gix::refs::Target;
use gix::refs::transaction::Change;
use gix::refs::transaction::PreviousValue;
use gix::refs::transaction::RefEdit;
use thiserror::Error;

use crate::git::get_git_backend;
use crate::git::get_git_repo;
use crate::object_id::ObjectId as _;
use crate::op_store::OperationId;
use crate::repo::ReadonlyRepo;
use crate::repo::Repo as _;
use crate::repo::RepoLoader;
use crate::transaction::Transaction;

/// Operation attribute identifying the one transaction enrolled in a journal.
pub const TRANSACTION_ATTRIBUTE: &str = "local-state-transaction";

/// A local-state safety conflict or storage failure. Errors leave the fence intact.
#[derive(Debug, Error)]
pub enum LocalStateError {
    /// The state cannot safely be attributed to this transaction.
    #[error("{0}")]
    Safety(String),
    /// A legacy journal has no trustworthy publication evidence.
    #[error(
        "This local-state journal lacks verifiable publication evidence. Recover it using the \
         version that created it before upgrading."
    )]
    LegacyJournal,
    /// A filesystem, reference-store, or operation-store failure.
    #[error("Local-state storage error")]
    Storage(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl LocalStateError {
    fn other(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Storage(error.into())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Publication {
    Prepared,
    OperationPrepared { target: String },
    Published { operation_id: String },
    LocalCommitted,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct FileChange {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<Option<Vec<u8>>>,
    retire: bool,
}

impl FileChange {
    fn final_contents(&self) -> Option<&[u8]> {
        self.after.last().unwrap_or(&self.before).as_deref()
    }

    fn record(&mut self, contents: Option<Vec<u8>>) -> Result<(), LocalStateError> {
        if read_journal_file(&self.path)?.as_deref() != self.final_contents() {
            return Err(LocalStateError::Safety(format!(
                "File {} changed between prepared local-state steps; refusing to absorb an \
                 external update",
                self.path.display()
            )));
        }
        self.after.push(contents);
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RefChange {
    name: FullName,
    before: Option<Target>,
    after: Option<Target>,
    previous: Vec<Option<Target>>,
    retire: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RefStoreChanges {
    git_dir: PathBuf,
    namespace: Option<Namespace>,
    changes: Vec<RefChange>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct JournalRecord {
    version: u32,
    repo_path: PathBuf,
    origin_operation: String,
    begin_heads: BTreeSet<String>,
    nonce: String,
    publication: Publication,
    files: Vec<FileChange>,
    ref_stores: Vec<RefStoreChanges>,
}

impl JournalRecord {
    fn require_prepared(&self) -> Result<(), LocalStateError> {
        if !matches!(self.publication, Publication::Prepared) {
            return Err(LocalStateError::Safety(
                "Local-state mutation follows its commit decision".into(),
            ));
        }
        Ok(())
    }

    fn check_repo(&self, loader: &RepoLoader) -> Result<(), LocalStateError> {
        if canonical_repo_path(loader)? != self.repo_path {
            return Err(LocalStateError::Safety(format!(
                "Pending local-state transaction belongs to JJ repository {}; run `jj util \
                 recover` there",
                self.repo_path.display()
            )));
        }
        Ok(())
    }

    fn ref_store_key(
        &mut self,
        git_repo: &gix::Repository,
        name: &FullName,
    ) -> Result<(usize, FullName), LocalStateError> {
        let shared = name
            .as_ref()
            .category()
            .is_none_or(|category| !category.is_worktree_private());
        let directory = if shared {
            git_repo.common_dir()
        } else {
            git_repo.path()
        };
        let git_dir = dunce::canonicalize(directory).map_err(LocalStateError::other)?;
        let (namespace, name) = if shared {
            // Let gix resolve namespace and worktree aliases, then store that
            // exact shared name in one transaction owning the packed-refs lock.
            let location = reference_location(git_repo, name);
            let relative = location
                .strip_prefix(&git_dir)
                .map_err(LocalStateError::other)?;
            let bytes = gix::path::try_into_bstr(relative).map_err(LocalStateError::other)?;
            let bytes = gix::path::to_unix_separators_on_windows(bytes);
            let name = FullName::try_from(bytes.as_ref()).map_err(LocalStateError::other)?;
            (None, name)
        } else {
            (git_repo.namespace().cloned(), name.clone())
        };
        let position = self
            .ref_stores
            .iter()
            .position(|store| store.git_dir == git_dir && store.namespace == namespace);
        let index = position.unwrap_or_else(|| {
            self.ref_stores.push(RefStoreChanges {
                git_dir,
                namespace,
                changes: Vec::new(),
            });
            self.ref_stores.len() - 1
        });
        Ok((index, name))
    }
}

fn journal_path(git_repo: &gix::Repository) -> PathBuf {
    // Preserve the physical fence so older binaries cannot overlook a new journal.
    git_repo.common_dir().join("jj-remote-journal")
}

fn journal_matches_store(path: &Path, git_repo: &gix::Repository) -> Result<bool, LocalStateError> {
    let journal_dir =
        dunce::canonicalize(path.parent().unwrap()).map_err(LocalStateError::other)?;
    let common_dir = dunce::canonicalize(git_repo.common_dir()).map_err(LocalStateError::other)?;
    Ok(journal_dir == common_dir)
}

fn canonical_repo_path(loader: &RepoLoader) -> Result<PathBuf, LocalStateError> {
    let backend = get_git_backend(loader.store()).map_err(LocalStateError::other)?;
    let path = backend.store_path().parent().ok_or_else(|| {
        LocalStateError::Safety("Git backend store has no owning JJ repository path".into())
    })?;
    dunce::canonicalize(path).map_err(LocalStateError::other)
}

fn require_exclusive_heads(loader: &RepoLoader) -> Result<(), LocalStateError> {
    // OpHeadsStore::lock() is explicitly optional. Publication evidence needs
    // the concrete filesystem store's exclusive lock, not a plugin's no-op.
    if loader
        .op_heads_store()
        .as_ref()
        .downcast_ref::<crate::simple_op_heads_store::SimpleOpHeadsStore>()
        .is_none()
    {
        return Err(LocalStateError::Safety(
            "Local-state transactions require the filesystem SimpleOpHeadsStore; this \
             operation-heads store does not provide a supported exclusive publication lock"
                .into(),
        ));
    }
    Ok(())
}

/// Tests whether this Git store has an interrupted cross-store transaction.
pub fn has_pending(git_repo: &gix::Repository) -> Result<bool, LocalStateError> {
    journal_path(git_repo)
        .try_exists()
        .map_err(LocalStateError::other)
}

/// Rejects mutation or transport while recovery is pending.
pub fn ensure_no_pending(git_repo: &gix::Repository) -> Result<(), LocalStateError> {
    if has_pending(git_repo)? {
        return Err(LocalStateError::Safety(
            "Interrupted local-state change requires `jj util recover` before mutation or \
             transport"
                .into(),
        ));
    }
    Ok(())
}

fn lock_journal(path: &Path) -> Result<crate::lock::FileLock, LocalStateError> {
    crate::lock::FileLock::try_lock(path.with_extension("lock"))
        .map_err(LocalStateError::other)?
        .ok_or_else(|| {
            LocalStateError::Safety("Another local-state change or recovery is running".into())
        })
}

fn load_record(path: &Path) -> Result<JournalRecord, LocalStateError> {
    let bytes = std::fs::read(path).map_err(LocalStateError::other)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(LocalStateError::other)?;
    if value.get("version").and_then(serde_json::Value::as_u64) != Some(2) {
        return Err(LocalStateError::LegacyJournal);
    }
    let record: JournalRecord = serde_json::from_value(value).map_err(LocalStateError::other)?;
    // The native serde implementations preserve bytes, but don't validate names.
    for store in &record.ref_stores {
        if let Some(namespace) = &store.namespace {
            let prefix = namespace.as_bstr().strip_suffix(b"/").ok_or_else(|| {
                LocalStateError::Safety("Invalid reference namespace in local-state journal".into())
            })?;
            <&FullNameRef>::try_from(prefix.as_bstr()).map_err(LocalStateError::other)?;
            if !prefix.starts_with(b"refs/namespaces/") {
                return Err(LocalStateError::Safety(
                    "Invalid reference namespace in local-state journal".into(),
                ));
            }
        }
        for change in &store.changes {
            validate_ref_name(&change.name)?;
            for target in std::iter::once(&change.before)
                .chain(std::iter::once(&change.after))
                .chain(&change.previous)
                .flatten()
            {
                if let Target::Symbolic(name) = target {
                    validate_ref_name(name)?;
                }
            }
        }
    }
    Ok(record)
}

fn validate_ref_name(name: &FullName) -> Result<(), LocalStateError> {
    <&FullNameRef>::try_from(name.as_bstr()).map_err(LocalStateError::other)?;
    Ok(())
}

fn canonical_file_path(path: &Path) -> Result<PathBuf, LocalStateError> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(LocalStateError::Safety(format!(
            "Parent traversal is not allowed in local-state file {}",
            path.display()
        )));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            LocalStateError::Safety(format!(
                "Local-state file has no parent: {}",
                path.display()
            ))
        })?;
    let name = path
        .file_name()
        .ok_or_else(|| LocalStateError::Safety("Local-state target is not a file".into()))?;
    Ok(dunce::canonicalize(parent)
        .map_err(LocalStateError::other)?
        .join(name))
}

fn checked_file_path(
    path: &Path,
    repo_path: &Path,
    git_repo: &gix::Repository,
    extra_paths: &[PathBuf],
) -> Result<PathBuf, LocalStateError> {
    let path = canonical_file_path(path)?;
    let common = dunce::canonicalize(git_repo.common_dir()).map_err(LocalStateError::other)?;
    if path.starts_with(repo_path)
        || path.starts_with(common)
        || extra_paths.contains(&path)
        || path == canonical_file_path(&git_repo.index_path())?
    {
        Ok(path)
    } else {
        Err(LocalStateError::Safety(format!(
            "File {} is outside the owning JJ repository and Git store",
            path.display()
        )))
    }
}

fn validate_file_paths(
    record: &JournalRecord,
    owner: &gix::Repository,
    extra_paths: &[PathBuf],
) -> Result<(), LocalStateError> {
    for change in &record.files {
        checked_file_path(&change.path, &record.repo_path, owner, extra_paths)?;
    }
    Ok(())
}

// Validate reference store identity before taking writer locks. Worktree-local
// refs remain attached to their original store.
fn open_ref_stores(
    record: &JournalRecord,
    owner: &gix::Repository,
) -> Result<Vec<gix::Repository>, LocalStateError> {
    let common = dunce::canonicalize(owner.common_dir()).map_err(LocalStateError::other)?;
    record
        .ref_stores
        .iter()
        .map(|store| {
            if store
                .git_dir
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            {
                return Err(LocalStateError::Safety(
                    "Parent traversal in a journal reference store".into(),
                ));
            }
            let mut git_repo = gix::open(&store.git_dir).map_err(LocalStateError::other)?;
            if dunce::canonicalize(git_repo.common_dir()).map_err(LocalStateError::other)? != common
            {
                return Err(LocalStateError::Safety(
                    "Journal reference store belongs to a different Git repository".into(),
                ));
            }
            git_repo.refs.namespace = store.namespace.clone();
            Ok(git_repo)
        })
        .collect()
}

/// A durable cross-store transaction. Dropping it never removes its recovery record.
pub struct LocalStateTransaction {
    path: PathBuf,
    loader: RepoLoader,
    // Caller authority is never serialized into the recovery record.
    extra_paths: Vec<PathBuf>,
    _lock: crate::lock::FileLock,
}

/// Captures local preimages and the actual operation heads under both stores' locks.
/// `extra_paths` are exact additional files authorized by the caller, including
/// externally stored repository configuration resolved by the CLI.
pub async fn begin(
    repo: &ReadonlyRepo,
    extra_paths: &[PathBuf],
) -> Result<LocalStateTransaction, LocalStateError> {
    require_exclusive_heads(repo.loader())?;
    let git_repo = get_git_repo(repo.store()).map_err(LocalStateError::other)?;
    let path = journal_path(&git_repo);
    let lock = lock_journal(&path)?;
    let _heads_lock = repo
        .op_heads_store()
        .lock()
        .await
        .map_err(LocalStateError::other)?;
    ensure_no_pending(&git_repo)?;
    let repo_path = canonical_repo_path(repo.loader())?;
    let config = git_repo
        .config_path(gix::config::Source::Local)
        .map_err(LocalStateError::other)?;
    let extra_paths: Vec<_> = extra_paths
        .iter()
        .map(|path| canonical_file_path(path))
        .collect::<Result<_, _>>()?;
    let paths: BTreeSet<_> = std::iter::once(&config)
        .chain(&extra_paths)
        .map(|path| checked_file_path(path, &repo_path, &git_repo, &extra_paths))
        .collect::<Result<_, _>>()?;
    let _file_locks = lock_files(paths.iter())?;
    let mut files = Vec::new();
    for path in paths {
        files.push(FileChange {
            before: read_journal_file(&path)?,
            path,
            after: Vec::new(),
            retire: false,
        });
    }
    let record = JournalRecord {
        version: 2,
        repo_path,
        origin_operation: repo.operation().id().hex(),
        begin_heads: repo
            .op_heads_store()
            .get_op_heads()
            .await
            .map_err(LocalStateError::other)?
            .iter()
            .map(|id| id.hex())
            .collect(),
        nonce: format!("{:032x}", rand::random::<u128>()),
        publication: Publication::Prepared,
        files,
        ref_stores: Vec::new(),
    };
    save_record(&path, &record)?;
    Ok(LocalStateTransaction {
        path,
        loader: repo.loader().clone(),
        extra_paths,
        _lock: lock,
    })
}

impl LocalStateTransaction {
    /// Enrolls this exact transaction, never an unrelated operation with similar state.
    pub fn bind_transaction(&self, transaction: &mut Transaction) -> Result<(), LocalStateError> {
        let record = load_record(&self.path)?;
        record.check_repo(transaction.base_repo().loader())?;
        record.require_prepared()?;
        let git_repo =
            get_git_repo(transaction.base_repo().store()).map_err(LocalStateError::other)?;
        if transaction.base_repo().operation().id().hex() != record.origin_operation
            || !journal_matches_store(&self.path, &git_repo)?
        {
            return Err(LocalStateError::Safety(
                "Local-state transaction has a different base operation or store".into(),
            ));
        }
        transaction.set_attribute(TRANSACTION_ATTRIBUTE.into(), record.nonce);
        Ok(())
    }

    /// Records reference CAS witnesses before applying a mutation.
    pub fn record_ref_edits(
        &self,
        git_repo: &gix::Repository,
        edits: &[RefEdit],
    ) -> Result<(), LocalStateError> {
        if !journal_matches_store(&self.path, git_repo)? {
            return Err(LocalStateError::Safety(
                "Reference mutation belongs to a different store".into(),
            ));
        }
        let mut record = load_record(&self.path)?;
        record.require_prepared()?;
        record_ref_edits_inner(git_repo, &mut record, edits)?;
        save_record(&self.path, &record)
    }

    /// Records metadata that may be retired only after proven operation publication.
    pub fn register_retirements(
        &self,
        paths: &[PathBuf],
        refs: &[String],
    ) -> Result<(), LocalStateError> {
        let mut record = load_record(&self.path)?;
        record.require_prepared()?;
        let git_repo = get_git_repo(self.loader.store()).map_err(LocalStateError::other)?;
        for path in paths {
            let path = checked_file_path(path, &record.repo_path, &git_repo, &self.extra_paths)?;
            let change = record
                .files
                .iter_mut()
                .find(|change| change.path == path)
                .ok_or_else(|| {
                    LocalStateError::Safety("Retirement lacks a prepared file witness".into())
                })?;
            change.retire = true;
        }
        let mut locations = reference_locations(&record, &git_repo)?;
        for name in refs {
            let name: FullName = name.as_str().try_into().map_err(LocalStateError::other)?;
            let location = reference_location(&git_repo, &name);
            if let Some(&(store, change)) = locations.get(&location) {
                record.ref_stores[store].changes[change].retire = true;
            } else {
                let before = read_ref(&git_repo, &name)?;
                let (index, name) = record.ref_store_key(&git_repo, &name)?;
                let store = &mut record.ref_stores[index];
                locations.insert(location, (index, store.changes.len()));
                store.changes.push(RefChange {
                    name,
                    after: before.clone(),
                    before,
                    previous: Vec::new(),
                    retire: true,
                });
            }
        }
        save_record(&self.path, &record)
    }

    /// Durably commits a local-only change after all its mutations have succeeded.
    /// Retirement always requires an operation and cannot use this decision.
    pub fn commit_local(&self) -> Result<(), LocalStateError> {
        let mut record = load_record(&self.path)?;
        record.check_repo(&self.loader)?;
        record.require_prepared()?;
        if record.files.iter().any(|change| change.retire)
            || record
                .ref_stores
                .iter()
                .any(|store| store.changes.iter().any(|change| change.retire))
        {
            return Err(LocalStateError::Safety(
                "Retirements require a published operation".into(),
            ));
        }
        let owner = get_git_repo(self.loader.store()).map_err(LocalStateError::other)?;
        validate_file_paths(&record, &owner, &self.extra_paths)?;
        let stores = open_ref_stores(&record, &owner)?;
        let _file_locks = lock_files(record.files.iter().map(|change| &change.path))?;
        validate_files(&record, RecoveryAction::CheckFinal)?;
        let mut transactions = Vec::new();
        for (git_repo, store) in stores.iter().zip(&record.ref_stores) {
            let edits = reference_edits(git_repo, store, RecoveryAction::CheckFinal)?;
            transactions.push(
                git_repo
                    .refs
                    .transaction()
                    .prepare(
                        edits,
                        gix::lock::acquire::Fail::Immediately,
                        gix::lock::acquire::Fail::Immediately,
                    )
                    .map_err(LocalStateError::other)?,
            );
        }
        record.publication = Publication::LocalCommitted;
        save_record(&self.path, &record)
    }

    /// Completes only a durable local decision or a proven published operation.
    pub async fn complete(self) -> Result<(), LocalStateError> {
        require_exclusive_heads(&self.loader)?;
        let _heads_lock = self
            .loader
            .op_heads_store()
            .lock()
            .await
            .map_err(LocalStateError::other)?;
        let mut record = load_record(&self.path)?;
        record.check_repo(&self.loader)?;
        if !establish_commit(&self.loader, &self.path, &mut record).await? {
            return Err(LocalStateError::Safety(
                "The enrolled operation has not been published; run `jj util recover`".into(),
            ));
        }
        finish(
            &self.loader,
            &self.path,
            &record,
            RecoveryAction::Forward,
            &self.extra_paths,
        )
    }
}

/// Persists the exact target before head publication. The caller must hold the
/// operation-heads lock; this hook must not acquire the journal lock in reverse order.
pub(crate) fn before_publish(repo: &ReadonlyRepo) -> Result<(), LocalStateError> {
    let nonce = repo
        .operation()
        .metadata()
        .attributes
        .get(TRANSACTION_ATTRIBUTE);
    let Ok(git_repo) = get_git_repo(repo.store()) else {
        if nonce.is_some() {
            return Err(LocalStateError::Safety(
                "Local-state publication has no Git store".into(),
            ));
        }
        return Ok(());
    };
    let path = journal_path(&git_repo);
    if !has_pending(&git_repo)? {
        return if nonce.is_none() {
            Ok(())
        } else {
            Err(LocalStateError::Safety(
                "Local-state publication has no matching journal".into(),
            ))
        };
    }
    require_exclusive_heads(repo.loader())?;
    let mut record = load_record(&path)?;
    record.check_repo(repo.loader())?;
    if nonce != Some(&record.nonce)
        || !repo
            .operation()
            .parent_ids()
            .iter()
            .any(|id| id.hex() == record.origin_operation)
    {
        return Err(LocalStateError::Safety(
            "An unrelated operation cannot publish while local-state recovery is pending".into(),
        ));
    }
    match &record.publication {
        Publication::Prepared => {}
        Publication::OperationPrepared { target } if target == &repo.operation().id().hex() => {
            return Ok(());
        }
        _ => {
            return Err(LocalStateError::Safety(
                "Local-state journal already has a different commit decision".into(),
            ));
        }
    }
    record.publication = Publication::OperationPrepared {
        target: repo.operation().id().hex(),
    };
    save_record(&path, &record)
}

/// Makes a successfully published decision irrevocable. The caller holds the
/// operation-heads lock and has just successfully updated the published heads.
pub(crate) fn after_publish(repo: &ReadonlyRepo) -> Result<(), LocalStateError> {
    let nonce = repo
        .operation()
        .metadata()
        .attributes
        .get(TRANSACTION_ATTRIBUTE);
    let Ok(git_repo) = get_git_repo(repo.store()) else {
        return Ok(());
    };
    if !has_pending(&git_repo)? {
        return if nonce.is_none() {
            Ok(())
        } else {
            Err(LocalStateError::Safety(
                "Published local-state operation has no journal".into(),
            ))
        };
    }
    require_exclusive_heads(repo.loader())?;
    let path = journal_path(&git_repo);
    let mut record = load_record(&path)?;
    record.check_repo(repo.loader())?;
    if nonce != Some(&record.nonce) {
        return Err(LocalStateError::Safety(
            "Published operation is not enrolled in the journal".into(),
        ));
    }
    match &record.publication {
        Publication::OperationPrepared { target } if target == &repo.operation().id().hex() => {}
        Publication::Published { operation_id } if operation_id == &repo.operation().id().hex() => {
            return Ok(());
        }
        _ => {
            return Err(LocalStateError::Safety(
                "Published operation does not match the prepared target".into(),
            ));
        }
    }
    record.publication = Publication::Published {
        operation_id: repo.operation().id().hex(),
    };
    save_record(&path, &record)
}

/// Result of deterministic recovery against actual published operation heads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryOutcome {
    /// There was no recovery fence.
    NoPending,
    /// No enrolled operation was published, so preimages were restored.
    RolledBack,
    /// The committed change was verified and its retirements completed.
    Completed,
}

async fn establish_commit(
    loader: &RepoLoader,
    path: &Path,
    record: &mut JournalRecord,
) -> Result<bool, LocalStateError> {
    require_exclusive_heads(loader)?;
    let target = match &record.publication {
        Publication::LocalCommitted | Publication::Published { .. } => return Ok(true),
        Publication::Prepared => return Ok(false),
        Publication::OperationPrepared { target } => {
            OperationId::try_from_hex(target).ok_or_else(|| {
                LocalStateError::Safety("Invalid publication target in local-state journal".into())
            })?
        }
    };
    let heads = loader
        .op_heads_store()
        .get_op_heads()
        .await
        .map_err(LocalStateError::other)?;
    if heads.iter().map(|id| id.hex()).collect::<BTreeSet<_>>() == record.begin_heads {
        return Ok(false);
    }
    let mut operations = Vec::with_capacity(heads.len());
    for id in heads {
        operations.push(
            loader
                .load_operation(&id)
                .await
                .map_err(LocalStateError::other)?,
        );
    }
    let ancestors = crate::op_walk::walk_ancestors(&operations);
    futures::pin_mut!(ancestors);
    while let Some(operation) = ancestors.try_next().await.map_err(LocalStateError::other)? {
        if operation.id() == &target {
            if operation.metadata().attributes.get(TRANSACTION_ATTRIBUTE) != Some(&record.nonce)
                || !operation
                    .parent_ids()
                    .iter()
                    .any(|id| id.hex() == record.origin_operation)
            {
                return Err(LocalStateError::Safety(
                    "Published operation does not carry the journal's enrollment proof".into(),
                ));
            }
            record.publication = Publication::Published {
                operation_id: target.hex(),
            };
            save_record(path, record)?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Recovers locally, without selecting a view, fetching, or publishing operations.
/// Additional external files must be freshly authorized by the caller; recorded
/// file paths never grant recovery authority by themselves.
pub async fn recover(
    loader: &RepoLoader,
    extra_paths: &[PathBuf],
) -> Result<RecoveryOutcome, LocalStateError> {
    require_exclusive_heads(loader)?;
    let git_repo = get_git_repo(loader.store()).map_err(LocalStateError::other)?;
    let path = journal_path(&git_repo);
    let _lock = lock_journal(&path)?;
    if !has_pending(&git_repo)? {
        return Ok(RecoveryOutcome::NoPending);
    }
    let extra_paths: Vec<_> = extra_paths
        .iter()
        .map(|path| canonical_file_path(path))
        .collect::<Result<_, _>>()?;
    let _heads_lock = loader
        .op_heads_store()
        .lock()
        .await
        .map_err(LocalStateError::other)?;
    let mut record = load_record(&path)?;
    record.check_repo(loader)?;
    let committed = establish_commit(loader, &path, &mut record).await?;
    if !committed {
        let heads: BTreeSet<_> = loader
            .op_heads_store()
            .get_op_heads()
            .await
            .map_err(LocalStateError::other)?
            .iter()
            .map(|id| id.hex())
            .collect();
        if heads != record.begin_heads {
            return Err(LocalStateError::Safety(
                "Operation heads changed without publication of the enrolled transaction; \
                 refusing ambiguous local-state recovery"
                    .into(),
            ));
        }
    }
    finish(
        loader,
        &path,
        &record,
        if committed {
            RecoveryAction::Forward
        } else {
            RecoveryAction::Rollback
        },
        &extra_paths,
    )?;
    Ok(if committed {
        RecoveryOutcome::Completed
    } else {
        RecoveryOutcome::RolledBack
    })
}

fn lock_files<'a>(
    paths: impl IntoIterator<Item = &'a PathBuf>,
) -> Result<Vec<gix::lock::Marker>, LocalStateError> {
    paths
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|path| {
            gix::lock::Marker::acquire_to_hold_resource(
                path,
                gix::lock::acquire::Fail::Immediately,
                None,
            )
            .map_err(LocalStateError::other)
        })
        .collect()
}

#[derive(Clone, Copy)]
enum RecoveryAction {
    Rollback,
    Forward,
    CheckFinal,
}

fn validate_files(record: &JournalRecord, action: RecoveryAction) -> Result<(), LocalStateError> {
    for change in &record.files {
        let current = read_journal_file(&change.path)?;
        let valid = match action {
            RecoveryAction::CheckFinal => current.as_deref() == change.final_contents(),
            RecoveryAction::Rollback => current == change.before || change.after.contains(&current),
            RecoveryAction::Forward => {
                current == change.before
                    || change.after.contains(&current)
                    || (change.retire && current.is_none())
            }
        };
        if !valid {
            return Err(LocalStateError::Safety(format!(
                "File {} changed outside the prepared transaction; refusing recovery",
                change.path.display()
            )));
        }
    }
    Ok(())
}

fn reference_edits(
    git_repo: &gix::Repository,
    store: &RefStoreChanges,
    action: RecoveryAction,
) -> Result<Vec<RefEdit>, LocalStateError> {
    store
        .changes
        .iter()
        .map(|change| {
            let current = read_ref(git_repo, &change.name)?;
            let valid = match action {
                RecoveryAction::CheckFinal => current == change.after,
                RecoveryAction::Rollback => {
                    current == change.before
                        || current == change.after
                        || change.previous.contains(&current)
                }
                RecoveryAction::Forward => {
                    current == change.before
                        || current == change.after
                        || change.previous.contains(&current)
                        || (change.retire && current.is_none())
                }
            };
            if !valid {
                return Err(LocalStateError::Safety(format!(
                    "Reference {} changed outside the prepared transaction; refusing recovery",
                    change.name.as_bstr()
                )));
            }
            let target = match action {
                RecoveryAction::Rollback => change.before.as_ref(),
                RecoveryAction::Forward if change.retire => None,
                RecoveryAction::Forward | RecoveryAction::CheckFinal => change.after.as_ref(),
            };
            Ok(ref_edit(&change.name, current.as_ref(), target))
        })
        .collect()
}

// Every file and every worktree/namespace ref store is validated and locked
// before the first write. Partial commits are retryable from recorded witnesses.
fn finish(
    loader: &RepoLoader,
    path: &Path,
    record: &JournalRecord,
    action: RecoveryAction,
    extra_paths: &[PathBuf],
) -> Result<(), LocalStateError> {
    let owner = get_git_repo(loader.store()).map_err(LocalStateError::other)?;
    validate_file_paths(record, &owner, extra_paths)?;
    let stores = open_ref_stores(record, &owner)?;
    let _file_locks = lock_files(record.files.iter().map(|change| &change.path))?;
    validate_files(record, action)?;
    let mut transactions = Vec::new();
    for (git_repo, store) in stores.iter().zip(&record.ref_stores) {
        let edits = reference_edits(git_repo, store, action)?;
        transactions.push(
            git_repo
                .refs
                .transaction()
                .prepare(
                    edits,
                    gix::lock::acquire::Fail::Immediately,
                    gix::lock::acquire::Fail::Immediately,
                )
                .map_err(LocalStateError::other)?,
        );
    }
    for change in &record.files {
        let contents = match action {
            RecoveryAction::Rollback => change.before.as_deref(),
            RecoveryAction::Forward if change.retire => None,
            RecoveryAction::Forward | RecoveryAction::CheckFinal => change.final_contents(),
        };
        if read_journal_file(&change.path)?.as_deref() == contents {
            continue;
        }
        if let Some(contents) = contents {
            let mut replacement = tempfile::NamedTempFile::new_in(change.path.parent().unwrap())
                .map_err(LocalStateError::other)?;
            std::io::Write::write_all(replacement.as_file_mut(), contents)
                .map_err(LocalStateError::other)?;
            replacement
                .as_file()
                .sync_all()
                .map_err(LocalStateError::other)?;
            replacement
                .persist(&change.path)
                .map_err(LocalStateError::other)?;
        } else {
            std::fs::remove_file(&change.path).map_err(LocalStateError::other)?;
        }
        sync_parent(&change.path)?;
    }
    for (git_repo, transaction) in stores.iter().zip(transactions) {
        transaction
            .commit(
                git_repo
                    .committer()
                    .transpose()
                    .map_err(LocalStateError::other)?,
            )
            .map_err(LocalStateError::other)?;
    }
    std::fs::remove_file(path).map_err(LocalStateError::other)?;
    sync_parent(path)
}

fn sync_parent(path: &Path) -> Result<(), LocalStateError> {
    File::open(path.parent().unwrap())
        .and_then(|file| file.sync_all())
        .map_err(LocalStateError::other)
}

fn save_record(path: &Path, record: &JournalRecord) -> Result<(), LocalStateError> {
    let mut replacement =
        tempfile::NamedTempFile::new_in(path.parent().unwrap()).map_err(LocalStateError::other)?;
    serde_json::to_writer(replacement.as_file_mut(), record).map_err(LocalStateError::other)?;
    replacement
        .as_file()
        .sync_all()
        .map_err(LocalStateError::other)?;
    replacement.persist(path).map_err(LocalStateError::other)?;
    File::open(path.parent().unwrap())
        .and_then(|file| file.sync_all())
        .map_err(LocalStateError::other)
}

fn read_journal_file(path: &Path) -> Result<Option<Vec<u8>>, LocalStateError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(LocalStateError::other(err)),
    }
}

fn read_ref(
    git_repo: &gix::Repository,
    name: &FullName,
) -> Result<Option<Target>, LocalStateError> {
    // High-level namespace lookup strips prefixes from symbolic targets. CAS
    // witnesses need the raw target that gix actually compares and writes.
    match std::fs::read(git_repo.refs.reference_path(name.as_ref())) {
        Ok(bytes) => Ok(Some(
            gix::refs::file::loose::Reference::try_from_path(
                name.clone(),
                &bytes,
                git_repo.object_hash(),
            )
            .map_err(LocalStateError::other)?
            .target,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(git_repo
            .try_find_reference(name)
            .map_err(LocalStateError::other)?
            .map(|reference| reference.target().into_owned())),
        Err(error) => Err(LocalStateError::other(error)),
    }
}

fn ref_edit(name: &FullName, current: Option<&Target>, target: Option<&Target>) -> RefEdit {
    let expected = current.cloned().map_or(
        PreviousValue::MustNotExist,
        PreviousValue::MustExistAndMatch,
    );
    if current == target {
        RefEdit::verify(name.clone(), expected)
    } else if let Some(target) = target {
        RefEdit::update(
            name.clone(),
            target.clone(),
            expected,
            "recover local state",
        )
    } else {
        RefEdit::delete(name.clone(), expected)
    }
}

fn reference_location(git_repo: &gix::Repository, name: &FullName) -> PathBuf {
    crate::file_util::normalize_path(&git_repo.refs.reference_path(name.as_ref()))
}

fn reference_locations(
    record: &JournalRecord,
    owner: &gix::Repository,
) -> Result<BTreeMap<PathBuf, (usize, usize)>, LocalStateError> {
    let stores = open_ref_stores(record, owner)?;
    let mut locations = BTreeMap::new();
    for (store_index, (git_repo, store)) in stores.iter().zip(&record.ref_stores).enumerate() {
        for (change_index, change) in store.changes.iter().enumerate() {
            if locations
                .insert(
                    reference_location(git_repo, &change.name),
                    (store_index, change_index),
                )
                .is_some()
            {
                return Err(LocalStateError::Safety(
                    "Journal contains duplicate physical reference witnesses".into(),
                ));
            }
        }
    }
    Ok(locations)
}

fn record_ref_edits_inner<'a>(
    git_repo: &gix::Repository,
    record: &mut JournalRecord,
    edits: impl IntoIterator<Item = &'a RefEdit>,
) -> Result<(), LocalStateError> {
    let mut edits = edits
        .into_iter()
        .filter(|edit| !matches!(edit.change, Change::Verify { .. }))
        .peekable();
    if edits.peek().is_none() {
        return Ok(());
    }
    let mut locations = reference_locations(record, git_repo)?;
    for edit in edits {
        let after = match &edit.change {
            Change::Update { new, .. } => Some(new.clone()),
            Change::Delete { .. } => None,
            Change::Verify { .. } => continue,
        };
        let location = reference_location(git_repo, &edit.name);
        if let Some(&(store_index, change_index)) = locations.get(&location) {
            let change = &mut record.ref_stores[store_index].changes[change_index];
            if read_ref(git_repo, &edit.name)? != change.after {
                return Err(LocalStateError::Safety(format!(
                    "Ref {} changed between prepared local-state steps; refusing to absorb an \
                     external update",
                    edit.name.as_bstr()
                )));
            }
            change
                .previous
                .push(std::mem::replace(&mut change.after, after));
        } else {
            let before = read_ref(git_repo, &edit.name)?;
            let (index, name) = record.ref_store_key(git_repo, &edit.name)?;
            let store = &mut record.ref_stores[index];
            locations.insert(location, (index, store.changes.len()));
            store.changes.push(RefChange {
                name,
                before,
                after,
                previous: Vec::new(),
                retire: false,
            });
        }
    }
    Ok(())
}

pub(crate) fn record_mutation<'a>(
    git_repo: &gix::Repository,
    edits: impl IntoIterator<Item = &'a RefEdit>,
    files: &[(PathBuf, Vec<u8>)],
) -> Result<(), LocalStateError> {
    if !has_pending(git_repo)? {
        return Ok(());
    }
    let path = journal_path(git_repo);
    let mut record = load_record(&path)?;
    record.require_prepared()?;
    record_ref_edits_inner(git_repo, &mut record, edits)?;
    for (file, contents) in files {
        let file = canonical_file_path(file)?;
        let change = record
            .files
            .iter_mut()
            .find(|change| change.path == file)
            .ok_or_else(|| {
                LocalStateError::Safety(format!(
                    "File {} was not included in the prepared journal",
                    file.display()
                ))
            })?;
        change.record(Some(contents.clone()))?;
    }
    save_record(&path, &record)
}

/// Records file updates (including deletions) under the caller's writer locks.
/// Newly encountered files capture their preimage before the first mutation.
pub(crate) fn record_file_mutations(
    git_repo: &gix::Repository,
    files: &[(PathBuf, Option<Vec<u8>>)],
) -> Result<(), LocalStateError> {
    if !has_pending(git_repo)? {
        return Ok(());
    }
    let path = journal_path(git_repo);
    let mut record = load_record(&path)?;
    if !matches!(
        record.publication,
        Publication::Prepared | Publication::Published { .. }
    ) {
        return Err(LocalStateError::Safety(
            "File mutation is not allowed after this local-state decision".into(),
        ));
    }
    for (file, contents) in files {
        let file = canonical_file_path(file)?;
        if let Some(change) = record.files.iter_mut().find(|change| change.path == file) {
            change.record(contents.clone())?;
        } else {
            checked_file_path(&file, &record.repo_path, git_repo, &[])?;
            let before = read_journal_file(&file)?;
            record.files.push(FileChange {
                path: file,
                before,
                after: vec![contents.clone()],
                retire: false,
            });
        }
    }
    save_record(&path, &record)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::StackedConfig;
    use crate::git_backend::GitBackend;
    use crate::op_store::RefTarget;
    use crate::ref_name::RefName;
    use crate::settings::UserSettings;
    use crate::signing::Signer;

    fn read_journal_ref(
        git_repo: &gix::Repository,
        name: &str,
    ) -> Result<Option<Target>, LocalStateError> {
        read_ref(git_repo, &name.try_into().map_err(LocalStateError::other)?)
    }

    fn journal_ref_edit(
        name: &str,
        current: Option<&Target>,
        target: Option<&Target>,
    ) -> Result<RefEdit, LocalStateError> {
        Ok(ref_edit(
            &name.try_into().map_err(LocalStateError::other)?,
            current,
            target,
        ))
    }

    #[derive(Debug)]
    struct OptionalLockHeads(Arc<dyn crate::op_heads_store::OpHeadsStore>);

    struct NoopHeadsLock;

    impl crate::op_heads_store::OpHeadsStoreLock for NoopHeadsLock {}

    #[async_trait::async_trait]
    impl crate::op_heads_store::OpHeadsStore for OptionalLockHeads {
        fn name(&self) -> &str {
            self.0.name()
        }

        async fn update_op_heads(
            &self,
            old: &[OperationId],
            new: &OperationId,
        ) -> Result<(), crate::op_heads_store::OpHeadsStoreError> {
            self.0.update_op_heads(old, new).await
        }

        async fn get_op_heads(
            &self,
        ) -> Result<Vec<OperationId>, crate::op_heads_store::OpHeadsStoreError> {
            self.0.get_op_heads().await
        }

        async fn lock(
            &self,
        ) -> Result<
            Box<dyn crate::op_heads_store::OpHeadsStoreLock + '_>,
            crate::op_heads_store::OpHeadsStoreError,
        > {
            Ok(Box::new(NoopHeadsLock))
        }
    }

    struct Fixture {
        directory: tempfile::TempDir,
        repo: Arc<ReadonlyRepo>,
        git: gix::Repository,
    }

    impl Fixture {
        async fn new() -> Self {
            Self::with_git(None).await
        }

        async fn with_git(git_path: Option<&Path>) -> Self {
            let directory = crate::tests::new_temp_dir();
            let settings = UserSettings::from_config(StackedConfig::with_defaults()).unwrap();
            let repo = ReadonlyRepo::init(
                &settings,
                directory.path(),
                &|settings, path| {
                    let backend = if let Some(git_path) = git_path {
                        GitBackend::init_external(settings, path, git_path)?
                    } else {
                        GitBackend::init_internal(settings, path, gix::hash::Kind::default())?
                    };
                    Ok(Box::new(backend))
                },
                Signer::from_settings(&settings).unwrap(),
                ReadonlyRepo::default_op_store_initializer(),
                ReadonlyRepo::default_op_heads_store_initializer(),
                ReadonlyRepo::default_index_store_initializer(),
                ReadonlyRepo::default_submodule_store_initializer(),
            )
            .await
            .unwrap();
            let mut git = get_git_repo(repo.store()).unwrap();
            let config = git.config_path(gix::config::Source::Local).unwrap();
            let mut bytes = std::fs::read(&config).unwrap();
            bytes.extend_from_slice(
                b"\n[user]\nname = Local State Test\nemail = local-state@example.com\n",
            );
            std::fs::write(config, bytes).unwrap();
            git.reload().unwrap();
            Self {
                directory,
                repo,
                git,
            }
        }

        fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
            let path = self.directory.path().join(name);
            std::fs::write(&path, contents).unwrap();
            path
        }

        fn change_file(&self, path: &Path, contents: &[u8]) {
            record_file_mutations(&self.git, &[(path.to_owned(), Some(contents.to_vec()))])
                .unwrap();
            std::fs::write(path, contents).unwrap();
        }

        async fn prepare(&self, journal: &LocalStateTransaction) -> Arc<ReadonlyRepo> {
            let mut transaction = self.repo.start_transaction();
            journal.bind_transaction(&mut transaction).unwrap();
            transaction.repo_mut().set_local_bookmark_target(
                RefName::new("enrolled"),
                RefTarget::normal(self.repo.store().root_commit_id().clone()),
            );
            let unpublished = transaction
                .write("enrolled local-state change")
                .await
                .unwrap();
            let repo = self
                .repo
                .loader()
                .load_at(unpublished.operation())
                .await
                .unwrap();
            let _lock = repo.op_heads_store().lock().await.unwrap();
            before_publish(&repo).unwrap();
            repo.clone()
        }

        // Bypass the publication hooks only to reproduce a crash between the
        // head write and after_publish, or a later external head replacement.
        async fn publish_heads(&self, repo: &ReadonlyRepo) {
            let _lock = repo.op_heads_store().lock().await.unwrap();
            let heads = repo.op_heads_store().get_op_heads().await.unwrap();
            repo.op_heads_store()
                .update_op_heads(&heads, repo.operation().id())
                .await
                .unwrap();
        }

        fn ref_value(&self, contents: &[u8]) -> Target {
            Target::Object(self.git.write_blob(contents).unwrap().detach())
        }

        fn set_ref(&self, name: &str, value: Option<&Target>) {
            let current = read_journal_ref(&self.git, name).unwrap();
            self.git
                .edit_reference(journal_ref_edit(name, current.as_ref(), value).unwrap())
                .unwrap();
        }

        fn linked_git(&self) -> gix::Repository {
            let private = self.git.common_dir().join("worktrees/linked");
            let worktree = self.directory.path().join("linked-worktree");
            std::fs::create_dir_all(&private).unwrap();
            std::fs::create_dir(&worktree).unwrap();
            std::fs::write(private.join("commondir"), "../..\n").unwrap();
            std::fs::write(
                private.join("gitdir"),
                format!("{}\n", worktree.join(".git").display()),
            )
            .unwrap();
            std::fs::write(private.join("HEAD"), "ref: refs/heads/linked\n").unwrap();
            std::fs::write(
                worktree.join(".git"),
                format!("gitdir: {}\n", private.display()),
            )
            .unwrap();
            gix::open(private).unwrap()
        }
    }

    #[tokio::test]
    async fn prepared_operation_object_is_not_publication() {
        let fixture = Fixture::new().await;
        let file = fixture.file("metadata", b"before");
        let journal = begin(&fixture.repo, std::slice::from_ref(&file))
            .await
            .unwrap();
        fixture.change_file(&file, b"after");
        let _unpublished = fixture.prepare(&journal).await;
        assert!(journal.complete().await.is_err());
        assert_eq!(
            recover(fixture.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"before");
    }

    #[tokio::test]
    async fn published_target_is_proved_through_a_different_descendant_view() {
        let fixture = Fixture::new().await;
        let file = fixture.file("metadata", b"before");
        let journal = begin(&fixture.repo, std::slice::from_ref(&file))
            .await
            .unwrap();
        fixture.change_file(&file, b"after");
        journal
            .register_retirements(std::slice::from_ref(&file), &[])
            .unwrap();
        let published = fixture.prepare(&journal).await;
        fixture.publish_heads(&published).await;
        let mut descendant = published.start_transaction();
        descendant
            .repo_mut()
            .set_local_bookmark_target(RefName::new("enrolled"), RefTarget::absent());
        let descendant = descendant.write("later different state").await.unwrap();
        let descendant = fixture
            .repo
            .loader()
            .load_at(descendant.operation())
            .await
            .unwrap();
        fixture.publish_heads(&descendant).await;
        assert_ne!(
            descendant.operation().view_id(),
            published.operation().view_id()
        );
        drop(journal);
        assert_eq!(
            recover(fixture.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::Completed
        );
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn local_commit_preserves_configuration_without_an_operation() {
        let fixture = Fixture::new().await;
        let config = fixture.git.config_path(gix::config::Source::Local).unwrap();
        let before_heads = fixture.repo.op_heads_store().get_op_heads().await.unwrap();
        let journal = begin(&fixture.repo, &[]).await.unwrap();
        let mut contents = std::fs::read(&config).unwrap();
        contents.extend_from_slice(b"\n[remote \"local-only\"]\nurl = /new/url\n");
        fixture.change_file(&config, &contents);
        journal.commit_local().unwrap();
        drop(journal);
        assert_eq!(
            recover(fixture.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::Completed
        );
        assert_eq!(std::fs::read(&config).unwrap(), contents);
        assert_eq!(
            fixture.repo.op_heads_store().get_op_heads().await.unwrap(),
            before_heads
        );
    }

    #[tokio::test]
    async fn outside_file_change_blocks_every_recovery_write() {
        let fixture = Fixture::new().await;
        let first = fixture.file("a", b"first before");
        let second = fixture.file("b", b"second before");
        let journal = begin(&fixture.repo, &[first.clone(), second.clone()])
            .await
            .unwrap();
        fixture.change_file(&first, b"first after");
        fixture.change_file(&second, b"second after");
        std::fs::write(&second, b"external").unwrap();
        drop(journal);
        assert!(recover(fixture.repo.loader(), &[]).await.is_err());
        assert_eq!(std::fs::read(&first).unwrap(), b"first after");
        assert_eq!(std::fs::read(&second).unwrap(), b"external");
        assert!(has_pending(&fixture.git).unwrap());
        std::fs::write(&second, b"second after").unwrap();
        assert_eq!(
            recover(fixture.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(std::fs::read(&first).unwrap(), b"first before");
        assert_eq!(std::fs::read(&second).unwrap(), b"second before");
    }

    #[tokio::test]
    async fn outside_ref_change_blocks_retirement_until_repaired() {
        let fixture = Fixture::new().await;
        let file = fixture.file("metadata", b"before");
        let name = "refs/jj/local-state-test";
        let before = fixture.ref_value(b"before");
        let outside = fixture.ref_value(b"external");
        fixture.set_ref(name, Some(&before));
        let journal = begin(&fixture.repo, std::slice::from_ref(&file))
            .await
            .unwrap();
        journal
            .register_retirements(std::slice::from_ref(&file), &[name.into()])
            .unwrap();
        let published = fixture.prepare(&journal).await;
        fixture.publish_heads(&published).await;
        fixture.set_ref(name, Some(&outside));
        drop(journal);
        assert!(recover(fixture.repo.loader(), &[]).await.is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"before");
        assert_eq!(read_journal_ref(&fixture.git, name).unwrap(), Some(outside));
        fixture.set_ref(name, Some(&before));
        assert_eq!(
            recover(fixture.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::Completed
        );
        assert!(!file.exists());
        assert_eq!(read_journal_ref(&fixture.git, name).unwrap(), None);
    }

    #[tokio::test]
    async fn interrupted_rollback_accepts_restored_and_intermediate_witnesses() {
        let fixture = Fixture::new().await;
        let first = fixture.file("a", b"first before");
        let second = fixture.file("b", b"second before");
        let name = "refs/jj/local-state-test";
        let original = fixture.ref_value(b"original");
        let intermediate = fixture.ref_value(b"intermediate");
        let final_value = fixture.ref_value(b"final");
        fixture.set_ref(name, Some(&original));
        let journal = begin(&fixture.repo, &[first.clone(), second.clone()])
            .await
            .unwrap();
        fixture.change_file(&first, b"first after");
        fixture.change_file(&second, b"second intermediate");
        fixture.change_file(&second, b"second after");
        for value in [&intermediate, &final_value] {
            let current = read_journal_ref(&fixture.git, name).unwrap();
            let edit = journal_ref_edit(name, current.as_ref(), Some(value)).unwrap();
            journal
                .record_ref_edits(&fixture.git, std::slice::from_ref(&edit))
                .unwrap();
            fixture.git.edit_reference(edit).unwrap();
        }
        // Simulate interruption after a prefix of rollback has been restored.
        std::fs::write(&first, b"first before").unwrap();
        std::fs::write(&second, b"second intermediate").unwrap();
        fixture.set_ref(name, Some(&intermediate));
        drop(journal);
        assert_eq!(
            recover(fixture.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(std::fs::read(&first).unwrap(), b"first before");
        assert_eq!(std::fs::read(&second).unwrap(), b"second before");
        assert_eq!(
            read_journal_ref(&fixture.git, name).unwrap(),
            Some(original)
        );
    }

    #[tokio::test]
    async fn interrupted_retirement_remains_committed_after_head_rewind() {
        let fixture = Fixture::new().await;
        let first = fixture.file("a", b"first");
        let second = fixture.file("b", b"second");
        let journal = begin(&fixture.repo, &[first.clone(), second.clone()])
            .await
            .unwrap();
        journal
            .register_retirements(&[first.clone(), second.clone()], &[])
            .unwrap();
        let published = fixture.prepare(&journal).await;
        fixture.publish_heads(&published).await;
        // Recovery observed the published target before crashing mid-retirement.
        {
            let _lock = fixture.repo.op_heads_store().lock().await.unwrap();
            let mut record = load_record(&journal.path).unwrap();
            assert!(
                establish_commit(fixture.repo.loader(), &journal.path, &mut record)
                    .await
                    .unwrap()
            );
        }
        std::fs::remove_file(&first).unwrap();
        fixture.publish_heads(&fixture.repo).await;
        drop(journal);
        assert_eq!(
            recover(fixture.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::Completed
        );
        assert!(!first.exists());
        assert!(!second.exists());
    }

    #[tokio::test]
    async fn published_file_step_is_replayed_after_record_before_write_crash() {
        let fixture = Fixture::new().await;
        let index = fixture.file("index-witness", b"before");
        let journal = begin(&fixture.repo, &[]).await.unwrap();
        let published = fixture.prepare(&journal).await;
        fixture.publish_heads(&published).await;
        {
            let _lock = fixture.repo.op_heads_store().lock().await.unwrap();
            after_publish(&published).unwrap();
        }
        record_file_mutations(&fixture.git, &[(index.clone(), Some(b"after".to_vec()))]).unwrap();
        drop(journal);
        assert_eq!(
            recover(fixture.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::Completed
        );
        assert_eq!(std::fs::read(index).unwrap(), b"after");
    }

    #[tokio::test]
    async fn published_reference_step_finishes_from_its_preimage() {
        let fixture = Fixture::new().await;
        let name = "refs/jj/local-state-test";
        let before = fixture.ref_value(b"before");
        let after = fixture.ref_value(b"after");
        fixture.set_ref(name, Some(&before));
        let journal = begin(&fixture.repo, &[]).await.unwrap();
        let edit = journal_ref_edit(name, Some(&before), Some(&after)).unwrap();
        journal.record_ref_edits(&fixture.git, &[edit]).unwrap();
        // The prepared Git write did not run, but the enrolled operation did
        // publish. Recovery must finish its exact recorded CAS, not roll back.
        let published = fixture.prepare(&journal).await;
        fixture.publish_heads(&published).await;
        drop(journal);
        assert_eq!(
            recover(fixture.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::Completed
        );
        assert_eq!(read_journal_ref(&fixture.git, name).unwrap(), Some(after));
    }

    #[tokio::test]
    async fn writer_lock_blocks_recovery_and_local_commit_forbids_retirement() {
        let fixture = Fixture::new().await;
        let file = fixture.file("metadata", b"before");
        let journal = begin(&fixture.repo, std::slice::from_ref(&file))
            .await
            .unwrap();
        journal
            .register_retirements(std::slice::from_ref(&file), &[])
            .unwrap();
        assert!(journal.commit_local().is_err());
        drop(journal);
        let lock = gix::lock::Marker::acquire_to_hold_resource(
            &file,
            gix::lock::acquire::Fail::Immediately,
            None,
        )
        .unwrap();
        assert!(recover(fixture.repo.loader(), &[]).await.is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"before");
        assert!(has_pending(&fixture.git).unwrap());
        drop(lock);
        assert_eq!(
            recover(fixture.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(std::fs::read(file).unwrap(), b"before");
    }

    #[tokio::test]
    async fn legacy_journal_is_rejected_without_touching_its_files() {
        let fixture = Fixture::new().await;
        let file = fixture.file("metadata", b"external");
        let path = journal_path(&fixture.git);
        let legacy = serde_json::json!({
            "operation_id": fixture.repo.operation().id().hex(),
            "files": [[file, [98, 101, 102, 111, 114, 101], [[97, 102, 116, 101, 114]]]],
            "refs": [],
            "requires_new_operation": true
        });
        let bytes = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            recover(fixture.repo.loader(), &[]).await,
            Err(LocalStateError::LegacyJournal)
        ));
        assert_eq!(std::fs::read(&file).unwrap(), b"external");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[tokio::test]
    async fn unproven_target_with_changed_heads_is_ambiguous() {
        let fixture = Fixture::new().await;
        let file = fixture.file("metadata", b"before");
        let journal = begin(&fixture.repo, std::slice::from_ref(&file))
            .await
            .unwrap();
        fixture.change_file(&file, b"after");
        let _unpublished = fixture.prepare(&journal).await;
        let unrelated = fixture
            .repo
            .start_transaction()
            .write("unrelated")
            .await
            .unwrap();
        let unrelated = fixture
            .repo
            .loader()
            .load_at(unrelated.operation())
            .await
            .unwrap();
        fixture.publish_heads(&unrelated).await;
        drop(journal);
        assert!(recover(fixture.repo.loader(), &[]).await.is_err());
        assert_eq!(std::fs::read(file).unwrap(), b"after");
        assert!(has_pending(&fixture.git).unwrap());
    }

    #[tokio::test]
    async fn shared_git_store_does_not_share_journal_authority() {
        let owner = Fixture::new().await;
        let other = Fixture::with_git(Some(owner.git.path())).await;
        assert_eq!(owner.repo.operation().id(), other.repo.operation().id());
        assert_eq!(owner.git.common_dir(), other.git.common_dir());
        let file = owner.file("metadata", b"before");
        let journal = begin(&owner.repo, std::slice::from_ref(&file))
            .await
            .unwrap();
        owner.change_file(&file, b"after");
        let mut foreign_transaction = other.repo.start_transaction();
        assert!(journal.bind_transaction(&mut foreign_transaction).is_err());
        // Even copied enrollment metadata is insufficient without the same
        // authoritative JJ store. The shared root operation IDs are identical.
        foreign_transaction.set_attribute(
            TRANSACTION_ATTRIBUTE.into(),
            load_record(&journal.path).unwrap().nonce,
        );
        let unpublished = foreign_transaction
            .write("foreign publication")
            .await
            .unwrap();
        let foreign_repo = other
            .repo
            .loader()
            .load_at(unpublished.operation())
            .await
            .unwrap();
        {
            let _lock = other.repo.op_heads_store().lock().await.unwrap();
            let error = before_publish(&foreign_repo).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&owner.directory.path().display().to_string())
            );
            assert!(after_publish(&foreign_repo).is_err());
        }
        drop(journal);
        let error = recover(other.repo.loader(), &[]).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&owner.directory.path().display().to_string())
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"after");
        assert!(has_pending(&owner.git).unwrap());
        assert_eq!(
            recover(owner.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(std::fs::read(file).unwrap(), b"before");
    }

    #[tokio::test]
    async fn recovery_preserves_linked_head_and_deduplicates_shared_refs() {
        let fixture = Fixture::new().await;
        let linked = fixture.linked_git();
        let main_head = read_journal_ref(&fixture.git, "HEAD").unwrap();
        let linked_head = read_journal_ref(&linked, "HEAD").unwrap();
        let name = "refs/jj/shared";
        let before = fixture.ref_value(b"before");
        let intermediate = fixture.ref_value(b"intermediate");
        let after = fixture.ref_value(b"after");
        fixture.set_ref(name, Some(&before));
        let journal = begin(&fixture.repo, &[]).await.unwrap();
        for (git_repo, target) in [(&fixture.git, &intermediate), (&linked, &after)] {
            let current = read_journal_ref(git_repo, name).unwrap();
            let edit = journal_ref_edit(name, current.as_ref(), Some(target)).unwrap();
            journal
                .record_ref_edits(git_repo, std::slice::from_ref(&edit))
                .unwrap();
            git_repo.edit_reference(edit).unwrap();
        }
        let target = Target::Symbolic("refs/heads/changed".try_into().unwrap());
        let edit = journal_ref_edit("HEAD", linked_head.as_ref(), Some(&target)).unwrap();
        journal
            .record_ref_edits(&linked, std::slice::from_ref(&edit))
            .unwrap();
        linked.edit_reference(edit).unwrap();
        drop(journal);
        assert_eq!(
            recover(fixture.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(read_journal_ref(&fixture.git, name).unwrap(), Some(before));
        assert_eq!(read_journal_ref(&fixture.git, "HEAD").unwrap(), main_head);
        assert_eq!(read_journal_ref(&linked, "HEAD").unwrap(), linked_head);
    }

    #[tokio::test]
    async fn multiple_namespaces_recover_with_one_shared_packed_lock() {
        let fixture = Fixture::new().await;
        let name = "refs/jj/namespaced";
        let outside = fixture.ref_value(b"outside");
        fixture.set_ref(name, Some(&outside));
        let journal = begin(&fixture.repo, &[]).await.unwrap();
        let mut namespaced = Vec::new();
        for namespace in ["one", "two"] {
            let mut git_repo = gix::open(fixture.git.path()).unwrap();
            git_repo.set_namespace(namespace).unwrap();
            let target = fixture.ref_value(namespace.as_bytes());
            let edit = journal_ref_edit(name, None, Some(&target)).unwrap();
            journal
                .record_ref_edits(&git_repo, std::slice::from_ref(&edit))
                .unwrap();
            git_repo.edit_reference(edit).unwrap();
            namespaced.push(git_repo);
        }
        drop(journal);
        assert_eq!(
            recover(fixture.repo.loader(), &[]).await.unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(read_journal_ref(&fixture.git, name).unwrap(), Some(outside));
        for git_repo in namespaced {
            assert_eq!(read_journal_ref(&git_repo, name).unwrap(), None);
        }
    }

    #[tokio::test]
    async fn recovery_rejects_serialized_outside_file_targets() {
        let fixture = Fixture::new().await;
        let outside_dir = crate::tests::new_temp_dir();
        let outside = outside_dir.path().join("outside");
        std::fs::write(&outside, b"outside").unwrap();
        let journal = begin(&fixture.repo, &[]).await.unwrap();
        assert!(
            record_file_mutations(
                &fixture.git,
                &[(outside.clone(), Some(b"changed".to_vec()))]
            )
            .is_err()
        );
        let mut record = load_record(&journal.path).unwrap();
        record.files.push(FileChange {
            path: outside.clone(),
            before: Some(b"before".to_vec()),
            after: vec![Some(b"outside".to_vec())],
            retire: false,
        });
        save_record(&journal.path, &record).unwrap();
        drop(journal);
        assert!(recover(fixture.repo.loader(), &[]).await.is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"outside");
        assert!(has_pending(&fixture.git).unwrap());
    }

    #[tokio::test]
    async fn optional_operation_head_locks_cannot_authorize_local_state_recovery() {
        let fixture = Fixture::new().await;
        let real = fixture.repo.loader();
        let plugin = RepoLoader::new(
            real.settings().clone(),
            real.store().clone(),
            real.op_store().clone(),
            Arc::new(OptionalLockHeads(real.op_heads_store().clone())),
            real.index_store().clone(),
            real.submodule_store().clone(),
        );
        let plugin_repo = plugin.load_at(fixture.repo.operation()).await.unwrap();
        // Unjournaled plugin transactions retain the ordinary upstream contract.
        plugin_repo
            .start_transaction()
            .commit("ordinary plugin operation")
            .await
            .unwrap();
        assert!(matches!(
            begin(&plugin_repo, &[]).await,
            Err(LocalStateError::Safety(_))
        ));
        assert!(!has_pending(&fixture.git).unwrap());
        let file = fixture.file("metadata", b"before");
        let journal = begin(&fixture.repo, std::slice::from_ref(&file))
            .await
            .unwrap();
        fixture.change_file(&file, b"after");
        drop(journal);
        assert!(matches!(
            recover(&plugin, &[]).await,
            Err(LocalStateError::Safety(_))
        ));
        assert_eq!(std::fs::read(&file).unwrap(), b"after");
        assert!(has_pending(&fixture.git).unwrap());
        assert_eq!(
            recover(real, &[]).await.unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(std::fs::read(file).unwrap(), b"before");
    }

    #[tokio::test]
    async fn external_configuration_requires_fresh_exact_recovery_authority() {
        let fixture = Fixture::new().await;
        let config_dir = crate::tests::new_temp_dir();
        let config = config_dir.path().join("config.toml");
        std::fs::write(&config, b"before").unwrap();
        let live = begin(&fixture.repo, std::slice::from_ref(&config))
            .await
            .unwrap();
        record_mutation(&fixture.git, &[], &[(config.clone(), b"live".to_vec())]).unwrap();
        std::fs::write(&config, b"live").unwrap();
        live.commit_local().unwrap();
        live.complete().await.unwrap();
        assert_eq!(std::fs::read(&config).unwrap(), b"live");

        let interrupted = begin(&fixture.repo, std::slice::from_ref(&config))
            .await
            .unwrap();
        fixture.change_file(&config, b"committed");
        interrupted.commit_local().unwrap();
        drop(interrupted);
        assert!(recover(fixture.repo.loader(), &[]).await.is_err());
        let other_config = config_dir.path().join("other.toml");
        assert!(
            recover(fixture.repo.loader(), &[other_config])
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&config).unwrap(), b"committed");
        assert!(has_pending(&fixture.git).unwrap());
        assert_eq!(
            recover(fixture.repo.loader(), std::slice::from_ref(&config))
                .await
                .unwrap(),
            RecoveryOutcome::Completed,
        );
        assert_eq!(std::fs::read(config).unwrap(), b"committed");
    }
}
