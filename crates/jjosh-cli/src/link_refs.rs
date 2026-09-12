use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;
use jj_cli::command_error::user_error_with_message;

// Publication leases are keyed by the exact source URL and destination branch.
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
    if !destination.starts_with("refs/heads/") {
        return Err(user_error(
            "A publication observation must identify a branch",
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

/// Missing ledger state is not evidence that the remote branch is absent.
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
