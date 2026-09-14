use jj_cli::cli_util::WorkspaceCommandHelper;
use jj_cli::command_error::{CommandError, user_error};
use jj_cli::git_remote::GitRemoteBindingArgs;
use jj_lib::project::{BindingRecord, BindingTarget, ConnectionId, Representation};
use jj_lib::ref_name::RemoteName;
use jj_lib::repo::Repo as _;

/// Prepare semantic state only. The core remote command owns connection policy,
/// the new BindingId, and publication through its recoverable local journal.
pub(super) fn prepare_binding(
    workspace: &WorkspaceCommandHelper,
    remote: &RemoteName,
    connection_id: &ConnectionId,
    args: &GitRemoteBindingArgs,
) -> Result<BindingRecord, CommandError> {
    let git = jj_lib::git::get_git_backend(workspace.repo().store())?.git_repo();
    if git.object_hash() != gix::hash::Kind::Sha1 {
        return Err(user_error("jjosh-v1 history conversion requires a SHA-1 Git backend"));
    }
    for key in ["jjosh-project", "jjosh-mount", "jjosh-base"] {
        if super::config_string(&git, &format!("remote.{}.{key}", remote.as_str())).map_err(user_error)?.is_some() {
            return Err(user_error(format!("Remote {} still has legacy configuration; use project migrate rather than attaching a new definition", remote.as_str())));
        }
    }
    if !remote.as_str().contains('/') {
        let sidecar = git.common_dir().join("josh/remotes").join(format!("{}.josh", remote.as_str()));
        match std::fs::symlink_metadata(&sidecar) {
            Ok(_) => return Err(user_error(format!("Legacy sidecar {} must be migrated or explicitly removed before creating a new binding", sidecar.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(user_error(error)),
        }
    }
    let state = workspace.repo().view().project_state();
    let count = usize::from(args.whole)
        + usize::from(args.filter.is_some())
        + usize::from(args.view.is_some())
        + usize::from(args.like_remote.is_some());
    if count != 1 {
        return Err(user_error("A binding requires exactly one of --whole, --filter, --view, or --like; representations are never inherited implicitly"));
    }
    let project = args.project.as_deref().map(|name| {
        let (id, _) = state.project_by_name(name).map_err(user_error)?;
        state.validate_project(&id).map_err(user_error)?;
        Ok::<_, CommandError>(id)
    }).transpose()?;
    let (target, representation, base) = if let Some(source) = &args.like_remote {
        git.find_remote(source.as_str()).map_err(user_error)?;
        let source_name = RemoteName::new(source);
        jj_lib::git::check_remote_capability(workspace.repo().store(), workspace.repo().view(), source_name, &["jjosh-v1"])
            .map_err(user_error)?;
        let id = jj_lib::git::remote_connection_id(&git, source_name).map_err(user_error)?
            .ok_or_else(|| user_error(format!("Remote {source} has no adopted connection; use project migrate for legacy configuration")))?;
        let (_, source_binding) = state.binding_for_connection(&id).map_err(user_error)?
            .ok_or_else(|| user_error(format!("Remote {source} has no active binding")))?;
        if let Some(project) = &project
            && source_binding.target != BindingTarget::Project(project.clone()) {
                return Err(user_error("--project must agree with the target copied by --like"));
            }
        if args.base.is_some() {
            return Err(user_error("--like copies the base selection rule; do not combine it with --base"));
        }
        (source_binding.target.clone(), source_binding.representation.clone(), source_binding.base.clone())
    } else {
        let representation = if args.whole {
            Representation::Whole
        } else if let Some(filter) = &args.filter {
            crate::binding_config::parse_filter(filter).map_err(user_error)?
        } else {
            crate::binding_config::parse_view(args.view.as_deref().unwrap()).map_err(user_error)?
        };
        (project.map(BindingTarget::Project).unwrap_or(BindingTarget::RepositoryView), representation,
            args.base.as_deref().map(crate::binding_config::parse_base).transpose().map_err(user_error)?)
    };
    if let BindingTarget::Project(project) = &target {
        state.validate_project(project).map_err(user_error)?;
    }
    crate::binding_config::filter(&representation).map_err(user_error)?;
    if target == BindingTarget::RepositoryView && representation == Representation::Whole {
        return Err(user_error("--whole requires --project; omit conversion options for an ordinary repository remote"));
    }
    Ok(BindingRecord { target, connection_id: connection_id.clone(), representation, base })
}
