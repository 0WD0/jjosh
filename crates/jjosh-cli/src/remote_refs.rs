use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;

// Leases store object identities, not object reachability. In particular, a
// shallow source's raw commit must never become a root in the canonical ODB.
fn observation_key(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
    destination: &str,
) -> Result<gix_hash::ObjectId, CommandError> {
    let key = format!("{remote}\0{destination}");
    josh_core::objects::write_blob(transaction.odb(), key.as_bytes()).map_err(user_error)
}

fn save_observation(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
    destination: &str,
    value: &[u8],
) -> Result<(), CommandError> {
    if !destination.starts_with("refs/heads/") && !destination.starts_with("refs/tags/") {
        return Err(user_error("A publication observation must identify a branch or tag"));
    }
    let key = observation_key(transaction, remote, destination)?;
    let reference = format!("refs/jjosh/observations/{key}");
    let previous = transaction.resolve_ref(&reference).map_err(user_error)?;
    let value = josh_core::objects::write_blob(transaction.odb(), value).map_err(user_error)?;
    transaction.update_ref(
        &reference,
        previous.map_or(josh_core::cache::Expected::Absent, josh_core::cache::Expected::At),
        value,
        "observe remote object identity",
    ).map_err(user_error)?;
    // Renewing an observation upgrades its previous on-disk representation.
    // Never reset an existing lease merely because the storage format changed.
    let old = format!("refs/jjosh/link-push/{key}");
    if let Some(value) = transaction.resolve_ref(&old).map_err(user_error)? {
        transaction.delete_ref(&old, josh_core::cache::Expected::At(value)).map_err(user_error)?;
    }
    transaction.flush_mem_odb().map_err(user_error)
}

pub(crate) fn record_observation(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
    destination: &str,
    object: gix_hash::ObjectId,
) -> Result<(), CommandError> {
    save_observation(transaction, remote, destination, object.to_string().as_bytes())
}

/// Missing ledger state is not evidence that the remote reference is absent.
pub(crate) fn observation(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
    destination: &str,
) -> Result<crate::git_transport::push::Expected, CommandError> {
    use crate::git_transport::push::Expected;
    let key = observation_key(transaction, remote, destination)?;
    if let Some(id) = transaction.resolve_ref(&format!("refs/jjosh/observations/{key}")).map_err(user_error)? {
        let (kind, bytes) = transaction.odb().read(id).map_err(user_error)?;
        if kind != gix_object::Kind::Blob {
            return Err(user_error("Remote observation is not an identity record"));
        }
        return if &bytes[..] == b"absent" {
            Ok(Expected::Absent)
        } else {
            let id = gix_hash::ObjectId::from_hex(&bytes).map_err(user_error)?;
            if id.is_null() {
                return Err(user_error("Remote observation contains a null object identity"));
            }
            Ok(Expected::At(id))
        };
    }
    let absent = gix_object::compute_hash(
        gix_hash::Kind::Sha1, gix_object::Kind::Blob, b"jjosh-remote-absent\n",
    ).map_err(user_error)?;
    Ok(match transaction.resolve_ref(&format!("refs/jjosh/link-push/{key}")).map_err(user_error)? {
        None => Expected::Unknown,
        Some(id) if id == absent => Expected::Absent,
        Some(id) => Expected::At(id),
    })
}

pub(crate) fn record_absence(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
    destination: &str,
) -> Result<(), CommandError> {
    save_observation(transaction, remote, destination, b"absent")
}

/// A commit tag chain, with its original annotation bytes in outer-to-inner order.
/// JJ tracks the peeled commit; the Git mirrors retain these annotations.
pub(crate) struct CommitTag {
    pub(crate) commit: gix::ObjectId,
    annotations: Vec<Vec<u8>>,
}

impl CommitTag {
    pub(crate) fn read(git: &gix::Repository, mut id: gix::ObjectId) -> anyhow::Result<Self> {
        let mut annotations = Vec::new();
        loop {
            let object = git.find_object(id)?;
            match object.kind {
                gix::objs::Kind::Commit => {
                    return Ok(Self {
                        commit: id,
                        annotations,
                    });
                }
                gix::objs::Kind::Tag => {
                    let tag = gix::objs::TagRef::from_bytes(&object.data, id.kind())?;
                    // Reject every supported armor format, even malformed signatures.
                    // Rewriting either a target or the public name invalidates it.
                    anyhow::ensure!(
                        tag.signature.is_none(),
                        "Signed tags cannot be transformed without invalidating their signatures; no canonical tag or publication lease was created. Publish an explicitly unsigned tag instead"
                    );
                    id = tag.target();
                    annotations.push(object.data.to_vec());
                }
                _ => anyhow::bail!("Only tags that peel to commits can be transformed"),
            }
        }
    }

    /// Preserve direct raw tag identities as evidence, independently of retargeting.
    pub(crate) fn copy_annotations_to(&self, git: &gix::Repository) -> anyhow::Result<()> {
        for annotation in self.annotations.iter().rev() {
            gix::objs::Write::write_buf(&git.objects, gix::objs::Kind::Tag, annotation)
                .map_err(anyhow::Error::from_boxed)?;
        }
        Ok(())
    }

    pub(crate) fn retarget(
        &self,
        git: &gix::Repository,
        mut target: gix::ObjectId,
        public_name: &str,
    ) -> anyhow::Result<gix::ObjectId> {
        let mut kind = "commit";
        for (index, annotation) in self.annotations.iter().enumerate().rev() {
            // Parse above validates the three leading headers. Rewrite only those
            // that change: keep inner tag names, tagger bytes and the entire body.
            let mut lines = annotation.splitn(4, |byte| *byte == b'\n');
            lines.next();
            lines.next();
            let name = lines.next().expect("validated tag name");
            let suffix = lines.next().expect("validated tag headers");
            let mut rewritten = format!("object {target}\ntype {kind}\n").into_bytes();
            if index == 0 {
                rewritten.extend_from_slice(format!("tag {public_name}").as_bytes());
            } else {
                rewritten.extend_from_slice(name);
            }
            rewritten.push(b'\n');
            rewritten.extend_from_slice(suffix);
            target = gix::objs::Write::write_buf(&git.objects, gix::objs::Kind::Tag, &rewritten)
                .map_err(anyhow::Error::from_boxed)?;
            kind = "tag";
        }
        Ok(target)
    }
}
