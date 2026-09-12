use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;
use jj_cli::command_error::user_error_with_message;

// Publication leases are keyed by the exact source URL and fully qualified ref.
// Retain the existing key so leases established by previous jjosh pushes survive.
pub fn push_tracking_ref(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
    destination: &str,
) -> Result<String, CommandError> {
    let key = format!("{remote}\0{destination}");
    let id = josh_core::objects::write_blob(transaction.odb(), key.as_bytes()).map_err(|err| {
        user_error_with_message("Failed to identify the link push destination", err)
    })?;
    Ok(format!("refs/jjosh/link-push/{id}"))
}

pub fn record_observation(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
    destination: &str,
    commit: gix_hash::ObjectId,
) -> Result<(), CommandError> {
    if !destination.starts_with("refs/heads/") && !destination.starts_with("refs/tags/") {
        return Err(user_error(
            "A publication observation must identify a branch or tag",
        ));
    }
    let reference = push_tracking_ref(transaction, remote, destination)?;
    let previous = transaction.resolve_ref(&reference).map_err(|err| {
        user_error_with_message("Failed to read the observed remote position", err)
    })?;
    transaction
        .update_ref(
            &reference,
            previous.map_or(
                josh_core::cache::Expected::Absent,
                josh_core::cache::Expected::At,
            ),
            commit,
            "jjosh link remote observation",
        )
        .and_then(|()| transaction.flush_mem_odb())
        .map_err(|err| user_error_with_message("Failed to save the observed remote position", err))
}

/// Missing ledger state is not evidence that the remote reference is absent.
pub(crate) fn observation(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
    destination: &str,
) -> Result<crate::git_transport::push::Expected, CommandError> {
    use crate::git_transport::push::Expected;
    let reference = push_tracking_ref(transaction, remote, destination)?;
    let absent = gix_object::compute_hash(
        gix_hash::Kind::Sha1,
        gix_object::Kind::Blob,
        b"jjosh-remote-absent\n",
    )
    .map_err(user_error)?;
    Ok(
        match transaction.resolve_ref(&reference).map_err(user_error)? {
            None => Expected::Unknown,
            Some(id) if id == absent => Expected::Absent,
            Some(id) => Expected::At(id),
        },
    )
}

/// Records a successfully observed absence, independently of projected visibility.
pub(crate) fn record_absence(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
    destination: &str,
) -> Result<(), CommandError> {
    let absent = josh_core::objects::write_blob(transaction.odb(), b"jjosh-remote-absent\n")
        .map_err(user_error)?;
    record_observation(transaction, remote, destination, absent)
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
