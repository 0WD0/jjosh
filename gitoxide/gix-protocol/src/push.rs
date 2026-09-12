//! Blocking receive-pack commands with explicit compare-and-swap targets.
//!
//! Authentication, lease authorization, object selection, and pack creation belong to the caller.
//! Only basic `report-status` is negotiated; sidebands and `report-status-v2` are not requested.

use std::{collections::HashMap, io::{self, Write}};

use bstr::{BString, ByteSlice};
use gix_transport::{
    Protocol,
    client::{self, MessageKind, WriteMode, blocking_io::{ExtendedBufRead, Transport}},
    packetline::{PacketLineRef, blocking_io::encode},
};

/// An already-authorized remote ref update. Null IDs represent absence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// Fully qualified destination, starting with `refs/`.
    pub name: BString,
    /// Required current value, never replaced with a freshly advertised value.
    pub old: gix_hash::ObjectId,
    /// Desired value, or null to delete the ref.
    pub new: gix_hash::ObjectId,
}

/// Features requested for this receive-pack invocation.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Require server-side atomicity rather than silently falling back to individual updates.
    pub atomic: bool,
    /// Opaque options delivered to receive-pack hooks, in order.
    pub push_options: Vec<BString>,
}

/// What the server confirmed about one requested ref.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefStatus {
    /// The server reported `ok` after successful unpacking.
    Accepted,
    /// The server reported `ng`, with its reason preserved verbatim.
    Rejected(BString),
    /// No unambiguous status was received. The ref may have changed; do not retry blindly.
    Indeterminate,
}

/// The result of a possibly mutating request, including any partially received confirmations.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Results in command order, including refs whose results are indeterminate.
    pub refs: Vec<(BString, RefStatus)>,
    /// The unpack report, if received: success or the server's failure reason.
    pub unpack: Option<Result<(), BString>>,
    /// Upload, transport, or malformed/incomplete report failure, without erasing confirmed results.
    /// A complete, well-formed report can still contain rejected refs or an unpack failure.
    pub error: Option<Error>,
}

/// A preflight failure, or a failure recorded in a possibly mutating [`Outcome`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error("Receive-pack requires protocol V0 or V1")]
    UnsupportedProtocol,
    #[error("Receive-pack did not advertise required capability {0}")]
    MissingCapability(&'static str),
    #[error("Invalid fully qualified push ref {0:?}")]
    InvalidRef(BString),
    #[error("Duplicate push ref {0:?}")]
    DuplicateRef(BString),
    #[error("Push object IDs do not match the advertised object format")]
    ObjectFormat,
    #[error("Both old and new values are null for {0:?}")]
    EmptyUpdate(BString),
    #[error("A pack writer is required when any new value is non-null")]
    MissingPack,
    #[error("A push option contains NUL or LF")]
    InvalidPushOption,
    #[error("A receive-pack command or push option exceeds the packet-line size limit")]
    PacketTooLarge,
    #[error(transparent)]
    Transport(#[from] client::Error),
    #[error("Receive-pack I/O failed")]
    Io(#[from] io::Error),
    #[error("Malformed receive-pack report: {0}")]
    MalformedStatus(&'static str),
    #[error("Receive-pack report ended before a terminating flush and all required statuses")]
    IncompleteStatus,
}

/// Validate a planned push without opening a mutation request.
///
/// `has_pack` states whether a complete raw pack will be supplied to [`execute`].
/// This checks wire capabilities and command syntax, not leases, object availability, or pack contents.
/// Empty commands always succeed, as they do in [`execute`].
pub fn preflight(
    handshake: &crate::Handshake,
    commands: &[Command],
    options: &Options,
    has_pack: bool,
) -> Result<(), Error> {
    if commands.is_empty() {
        return Ok(());
    }
    validate(handshake, commands, options, has_pack).map(|_| ())
}

/// Execute authorized commands on an already handshaken/authenticated receive-pack transport.
///
/// All preflight validation happens before requesting a writer. `Err` means no commands were
/// sent. Once the request is obtained, failures instead populate [`Outcome::error`]; consult each
/// [`RefStatus`] even when that field is set. Empty commands do not open a mutation request.
///
/// `write_pack` writes a complete raw pack, including its checksum, without packet-line framing.
/// It is required for non-deletions (an empty valid pack suffices when all objects exist remotely).
/// It must use the commands' hash kind, provide the required object closure, and not use OFS deltas,
/// since `ofs-delta` is not negotiated. No pack is needed for deletion-only requests.
/// The upload writer is flushed and dropped before reading, including on upload failure.
/// Authentication failures on the mutation request are not retried.
pub fn execute<T: Transport + ?Sized>(
    transport: &mut T,
    handshake: &crate::Handshake,
    commands: &[Command],
    options: &Options,
    write_pack: Option<&mut dyn FnMut(&mut dyn Write) -> io::Result<()>>,
) -> Result<Outcome, Error> {
    if commands.is_empty() {
        return Ok(Outcome::default());
    }
    let (capabilities, indices) = validate(handshake, commands, options, write_pack.is_some())?;
    let mut outcome = Outcome {
        refs: commands.iter().map(|cmd| (cmd.name.clone(), RefStatus::Indeterminate)).collect(),
        ..Outcome::default()
    };
    let request = transport.request(WriteMode::Binary, MessageKind::Flush, false)?;
    let (mut writer, mut reader) = request.into_parts();
    let upload = (|| -> io::Result<()> {
        let mut line = Vec::new();
        for (index, command) in commands.iter().enumerate() {
            line.clear();
            write!(&mut line, "{} {} ", command.old, command.new)?;
            line.extend_from_slice(command.name.as_bytes());
            if index == 0 {
                line.push(0);
                line.extend_from_slice(&capabilities);
            }
            encode::data_to_write(&line, &mut writer)?;
        }
        encode::flush_to_write(&mut writer)?;
        if !options.push_options.is_empty() {
            for option in &options.push_options {
                if option.is_empty() {
                    // The generic encoder rejects empty data, but an empty push option is valid.
                    writer.write_all(b"0004")?;
                } else {
                    encode::data_to_write(option.as_bytes(), &mut writer)?;
                }
            }
            encode::flush_to_write(&mut writer)?;
        }
        if let Some(write_pack) = write_pack {
            write_pack(&mut writer)?;
        }
        writer.flush()
    })();
    drop(writer);
    if let Err(err) = upload {
        outcome.error = Some(err.into());
        return Ok(outcome);
    }
    outcome.error = read_status(reader.as_mut(), &indices, &mut outcome).err();
    Ok(outcome)
}

fn validate<'a>(
    handshake: &crate::Handshake,
    commands: &'a [Command],
    options: &Options,
    has_pack: bool,
) -> Result<(Vec<u8>, HashMap<&'a [u8], usize>), Error> {
    if !matches!(handshake.server_protocol_version, Protocol::V0 | Protocol::V1) {
        return Err(Error::UnsupportedProtocol);
    }
    let require = |name| {
        if handshake.capabilities.contains(name) { Ok(()) } else { Err(Error::MissingCapability(name)) }
    };
    require("report-status")?;
    let format_cap = handshake.capabilities.capability("object-format");
    let format = match &format_cap {
        Some(cap) => cap.value().ok_or(Error::ObjectFormat)?.as_bytes(),
        None => b"sha1",
    };
    if !matches!(format, b"sha1" | b"sha256") {
        return Err(Error::ObjectFormat);
    }
    let kind = commands[0].old.kind();
    if kind.to_string().as_bytes() != format {
        return Err(Error::ObjectFormat);
    }
    let mut capabilities = b"report-status".to_vec();
    if options.atomic {
        require("atomic")?;
        capabilities.extend_from_slice(b" atomic");
    }
    if !options.push_options.is_empty() {
        require("push-options")?;
        capabilities.extend_from_slice(b" push-options");
    }
    if format_cap.is_some() {
        capabilities.extend_from_slice(b" object-format=");
        capabilities.extend_from_slice(format);
    }
    // Maximum packet payload supported by the transport's packet-line encoder.
    const MAX_PACKET_DATA: usize = 65516;
    for option in &options.push_options {
        if option.contains(&0) || option.contains(&b'\n') {
            return Err(Error::InvalidPushOption);
        }
        if option.len() > MAX_PACKET_DATA {
            return Err(Error::PacketTooLarge);
        }
    }
    let mut indices = HashMap::with_capacity(commands.len());
    let mut needs_pack = false;
    for (index, cmd) in commands.iter().enumerate() {
        if !cmd.name.starts_with(b"refs/") || <&gix_ref::FullNameRef>::try_from(cmd.name.as_bstr()).is_err() {
            return Err(Error::InvalidRef(cmd.name.clone()));
        }
        if indices.insert(cmd.name.as_bytes(), index).is_some() {
            return Err(Error::DuplicateRef(cmd.name.clone()));
        }
        if cmd.old.kind() != kind || cmd.new.kind() != kind {
            return Err(Error::ObjectFormat);
        }
        if cmd.new.is_null() {
            if cmd.old.is_null() {
                return Err(Error::EmptyUpdate(cmd.name.clone()));
            }
            require("delete-refs")?;
        } else {
            needs_pack = true;
        }
        let length = kind.len_in_hex() * 2 + 2 + cmd.name.len()
            + if index == 0 { 1 + capabilities.len() } else { 0 };
        if length > MAX_PACKET_DATA {
            return Err(Error::PacketTooLarge);
        }
    }
    if needs_pack && !has_pack {
        return Err(Error::MissingPack);
    }
    Ok((capabilities, indices))
}

fn read_status<'a>(
    reader: &mut dyn ExtendedBufRead<'a>,
    indices: &HashMap<&[u8], usize>,
    outcome: &mut Outcome,
) -> Result<(), Error> {
    loop {
        let packet = match reader.readline() {
            Some(Ok(Ok(packet))) => packet,
            Some(Ok(Err(err))) => return Err(client::Error::from(err).into()),
            Some(Err(err)) => return Err(err.into()),
            None => {
                if reader.stopped_at() == Some(MessageKind::Flush) {
                    break;
                }
                return Err(Error::IncompleteStatus);
            }
        };
        let data = match packet {
            PacketLineRef::Data(data) => data,
            PacketLineRef::Flush => break,
            _ => return Err(Error::MalformedStatus("unexpected control packet")),
        };
        // Git permits the final LF to be omitted, but one packet must contain exactly one record.
        let line = data.strip_suffix(b"\n").unwrap_or(data);
        if line.contains(&0) || line.contains(&b'\n') || line.is_empty() {
            return Err(Error::MalformedStatus("invalid status record"));
        }
        if outcome.unpack.is_none() {
            let result = line.strip_prefix(b"unpack ")
                .filter(|result| !result.is_empty())
                .ok_or(Error::MalformedStatus("expected unpack report first"))?;
            outcome.unpack = Some(if result == b"ok" { Ok(()) } else { Err(result.into()) });
            continue;
        }
        let (name, status) = if let Some(name) = line.strip_prefix(b"ok ") {
            if !matches!(outcome.unpack, Some(Ok(()))) {
                return Err(Error::MalformedStatus("ref accepted after unpack failure"));
            }
            (name, RefStatus::Accepted)
        } else if let Some(rest) = line.strip_prefix(b"ng ") {
            let split = rest.iter().position(|byte| *byte == b' ')
                .ok_or(Error::MalformedStatus("rejection lacks a reason"))?;
            let (name, reason) = (&rest[..split], &rest[split + 1..]);
            if reason.is_empty() {
                return Err(Error::MalformedStatus("rejection lacks a reason"));
            }
            (name, RefStatus::Rejected(reason.into()))
        } else {
            return Err(Error::MalformedStatus("expected ok or ng record"));
        };
        let index = *indices.get(name).ok_or(Error::MalformedStatus("status names an unrequested ref"))?;
        let previous = &mut outcome.refs[index].1;
        if !matches!(previous, RefStatus::Indeterminate) {
            // A conflicting duplicate invalidates this ref's confirmation, not unrelated confirmations.
            *previous = RefStatus::Indeterminate;
            return Err(Error::MalformedStatus("duplicate ref status"));
        }
        *previous = status;
    }
    if outcome.unpack.is_none() || outcome.refs.iter().any(|(_, status)| matches!(status, RefStatus::Indeterminate)) {
        return Err(Error::IncompleteStatus);
    }
    Ok(())
}

#[cfg(all(test, feature = "sha1"))]
mod tests {
    use super::*;
    use gix_transport::client::git::{self, blocking_io::Connection};

    fn commands() -> Vec<Command> {
        ["refs/heads/a", "refs/heads/b"].into_iter().map(|name| Command {
            name: name.into(),
            old: gix_hash::ObjectId::from_hex(b"1111111111111111111111111111111111111111").unwrap(),
            new: gix_hash::Kind::Sha1.null(),
        }).collect()
    }

    fn response(records: &[&[u8]], flush: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        for record in records {
            encode::data_to_write(record, &mut bytes).unwrap();
        }
        if flush {
            encode::flush_to_write(&mut bytes).unwrap();
        }
        bytes
    }

    fn run(reader: impl io::Read) -> Outcome {
        let mut transport = Connection::new(
            reader, io::sink(), Protocol::V1, "/unused",
            None::<(String, Option<u16>)>, git::ConnectMode::Process, false,
        );
        let handshake = crate::Handshake {
            server_protocol_version: Protocol::V1,
            capabilities: client::Capabilities::from_bytes(b"\0report-status delete-refs").unwrap().0,
            ..crate::Handshake::default()
        };
        execute(&mut transport, &handshake, &commands(), &Options::default(), None).unwrap()
    }

    #[test]
    fn stale_ref_rejection_does_not_erase_another_refs_acceptance() {
        let report = response(&[b"unpack ok\n", b"ok refs/heads/a\n", b"ng refs/heads/b stale info\n"], true);
        let outcome = run(report.as_slice());
        assert!(outcome.error.is_none());
        assert_eq!(outcome.refs, vec![
            ("refs/heads/a".into(), RefStatus::Accepted),
            ("refs/heads/b".into(), RefStatus::Rejected("stale info".into())),
        ]);
    }

    #[test]
    fn missing_status_is_indeterminate_even_after_a_flush() {
        let report = response(&[b"unpack ok", b"ok refs/heads/a"], true);
        let outcome = run(report.as_slice());
        assert!(matches!(outcome.error, Some(Error::IncompleteStatus)));
        assert_eq!(outcome.refs[0].1, RefStatus::Accepted);
        assert_eq!(outcome.refs[1].1, RefStatus::Indeterminate);
    }

    #[test]
    fn truncated_packet_preserves_the_preceding_confirmation() {
        let mut report = response(&[b"unpack ok", b"ok refs/heads/a"], false);
        report.extend_from_slice(b"0020ng refs/heads/b");
        let outcome = run(report.as_slice());
        assert!(outcome.error.is_some());
        assert_eq!(outcome.refs[0].1, RefStatus::Accepted);
        assert_eq!(outcome.refs[1].1, RefStatus::Indeterminate);
    }

    #[test]
    fn missing_terminal_flush_is_not_a_complete_report() {
        let report = response(&[b"unpack ok", b"ok refs/heads/a", b"ok refs/heads/b"], false);
        let outcome = run(report.as_slice());
        assert!(outcome.error.is_some());
        assert!(outcome.refs.iter().all(|(_, status)| *status == RefStatus::Accepted));
    }

    #[test]
    fn conflicting_duplicate_invalidates_only_its_own_ref() {
        let report = response(&[
            b"unpack ok", b"ok refs/heads/a", b"ok refs/heads/b", b"ng refs/heads/b denied",
        ], true);
        let outcome = run(report.as_slice());
        assert!(matches!(outcome.error, Some(Error::MalformedStatus(_))));
        assert_eq!(outcome.refs[0].1, RefStatus::Accepted);
        assert_eq!(outcome.refs[1].1, RefStatus::Indeterminate);
    }

    #[test]
    fn statuses_require_exact_names_and_cannot_precede_unpack() {
        for records in [
            vec![b"unpack ok".as_slice(), b"ok refs/heads/ab"],
            vec![b"ok refs/heads/a".as_slice()],
            vec![b"unpack failed".as_slice(), b"ok refs/heads/a"],
            vec![b"unpack ok".as_slice(), b"ng refs/heads/a"],
        ] {
            let report = response(&records, true);
            let outcome = run(report.as_slice());
            assert!(matches!(outcome.error, Some(Error::MalformedStatus(_))));
            assert!(outcome.refs.iter().all(|(_, status)| *status == RefStatus::Indeterminate));
        }
    }

    #[test]
    fn transport_failure_preserves_confirmations_without_fabricating_rejections() {
        struct FailAfter(io::Cursor<Vec<u8>>);
        impl io::Read for FailAfter {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let count = io::Read::read(&mut self.0, buffer)?;
                if count == 0 { Err(io::ErrorKind::ConnectionReset.into()) } else { Ok(count) }
            }
        }
        let outcome = run(FailAfter(io::Cursor::new(response(&[b"unpack ok", b"ok refs/heads/a"], false))));
        assert!(matches!(outcome.error, Some(Error::Io(_))));
        assert_eq!(outcome.refs[0].1, RefStatus::Accepted);
        assert_eq!(outcome.refs[1].1, RefStatus::Indeterminate);
    }
}
