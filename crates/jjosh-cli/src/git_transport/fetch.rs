use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, ensure};
use gix::bstr::BString;
use gix::progress::Discard;
use gix::protocol::handshake::Ref;
use gix::remote::Direction::Fetch;
use gix::remote::fetch::{Status, Tags, refmap::Source};

/// Raw server identities and GC protection for objects not yet published by the caller.
pub(crate) struct Outcome {
    pub advertised: Vec<Ref>,
    /// Selected refs whose non-null direct objects are available locally, including
    /// objects already present before this fetch. Unborn refs remain in `advertised`.
    pub received: Vec<Ref>,
    /// Keep these files until raw retention refs have been published successfully.
    pub keep_paths: Vec<PathBuf>,
}

/// Discover all refs, then receive only the selected, advertisement-pinned objects.
///
/// Neither connection has destination refspecs or automatic tag mappings. The
/// second connection requests literal object IDs, never names that may have moved
/// since discovery. A server refusing those IDs fails the operation.
pub(crate) fn fetch(
    remote: gix::Remote<'_>,
    select: impl Fn(&Ref) -> bool,
    interrupt: &AtomicBool,
) -> anyhow::Result<Outcome> {
    ensure!(!interrupt.load(Ordering::Relaxed), "Git fetch interrupted");
    let repo = remote.repo();
    let mut remote = remote.with_fetch_tags(Tags::None);
    remote
        .replace_refspecs(std::iter::empty::<&str>(), Fetch)
        .context("Clearing configured fetch mappings for object-only discovery")?;
    let (ref_map, _) = remote
        .connect(Fetch)
        .context("Connecting to Git remote for discovery")?
        .ref_map(
            Discard,
            gix::remote::ref_map::Options {
                prefix_from_spec_as_filter_on_remote: false,
                ..Default::default()
            },
        )
        .context("Reading the complete Git remote advertisement")?;
    let advertised = ref_map.remote_refs;
    let received: Vec<_> = advertised
        .iter()
        .filter(|reference| select(reference))
        .filter(|reference| reference.unpack().1.is_some_and(|id| !id.is_null()))
        .cloned()
        .collect();
    ensure!(!interrupt.load(Ordering::Relaxed), "Git fetch interrupted");
    if received.is_empty() {
        return Ok(Outcome {
            advertised,
            received,
            keep_paths: Vec::new(),
        });
    }

    // Multiple refs can name the same object. Deduplicate wants, not ref identities.
    let mut ids: Vec<_> = received
        .iter()
        .filter_map(|reference| reference.unpack().1.map(ToOwned::to_owned))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let mut hex = gix::hash::Kind::hex_buf();
    remote
        .replace_refspecs(
            ids.iter()
                .map(|id| BString::from(id.hex_to_buf(&mut hex).as_bytes())),
            Fetch,
        )
        .context("Installing pinned object-only fetch requests")?;
    let prepared = remote
        .connect(Fetch)
        .context("Connecting to Git remote for object receive")?
        .prepare_fetch(
            Discard,
            gix::remote::ref_map::Options {
                prefix_from_spec_as_filter_on_remote: false,
                ..Default::default()
            },
        )
        .context("Preparing pinned Git object receive")?;
    ensure!(
        prepared.ref_map().mappings.len() == ids.len()
            && prepared.ref_map().mappings.iter().all(|mapping| {
                mapping.local.is_none()
                    && matches!(&mapping.remote, Source::ObjectId(id) if ids.binary_search(id).is_ok())
            }),
        "Object-only Git fetch unexpectedly mapped destination refs or unpinned objects"
    );
    let outcome = prepared
        .receive(Discard, interrupt)
        .context("Receiving advertisement-pinned Git objects")?;
    let (keep_path, edits) = match outcome.status {
        Status::Change {
            write_pack_bundle,
            update_refs,
            ..
        } => (write_pack_bundle.keep_path, update_refs.edits),
        Status::NoPackReceived { update_refs, .. } => (None, update_refs.edits),
    };
    ensure!(edits.is_empty(), "Object-only Git fetch edited local refs");
    // A successful protocol exchange alone must not confirm an object the server
    // omitted. Header lookup propagates ODB failures without allocating object data.
    for id in ids {
        repo.find_header(id)
            .with_context(|| format!("Pinned Git object {id} is unavailable after receive"))?;
    }
    Ok(Outcome {
        advertised,
        received,
        keep_paths: keep_path.into_iter().collect(),
    })
}
