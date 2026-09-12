use std::collections::{HashMap, HashSet};
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
    Any,
}

#[derive(Clone, Debug)]
pub(crate) struct Update {
    pub name: BString,
    pub expected: Expected,
    pub new: Option<ObjectId>,
}

#[derive(Default, Debug)]
pub(crate) struct Options {
    pub dry_run: bool,
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
    /// Authorized and preflighted, but not sent because this is a dry run.
    Planned,
}

#[derive(Debug)]
pub(crate) struct Outcome {
    /// Exact raw destination names, in the same order as the supplied updates.
    pub refs: Vec<(BString, RefStatus)>,
    /// A post-send failure does not invalidate independently confirmed per-ref reports.
    pub error: Option<anyhow::Error>,
}

/// Push raw objects and refs without changing local refs, leases, or JJ state.
/// Errors returned directly are pre-mutation failures; post-send failures retain per-ref outcomes.
pub(crate) fn push(
    repo: &gix::Repository,
    remote: gix::Remote<'_>,
    updates: &[Update],
    options: &Options,
) -> Result<Outcome> {
    let hash = repo.object_hash();
    let mut names = HashSet::with_capacity(updates.len());
    for update in updates {
        ensure!(update.name.starts_with(b"refs/"), "push destination must be fully qualified: {:?}", update.name);
        gix::validate::reference::name(update.name.as_bstr())
            .with_context(|| format!("invalid push destination {:?}", update.name))?;
        ensure!(names.insert(update.name.as_bstr()), "duplicate push destination {:?}", update.name);
        for id in update.new.iter().chain(match &update.expected {
            Expected::At(id) => Some(id),
            _ => None,
        }) {
            ensure!(id.kind() == hash && !id.is_null(), "invalid object ID {id} for push destination {:?}", update.name);
        }
    }
    if updates.is_empty() {
        return Ok(Outcome { refs: Vec::new(), error: None });
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
    let transport = transport::client::blocking_io::connect(
        url,
        transport::client::blocking_io::connect::Options {
            version: transport::Protocol::V1,
            ssh,
            trace: repo.config_snapshot().boolean("gitoxide.tracePacket").unwrap_or_default(),
        },
    )?;
    let mut connection = remote.to_connection_with_transport(transport);
    let credentials = connection.configured_credentials_for_current_url();
    if let Some(config) = transport_options {
        connection.transport_mut().configure(&*config)?;
    }
    let handshake = protocol::handshake(
        connection.transport_mut(),
        transport::Service::ReceivePack,
        credentials,
        Vec::new(),
        &mut gix::progress::Discard,
    )?;
    ensure!(
        matches!(handshake.server_protocol_version, transport::Protocol::V0 | transport::Protocol::V1),
        "receive-pack did not negotiate protocol V1"
    );

    let mut advertised = HashMap::new();
    for reference in handshake.refs.as_ref().context("receive-pack omitted its ref advertisement")? {
        use protocol::handshake::Ref;
        let (name, id) = match reference {
            Ref::Direct { full_ref_name, object } => (full_ref_name, Some(*object)),
            Ref::Peeled { full_ref_name, tag, .. } => (full_ref_name, Some(*tag)),
            Ref::Symbolic { full_ref_name, tag, object, .. } => (full_ref_name, Some(tag.unwrap_or(*object))),
            Ref::Unborn { full_ref_name, .. } => (full_ref_name, None),
        };
        if let Some(id) = id {
            ensure!(id.kind() == hash && !id.is_null(), "remote advertised an incompatible object ID for {name:?}");
        }
        ensure!(advertised.insert(name.as_bstr(), id).is_none(), "remote advertised duplicate ref {name:?}");
    }

    // A separate handle prevents replacement refs from changing raw object identity.
    let mut objects = repo.objects.clone();
    objects.ignore_replacements = true;
    objects.prevent_pack_unload();
    let mut outcome = Outcome { refs: Vec::with_capacity(updates.len()), error: None };
    let mut commands = Vec::new();
    let mut command_indices = Vec::new();
    for (index, update) in updates.iter().enumerate() {
        let old = advertised.get(update.name.as_bstr()).copied().flatten();
        let rejection = match update.expected {
            Expected::Absent if old.is_some() => Some(BString::from("stale lease: expected remote ref to be absent")),
            Expected::At(expected) if old != Some(expected) => Some(BString::from(format!("stale lease: expected remote ref at {expected}"))),
            Expected::Unknown => match (old, update.new) {
                (_, None) => Some(BString::from("deletion requires an explicit lease or force authorization")),
                (None, Some(_)) => None,
                (Some(old), Some(new)) if old == new => None,
                (Some(old), Some(new)) if update.name.starts_with(b"refs/heads/") => {
                    if is_ancestor(&objects, old, new)? {
                        None
                    } else {
                        Some(BString::from("non-fast-forward update requires an explicit lease or force authorization"))
                    }
                }
                _ => Some(BString::from("overwriting this ref requires an explicit lease or force authorization")),
            },
            _ => None,
        };
        let status = if let Some(reason) = rejection {
            RefStatus::Rejected(reason)
        } else if old == update.new {
            if options.dry_run { RefStatus::Planned } else { RefStatus::Accepted }
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
    }
    if options.atomic && outcome.refs.iter().any(|(_, status)| matches!(status, RefStatus::Rejected(_))) {
        for (_, status) in &mut outcome.refs {
            if !matches!(status, RefStatus::Rejected(_)) {
                *status = RefStatus::Rejected("atomic push aborted by another ref's preflight rejection".into());
            }
        }
        return Ok(outcome);
    }
    if commands.is_empty() {
        return Ok(outcome);
    }

    let wire_options = wire::Options { atomic: options.atomic, push_options: options.push_options.clone() };
    let has_pack = commands.iter().any(|command| !command.new.is_null());
    wire::preflight(&handshake, &commands, &wire_options, has_pack)?;
    let mut pack = if has_pack {
        Some(prepare_pack(&objects, hash, &commands)?)
    } else {
        None
    };
    if options.dry_run {
        return Ok(outcome);
    }

    // All graph and encoding failures occur before issuing a mutation request.
    // The callback only streams an already-complete pack, never retaining packed bytes in RAM.
    let mut copy_pack = |out: &mut dyn Write| -> io::Result<()> {
        io::copy(pack.as_mut().expect("pack callback only supplied with a pack"), out)?;
        Ok(())
    };
    let report = wire::execute(
        connection.transport_mut(),
        &handshake,
        &commands,
        &wire_options,
        if has_pack { Some(&mut copy_pack) } else { None },
    )?;
    for index in &command_indices {
        outcome.refs[*index].1 = RefStatus::Indeterminate;
    }
    let indices: HashMap<_, _> = command_indices.iter().map(|index| (updates[*index].name.as_bstr(), *index)).collect();
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
    if outcome.error.is_none() {
        if let Some(Err(reason)) = report.unpack {
            outcome.error = Some(anyhow::anyhow!("receive-pack unpack failed: {reason}"));
        }
    }
    Ok(outcome)
}

fn checked_object<'a>(objects: &gix::OdbHandle, id: ObjectId, buffer: &'a mut Vec<u8>) -> Result<gix_object::Data<'a>> {
    let data = objects.try_find(id.as_ref(), buffer)
        .map_err(anyhow::Error::from_boxed)?
        .with_context(|| format!("missing object {id} while preparing push"))?;
    ensure!(gix_object::compute_hash(id.kind(), data.kind, data.data)? == id, "corrupt object {id}: content hash mismatch");
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

fn prepare_pack(objects: &gix::OdbHandle, hash: gix::hash::Kind, commands: &[wire::Command]) -> Result<File> {
    let mut pending: Vec<_> = commands.iter().filter(|command| !command.new.is_null())
        .map(|command| (command.new, command.name.starts_with(b"refs/heads/").then_some(Kind::Commit)))
        .collect();
    let mut visited = HashMap::new();
    let mut buffer = Vec::new();
    while let Some((id, expected_kind)) = pending.pop() {
        if let Some(kind) = visited.get(&id) {
            ensure!(expected_kind.is_none_or(|expected| expected == *kind), "object {id} has the wrong kind for a reachable edge");
            continue;
        }
        ensure!(id.kind() == hash && !id.is_null(), "invalid reachable object ID {id}");
        let data = checked_object(objects, id, &mut buffer)?;
        ensure!(expected_kind.is_none_or(|expected| expected == data.kind), "object {id} has the wrong kind for a reachable edge");
        visited.insert(id, data.kind);
        ensure!(u32::try_from(visited.len()).is_ok(), "push pack exceeds the u32 object count limit");
        match data.decode().with_context(|| format!("corrupt reachable object {id}"))? {
            ObjectRef::Commit(commit) => {
                pending.push((commit.tree(), Some(Kind::Tree)));
                pending.extend(commit.parents().map(|parent| (parent, Some(Kind::Commit))));
            }
            ObjectRef::Tree(tree) => {
                for entry in tree.entries {
                    if !entry.mode.is_commit() {
                        let kind = if entry.mode.is_tree() { Kind::Tree } else { Kind::Blob };
                        pending.push((entry.oid.to_owned(), Some(kind)));
                    }
                }
            }
            ObjectRef::Tag(tag) => pending.push((tag.target(), Some(tag.target_kind))),
            ObjectRef::Blob(_) => {}
        }
    }
    let count = u32::try_from(visited.len()).context("push pack exceeds the u32 object count limit")?;
    let mut file = tempfile::tempfile().context("creating push pack spool")?;
    // One base entry is compressed at a time: bounded by the largest individual
    // object rather than total packed bytes. Every dependency is included; no deltas.
    let entries = visited.into_keys().map(|id| -> io::Result<Vec<gix_pack::data::output::Entry>> {
        let data = checked_object(objects, id, &mut buffer).map_err(io::Error::other)?;
        let entry = gix_pack::data::output::Entry::from_data(
            &gix_pack::data::output::Count::from_data(id, None),
            &data,
            gix::zlib::Compression::DEFAULT,
        ).map_err(io::Error::other)?;
        Ok(vec![entry])
    });
    let mut writer = gix_pack::data::output::bytes::FromEntriesIter::new(
        entries, &mut file, count, gix_pack::data::Version::V2, hash,
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
