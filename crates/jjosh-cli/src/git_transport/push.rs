use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs::File;
use std::io::{self, Seek, Write};

use anyhow::{Context, Result, bail, ensure};
use gix::ObjectId;
use gix::bstr::{BString, ByteSlice};
use gix::protocol::{self, push as wire, transport};
use gix_object::{Find, Kind, ObjectRef};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Expected {
    Unknown,
    Absent,
    At(ObjectId),
}

#[derive(Clone, Debug)]
pub(crate) struct Update {
    pub name: BString,
    pub expected: Expected,
    pub new: Option<ObjectId>,
}

#[derive(Default, Debug)]
pub(crate) struct Options {
    pub atomic: bool,
    pub push_options: Vec<BString>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RefStatus {
    /// Confirmed by receive-pack, or already at the authorized target in its advertisement.
    Accepted,
    Rejected(BString),
    /// Commands may have been applied, but no conclusive report was received.
    Indeterminate,
    /// Authorized and preflighted, but not yet sent.
    Planned,
}

#[derive(Debug)]
pub(crate) struct Outcome {
    /// Exact raw destination names, in the same order as the supplied updates.
    pub refs: Vec<(BString, RefStatus)>,
    /// A post-send failure does not invalidate independently confirmed per-ref reports.
    pub error: Option<anyhow::Error>,
}

/// An owned receive-pack session whose authorization and complete pack are fixed.
pub(crate) struct PreparedPush {
    transfer: Option<Transfer>,
    outcome: Outcome,
}

struct Transfer {
    transport: Box<dyn transport::client::blocking_io::Transport + Send>,
    handshake: protocol::Handshake,
    commands: Vec<wire::Command>,
    command_indices: Vec<usize>,
    options: wire::Options,
    pack: Option<File>,
}

/// Prepare raw objects and refs without changing remote refs, leases, or JJ state.
/// Every known rejection aborts the complete batch before a mutation request.
/// Resolves each authorization policy to the advertised expected-old used on wire.
pub(crate) fn prepare(
    repo: &gix::Repository,
    remote: gix::Remote<'_>,
    objects: &gix::OdbHandle,
    updates: &mut [Update],
    options: &Options,
) -> Result<PreparedPush> {
    let hash = repo.object_hash();
    let mut names = HashSet::with_capacity(updates.len());
    for update in updates.iter() {
        ensure!(
            update.name.starts_with(b"refs/"),
            "push destination must be fully qualified: {:?}",
            update.name
        );
        gix::validate::reference::name(update.name.as_bstr())
            .with_context(|| format!("invalid push destination {:?}", update.name))?;
        ensure!(
            names.insert(update.name.as_bstr()),
            "duplicate push destination {:?}",
            update.name
        );
        for id in update.new.iter().chain(match &update.expected {
            Expected::At(id) => Some(id),
            _ => None,
        }) {
            ensure!(
                id.kind() == hash && !id.is_null(),
                "invalid object ID {id} for push destination {:?}",
                update.name
            );
        }
    }
    if updates.is_empty() {
        return Ok(PreparedPush {
            transfer: None,
            outcome: Outcome {
                refs: Vec::new(),
                error: None,
            },
        });
    }

    // Receive-pack is a V1 service even when the repository's fetch protocol is V2.
    // Use the configured remote's URL policy, SSH settings, HTTP options, and credentials.
    let (url, _) = remote.sanitized_url_and_version(gix::remote::Direction::Push)?;
    let ssh = if url.scheme == gix::url::Scheme::Ssh {
        repo.ssh_connect_options()?
    } else {
        Default::default()
    };
    let transport_options = repo.transport_options(
        url.to_bstring().as_bstr(),
        remote.name().map(|name| name.as_bstr()),
    )?;
    let mut transport = transport::client::blocking_io::connect::connect(
        url,
        transport::client::blocking_io::connect::Options {
            version: transport::Protocol::V1,
            ssh,
            trace: repo
                .config_snapshot()
                .boolean("gitoxide.tracePacket")
                .unwrap_or_default(),
        },
    )?;
    let credentials = repo.configured_credentials_for_current_url();
    if let Some(config) = transport_options {
        transport.configure(&*config)?;
    }
    let handshake = protocol::handshake(
        &mut transport,
        transport::Service::ReceivePack,
        credentials,
        Vec::new(),
        &mut gix::progress::Discard,
    )?;
    ensure!(
        matches!(
            handshake.server_protocol_version,
            transport::Protocol::V0 | transport::Protocol::V1
        ),
        "receive-pack did not negotiate protocol V1"
    );

    let (advertised, remote_objects) = index_advertisement(
        handshake
            .refs
            .as_ref()
            .context("receive-pack omitted its ref advertisement")?,
        hash,
    )?;

    // A separate handle prevents replacement refs from changing raw object identity.
    let mut objects = objects.clone();
    objects.ignore_replacements = true;
    objects.prevent_pack_unload();
    let mut outcome = Outcome {
        refs: Vec::with_capacity(updates.len()),
        error: None,
    };
    let mut commands = Vec::new();
    let mut command_indices = Vec::new();
    for (index, update) in updates.iter_mut().enumerate() {
        let old = advertised.get(update.name.as_bstr()).copied().flatten();
        let rejection = match update.expected {
            Expected::Absent if old.is_some() => Some(BString::from(
                "stale lease: expected remote ref to be absent",
            )),
            Expected::At(expected) if old != Some(expected) => Some(BString::from(format!(
                "stale lease: expected remote ref at {expected}"
            ))),
            Expected::Unknown => match (old, update.new) {
                (_, None) => Some(BString::from(
                    "remote ref has no known lease; fetch it before deleting",
                )),
                (None, Some(_)) => None,
                (Some(old), Some(new)) if old == new => None,
                (Some(old), Some(new)) if update.name.starts_with(b"refs/heads/") => {
                    if is_ancestor(&objects, old, new)? {
                        None
                    } else {
                        Some(BString::from(
                            "remote ref has no known lease; fetch it before rewriting history",
                        ))
                    }
                }
                _ => Some(BString::from(
                    "remote ref has no known lease; fetch it before overwriting",
                )),
            },
            _ => None,
        };
        let status = if let Some(reason) = rejection {
            RefStatus::Rejected(reason)
        } else if old == update.new {
            RefStatus::Accepted
        } else {
            commands.push(wire::Command {
                name: update.name.clone(),
                old: old.unwrap_or_else(|| ObjectId::null(hash)),
                new: update.new.unwrap_or_else(|| ObjectId::null(hash)),
            });
            command_indices.push(index);
            RefStatus::Planned
        };
        outcome.refs.push((update.name.clone(), status));
        update.expected = old.map_or(Expected::Absent, Expected::At);
    }
    let rejected: Vec<_> = outcome
        .refs
        .iter()
        .filter_map(|(name, status)| match status {
            RefStatus::Rejected(reason) => Some(format!("{name}: {reason}")),
            _ => None,
        })
        .collect();
    ensure!(
        rejected.is_empty(),
        "Push preflight rejected references:\n{}",
        rejected.join("\n")
    );

    let wire_options = wire::Options {
        atomic: options.atomic,
        push_options: options.push_options.clone(),
    };
    let has_pack = commands.iter().any(|command| !command.new.is_null());
    wire::preflight(&handshake, &commands, &wire_options, has_pack)?;
    let pack = if has_pack {
        Some(prepare_pack(&objects, hash, &commands, &remote_objects)
            .context("Cannot prepare a complete publication pack; shallow sources require destination-owned ancestry or fetch --unshallow")?)
    } else {
        None
    };
    Ok(PreparedPush {
        transfer: Some(Transfer {
            transport,
            handshake,
            commands,
            command_indices,
            options: wire_options,
            pack,
        }),
        outcome,
    })
}

type AdvertisedRefs<'a> = HashMap<&'a gix::bstr::BStr, Option<ObjectId>>;

fn index_advertisement(
    refs: &[protocol::handshake::Ref],
    hash: gix::hash::Kind,
) -> Result<(AdvertisedRefs<'_>, HashSet<ObjectId>)> {
    let mut advertised = HashMap::new();
    let mut remote_objects = HashSet::new();
    for reference in refs {
        use protocol::handshake::Ref;
        let (_, direct, peeled) = reference.unpack();
        remote_objects.extend(direct.into_iter().chain(peeled).map(ToOwned::to_owned));
        let (name, id) = match reference {
            Ref::Direct {
                full_ref_name,
                object,
            } => (full_ref_name, Some(*object)),
            Ref::Peeled {
                full_ref_name, tag, ..
            } => (full_ref_name, Some(*tag)),
            Ref::Symbolic {
                full_ref_name,
                tag,
                object,
                ..
            } => (full_ref_name, Some(tag.unwrap_or(*object))),
            Ref::Unborn { full_ref_name, .. } => (full_ref_name, None),
        };
        if let Some(id) = id {
            ensure!(
                id.kind() == hash && !id.is_null(),
                "remote advertised an incompatible object ID for {name:?}"
            );
        }
        // receive-pack may advertise multiple pseudo refs named .have for
        // objects reachable through alternate namespaces or object stores.
        // They are object-presence hints, not mutable refs: retain their OIDs
        // for pack pruning, but exclude them from lease/ref-name accounting.
        if name.as_bstr() == b".have".as_bstr() {
            continue;
        }
        ensure!(
            advertised.insert(name.as_bstr(), id).is_none(),
            "remote advertised duplicate ref {name:?}"
        );
    }
    Ok((advertised, remote_objects))
}

impl PreparedPush {
    /// Send only the retained commands and pack on the original advertisement.
    pub(crate) fn publish(self) -> Outcome {
        let Self {
            transfer,
            mut outcome,
        } = self;
        let Some(Transfer {
            mut transport,
            handshake,
            commands,
            command_indices,
            options,
            mut pack,
        }) = transfer
        else {
            return outcome;
        };
        let has_pack = pack.is_some();
        // Graph traversal and encoding have already succeeded. This callback can
        // only stream the complete spool; it cannot discover new object failures.
        let mut copy_pack = |out: &mut dyn Write| -> io::Result<()> {
            io::copy(
                pack.as_mut()
                    .expect("pack callback only supplied with a pack"),
                out,
            )?;
            Ok(())
        };
        let report = match wire::execute(
            &mut transport,
            &handshake,
            &commands,
            &options,
            if has_pack { Some(&mut copy_pack) } else { None },
        ) {
            Ok(report) => report,
            Err(error) => {
                // execute's direct errors guarantee that no commands were sent.
                outcome.error = Some(error.into());
                return outcome;
            }
        };
        for index in &command_indices {
            outcome.refs[*index].1 = RefStatus::Indeterminate;
        }
        let indices: HashMap<_, _> = commands
            .iter()
            .zip(&command_indices)
            .map(|(command, index)| (command.name.as_bstr(), *index))
            .collect();
        for (name, status) in report.refs {
            if let Some(index) = indices.get(name.as_bstr()) {
                outcome.refs[*index].1 = match status {
                    wire::RefStatus::Accepted => RefStatus::Accepted,
                    wire::RefStatus::Rejected(reason) => RefStatus::Rejected(reason),
                    wire::RefStatus::Indeterminate => RefStatus::Indeterminate,
                };
            }
        }
        outcome.error = report.error.map(anyhow::Error::new);
        if outcome.error.is_none()
            && let Some(Err(reason)) = report.unpack
        {
            outcome.error = Some(anyhow::anyhow!("receive-pack unpack failed: {reason}"));
        }
        outcome
    }
}

fn checked_object<'a>(
    objects: &gix::OdbHandle,
    id: ObjectId,
    buffer: &'a mut Vec<u8>,
) -> Result<gix_object::Data<'a>> {
    let data = objects
        .try_find(id.as_ref(), buffer)?
        .with_context(|| format!("missing object {id} while preparing push"))?;
    ensure!(
        gix_object::compute_hash(id.kind(), data.kind, data.data)? == id,
        "corrupt object {id}: content hash mismatch"
    );
    Ok(data)
}

fn is_ancestor(objects: &gix::OdbHandle, ancestor: ObjectId, tip: ObjectId) -> Result<bool> {
    let mut pending = vec![tip];
    let mut visited = HashSet::new();
    let mut buffer = Vec::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let data = checked_object(objects, id, &mut buffer)?;
        if data.kind != Kind::Commit {
            return Ok(false);
        }
        let commit = gix_object::CommitRef::from_bytes(data.data, id.kind())
            .with_context(|| format!("corrupt commit {id} while proving fast-forward"))?;
        if id == ancestor {
            return Ok(true);
        }
        pending.extend(commit.parents());
    }
    Ok(false)
}

const WANT: u8 = 1;
const HAVE: u8 = 2;

struct WantCommit {
    tree: ObjectId,
    parents: Vec<ObjectId>,
}

fn commit_time(commit: &gix_object::CommitRef<'_>) -> i64 {
    commit
        .committer()
        .ok()
        .and_then(|sig| sig.time().ok())
        .map_or(0, |time| time.seconds)
}

fn propagate_have(
    start: &ObjectId,
    want_commits: &HashMap<ObjectId, WantCommit>,
    marks: &mut HashMap<ObjectId, u8>,
    unpopped_want: &mut HashSet<ObjectId>,
) {
    let mut stack = vec![*start];
    while let Some(id) = stack.pop() {
        let Some(info) = want_commits.get(&id) else {
            continue;
        };
        for &parent in &info.parents {
            let entry = marks.entry(parent).or_insert(0);
            if *entry & HAVE == 0 {
                *entry |= HAVE;
                unpopped_want.remove(&parent);
                if want_commits.contains_key(&parent) {
                    stack.push(parent);
                }
            }
        }
    }
}

fn prepare_pack(
    objects: &gix::OdbHandle,
    hash: gix::hash::Kind,
    commands: &[wire::Command],
    remote_objects: &HashSet<ObjectId>,
) -> Result<File> {
    let mut visited = HashMap::new();
    let mut have_objects = remote_objects.clone();
    let mut have_trees = HashSet::new();
    let mut marks: HashMap<ObjectId, u8> = HashMap::new();
    let mut queue: BinaryHeap<(i64, u64, ObjectId)> = BinaryHeap::new();
    let mut seq = 0u64;
    let mut buffer = Vec::new();
    let mut parent_buf = Vec::new();

    for &remote_id in remote_objects {
        let mut current = remote_id;
        let mut hops = 0usize;
        while let Some(data) = objects.try_find(current.as_ref(), &mut buffer)? {
            match data.decode() {
                Ok(ObjectRef::Commit(commit)) => {
                    let entry = marks.entry(current).or_insert(0);
                    if *entry & HAVE == 0 {
                        *entry |= HAVE;
                        seq += 1;
                        queue.push((commit_time(&commit), seq, current));
                    }
                    break;
                }
                Ok(ObjectRef::Tree(_)) => {
                    have_trees.insert(current);
                    break;
                }
                Ok(ObjectRef::Tag(tag)) => {
                    let target = tag.target();
                    have_objects.insert(target);
                    if hops >= 64 {
                        break;
                    }
                    hops += 1;
                    current = target;
                }
                _ => break,
            }
        }
    }

    let mut unpopped_want = HashSet::new();
    let mut missing_want = HashSet::new();
    let mut pending_objects = Vec::new();
    for command in commands.iter().filter(|command| !command.new.is_null()) {
        let mut current = command.new;
        let mut expected_kind = command
            .name
            .starts_with(b"refs/heads/")
            .then_some(Kind::Commit);
        loop {
            if have_objects.contains(&current) {
                break;
            }
            ensure!(
                current.kind() == hash && !current.is_null(),
                "invalid reachable object ID {current}"
            );
            let data = checked_object(objects, current, &mut buffer)?;
            ensure!(
                expected_kind.is_none_or(|expected| expected == data.kind),
                "object {current} has the wrong kind for a reachable edge"
            );
            match data
                .decode()
                .with_context(|| format!("corrupt reachable object {current}"))?
            {
                ObjectRef::Commit(commit) => {
                    let entry = marks.entry(current).or_insert(0);
                    if *entry & (WANT | HAVE) == 0 {
                        *entry = WANT;
                        unpopped_want.insert(current);
                        seq += 1;
                        queue.push((commit_time(&commit), seq, current));
                    }
                    break;
                }
                ObjectRef::Tag(tag) => {
                    visited.insert(current, Kind::Tag);
                    ensure!(
                        u32::try_from(visited.len()).is_ok(),
                        "push pack exceeds the u32 object count limit"
                    );
                    expected_kind = Some(tag.target_kind);
                    current = tag.target();
                }
                ObjectRef::Tree(_) | ObjectRef::Blob(_) => {
                    pending_objects.push((current, expected_kind));
                    break;
                }
            }
        }
    }

    let mut want_commits = HashMap::new();
    let mut want_order = Vec::new();
    while let Some((_, _, id)) = queue.pop() {
        let mark = marks.get(&id).copied().unwrap_or(0);
        if mark == WANT {
            unpopped_want.remove(&id);
            let data = checked_object(objects, id, &mut buffer)?;
            ensure!(
                data.kind == Kind::Commit,
                "object {id} has the wrong kind for a reachable edge"
            );
            let ObjectRef::Commit(commit) = data
                .decode()
                .with_context(|| format!("corrupt reachable object {id}"))?
            else {
                unreachable!("checked Kind::Commit above");
            };
            let tree = commit.tree();
            let parents: Vec<_> = commit.parents().collect();
            for &parent in &parents {
                let parent_mark = marks.get(&parent).copied().unwrap_or(0);
                if parent_mark & (WANT | HAVE) != 0 {
                    continue;
                }
                ensure!(
                    parent.kind() == hash && !parent.is_null(),
                    "invalid reachable object ID {parent}"
                );
                let Some(pdata) = objects.try_find(parent.as_ref(), &mut parent_buf)? else {
                    missing_want.insert(parent);
                    continue;
                };
                ensure!(
                    pdata.kind == Kind::Commit,
                    "object {parent} has the wrong kind for a reachable edge"
                );
                let ObjectRef::Commit(pcommit) = pdata
                    .decode()
                    .with_context(|| format!("corrupt reachable object {parent}"))?
                else {
                    unreachable!("checked Kind::Commit above");
                };
                marks.insert(parent, WANT);
                unpopped_want.insert(parent);
                seq += 1;
                queue.push((commit_time(&pcommit), seq, parent));
            }
            want_order.push(id);
            want_commits.insert(id, WantCommit { tree, parents });
        } else if mark & HAVE != 0 {
            if unpopped_want.is_empty() && missing_want.is_empty() {
                break;
            }
            if want_commits.contains_key(&id) {
                propagate_have(&id, &want_commits, &mut marks, &mut unpopped_want);
            }
            let Some(data) = objects.try_find(id.as_ref(), &mut buffer)? else {
                continue;
            };
            let Ok(ObjectRef::Commit(commit)) = data.decode() else {
                continue;
            };
            for parent in commit.parents() {
                let entry = marks.entry(parent).or_insert(0);
                if *entry & HAVE == 0 {
                    *entry |= HAVE;
                    unpopped_want.remove(&parent);
                    missing_want.remove(&parent);
                    if want_commits.contains_key(&parent) {
                        propagate_have(&parent, &want_commits, &mut marks, &mut unpopped_want);
                    }
                    if let Some(pdata) = objects.try_find(parent.as_ref(), &mut parent_buf)?
                        && pdata.kind == Kind::Commit
                        && let Ok(ObjectRef::Commit(pcommit)) = pdata.decode()
                    {
                        seq += 1;
                        queue.push((commit_time(&pcommit), seq, parent));
                    }
                }
            }
        }
    }

    if let Some(missing) = missing_want
        .into_iter()
        .find(|id| marks.get(id).copied().unwrap_or(0) & HAVE == 0)
    {
        bail!("missing object {missing} while preparing push");
    }

    let mut root_trees = Vec::new();
    for id in want_order {
        if marks.get(&id).copied().unwrap_or(0) != WANT {
            continue;
        }
        visited.insert(id, Kind::Commit);
        ensure!(
            u32::try_from(visited.len()).is_ok(),
            "push pack exceeds the u32 object count limit"
        );
        let info = &want_commits[&id];
        root_trees.push(info.tree);
        for &parent in &info.parents {
            if marks.get(&parent).copied().unwrap_or(0) & HAVE != 0
                && let Some(pdata) = objects.try_find(parent.as_ref(), &mut buffer)?
                && pdata.kind == Kind::Commit
                && let Ok(ObjectRef::Commit(pcommit)) = pdata.decode()
            {
                have_trees.insert(pcommit.tree());
            }
        }
    }

    // Seed remote-owned trees and blobs from advertised tips and boundary commits
    // without reading blob payloads from the local ODB.
    let mut tree_queue: Vec<_> = have_trees.into_iter().collect();
    let mut expanded_have_trees = HashSet::new();
    while let Some(tree_id) = tree_queue.pop() {
        if !expanded_have_trees.insert(tree_id) {
            continue;
        }
        have_objects.insert(tree_id);
        let Some(data) = objects.try_find(tree_id.as_ref(), &mut buffer)? else {
            continue;
        };
        if let Ok(ObjectRef::Tree(tree)) = data.decode() {
            for entry in tree.entries {
                if entry.mode.is_commit() {
                    continue;
                }
                let child = entry.oid.to_owned();
                have_objects.insert(child);
                if entry.mode.is_tree() && !expanded_have_trees.contains(&child) {
                    tree_queue.push(child);
                }
            }
        }
    }

    pending_objects.extend(root_trees.into_iter().map(|tree| (tree, Some(Kind::Tree))));
    while let Some((id, expected_kind)) = pending_objects.pop() {
        if have_objects.contains(&id) {
            continue;
        }
        if let Some(kind) = visited.get(&id) {
            ensure!(
                expected_kind.is_none_or(|expected| expected == *kind),
                "object {id} has the wrong kind for a reachable edge"
            );
            continue;
        }
        ensure!(
            id.kind() == hash && !id.is_null(),
            "invalid reachable object ID {id}"
        );
        let data = checked_object(objects, id, &mut buffer)?;
        ensure!(
            expected_kind.is_none_or(|expected| expected == data.kind),
            "object {id} has the wrong kind for a reachable edge"
        );
        visited.insert(id, data.kind);
        ensure!(
            u32::try_from(visited.len()).is_ok(),
            "push pack exceeds the u32 object count limit"
        );
        match data
            .decode()
            .with_context(|| format!("corrupt reachable object {id}"))?
        {
            ObjectRef::Tree(tree) => {
                for entry in tree.entries {
                    if !entry.mode.is_commit() {
                        let kind = if entry.mode.is_tree() {
                            Kind::Tree
                        } else {
                            Kind::Blob
                        };
                        pending_objects.push((entry.oid.to_owned(), Some(kind)));
                    }
                }
            }
            ObjectRef::Tag(tag) => pending_objects.push((tag.target(), Some(tag.target_kind))),
            ObjectRef::Blob(_) | ObjectRef::Commit(_) => {}
        }
    }
    let count =
        u32::try_from(visited.len()).context("push pack exceeds the u32 object count limit")?;
    let mut file = tempfile::tempfile().context("creating push pack spool")?;
    // One base entry is compressed at a time. Every dependency not already
    // owned by the destination is included; no thin-pack deltas are generated.
    let entries =
        visited
            .into_keys()
            .map(|id| -> gix::Result<Vec<gix_pack::data::output::Entry>> {
                let data = checked_object(objects, id, &mut buffer)
                    .map_err(|err| gix::Error::from_boxed(err.into()))?;
                let entry = gix_pack::data::output::Entry::from_data(
                    &gix_pack::data::output::Count::from_data(id, None),
                    &data,
                    gix::zlib::Compression::DEFAULT,
                )?;
                Ok(vec![entry])
            });
    let mut writer = gix_pack::data::output::bytes::FromEntriesIter::new(
        entries,
        &mut file,
        count,
        gix_pack::data::Version::V2,
        hash,
    );
    for result in writer.by_ref() {
        result.context("encoding complete push pack")?;
    }
    if writer.digest().is_none() {
        bail!("push pack encoder did not produce a trailer");
    }
    drop(writer);
    file.rewind().context("rewinding push pack spool")?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix::protocol::handshake::Ref;

    fn oid(hex: &[u8; 40]) -> ObjectId {
        ObjectId::from_hex(hex).expect("valid SHA-1")
    }

    #[test]
    fn advertisement_accepts_repeated_have_pseudo_refs() {
        let main = oid(b"1111111111111111111111111111111111111111");
        let have_a = oid(b"2222222222222222222222222222222222222222");
        let have_b = oid(b"3333333333333333333333333333333333333333");
        let refs = vec![
            Ref::Direct {
                full_ref_name: "refs/heads/main".into(),
                object: main,
            },
            Ref::Direct {
                full_ref_name: ".have".into(),
                object: have_a,
            },
            Ref::Direct {
                full_ref_name: ".have".into(),
                object: have_b,
            },
        ];

        let (advertised, remote_objects) =
            index_advertisement(&refs, gix::hash::Kind::Sha1).expect("valid advertisement");

        assert_eq!(
            advertised
                .get(b"refs/heads/main".as_bstr())
                .copied()
                .flatten(),
            Some(main)
        );
        assert!(!advertised.contains_key(b".have".as_bstr()));
        assert_eq!(
            remote_objects,
            HashSet::from([main, have_a, have_b]),
            "all advertised objects, including .have hints, prune the push pack"
        );
    }

    #[test]
    fn advertisement_still_rejects_duplicate_real_refs() {
        let refs = vec![
            Ref::Direct {
                full_ref_name: "refs/heads/main".into(),
                object: oid(b"1111111111111111111111111111111111111111"),
            },
            Ref::Direct {
                full_ref_name: "refs/heads/main".into(),
                object: oid(b"2222222222222222222222222222222222222222"),
            },
        ];

        let error = index_advertisement(&refs, gix::hash::Kind::Sha1)
            .expect_err("duplicate real refs must remain invalid");
        assert!(
            error
                .to_string()
                .contains("remote advertised duplicate ref"),
            "{error:#}"
        );
    }

    #[test]
    fn prepare_pack_prunes_remote_ancestors_and_shared_trees() {
        use std::io::Read as _;
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str], env: &[(&str, &str)]| -> String {
            let mut cmd = Command::new("git");
            cmd.current_dir(dir.path()).args(args);
            for (k, v) in env {
                cmd.env(k, v);
            }
            let out = cmd.output().unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-b", "main"], &[]);
        git(&["config", "user.name", "Test"], &[]);
        git(&["config", "user.email", "test@example.com"], &[]);
        std::fs::create_dir_all(dir.path().join("dir")).unwrap();
        std::fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        std::fs::write(dir.path().join("dir/sub.txt"), "sub\n").unwrap();
        git(&["add", "."], &[]);
        git(
            &["commit", "-m", "base"],
            &[("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")],
        );
        let base_oid = ObjectId::from_hex(git(&["rev-parse", "HEAD"], &[]).as_bytes()).unwrap();

        std::fs::write(dir.path().join("remote.txt"), "remote 1\n").unwrap();
        git(&["add", "."], &[]);
        git(
            &["commit", "-m", "remote 1"],
            &[("GIT_COMMITTER_DATE", "2026-01-02T00:00:00Z")],
        );
        std::fs::write(dir.path().join("remote.txt"), "remote 2\n").unwrap();
        git(&["add", "."], &[]);
        git(
            &["commit", "-m", "remote 2"],
            &[("GIT_COMMITTER_DATE", "2026-01-03T00:00:00Z")],
        );
        let remote_tip = ObjectId::from_hex(git(&["rev-parse", "HEAD"], &[]).as_bytes()).unwrap();

        git(&["checkout", "-b", "topic", &base_oid.to_string()], &[]);
        std::fs::write(dir.path().join("changed.txt"), "topic\n").unwrap();
        git(&["add", "."], &[]);
        git(
            &["commit", "-m", "topic 1"],
            &[("GIT_COMMITTER_DATE", "2026-01-04T00:00:00Z")],
        );
        let topic_tip = ObjectId::from_hex(git(&["rev-parse", "HEAD"], &[]).as_bytes()).unwrap();

        let repo = gix::open(dir.path()).unwrap();
        let mut pack = prepare_pack(
            &repo.objects,
            repo.object_hash(),
            &[wire::Command {
                name: "refs/heads/topic".into(),
                old: ObjectId::null(repo.object_hash()),
                new: topic_tip,
            }],
            &HashSet::from([remote_tip]),
        )
        .unwrap();
        let mut header = [0u8; 12];
        pack.read_exact(&mut header).unwrap();
        assert_eq!(&header[..4], b"PACK");
        let object_count = u32::from_be_bytes(header[8..12].try_into().unwrap());
        assert_eq!(
            object_count, 3,
            "expected only commit, root tree, and changed blob"
        );
    }
}
