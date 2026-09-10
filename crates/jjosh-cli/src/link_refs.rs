use std::fmt::Write as _;
use std::path::Path;

use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;
use jj_cli::command_error::user_error_with_message;

pub fn source_name(path: &Path) -> Result<String, CommandError> {
    let path = path
        .to_str()
        .ok_or_else(|| user_error("Link path must be valid UTF-8"))?;
    let mut name = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            name.push(char::from(byte));
        } else {
            write!(name, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    Ok(name)
}

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
