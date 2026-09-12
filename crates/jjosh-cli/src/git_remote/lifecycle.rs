use jj_cli::cli_util::WorkspaceCommandHelper;
use jj_cli::command_error::{CommandError, user_error};
use jj_lib::git::{GitRemoteManagementOptions, GitRemoteSidecar};
use jj_lib::ref_name::RemoteName;
use jj_lib::repo::Repo as _;

pub(super) fn prepare(
    workspace: &WorkspaceCommandHelper,
    old: &RemoteName,
    new: Option<&RemoteName>,
) -> Result<GitRemoteManagementOptions, CommandError> {
    // Josh configuration cannot be created for names with slashes. Leave such
    // legacy Git remotes to jj's existing rename/remove rules without deriving
    // filesystem paths from them.
    if old.as_str().contains('/') {
        return Ok(GitRemoteManagementOptions::default());
    }
    for name in std::iter::once(old).chain(new) {
        gix::remote::name::validated(name.as_str()).map_err(user_error)?;
    }
    let git = jj_lib::git::get_git_backend(workspace.repo().store())?.git_repo();
    let directory = git.common_dir().join("josh/remotes");
    let old_sidecar = directory.join(format!("{}.josh", old.as_str()));
    if new.is_some_and(|name| name.as_str().contains('/'))
        && (old_sidecar.try_exists().map_err(user_error)?
            || super::config_string(&git, &format!("remote.{}.jjosh-project", old.as_str()))
                .map_err(user_error)?
                .is_some())
    {
        return Err(user_error(
            "Projection remote names must be a single path component",
        ));
    }
    // Only name-keyed filter configuration moves. Endpoint-keyed leases and
    // shared native/project history belong to their endpoint, not this alias.
    Ok(GitRemoteManagementOptions {
        extra_config_keys: &[
            "jjosh-project",
            "jjosh-mount",
            "jjosh-readOnly",
            "jjosh-base",
        ],
        sidecar: Some(GitRemoteSidecar {
            old: old_sidecar,
            new: new.map(|name| directory.join(format!("{}.josh", name.as_str()))),
        }),
        repo_config: None,
    })
}
