use std::{
    collections::HashMap,
    ffi::OsString,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result};
use gix::{
    ObjectId,
    bstr::{BString, ByteSlice, ByteVec},
    objs::commit::ref_iter::Token,
};

use crate::app::CommitRow;

pub(crate) type Decorations = HashMap<ObjectId, Vec<BString>>;
const COMMIT_BATCH_SIZE: usize = 1024;

#[derive(Debug)]
pub(crate) enum Event {
    Decorations(Decorations),
    Commits(Vec<CommitRow>),
    Complete,
    Cancelled,
}

pub(crate) fn load(
    repository: &Path,
    revisions: &[OsString],
    cancelled: &AtomicBool,
    mut emit: impl FnMut(Event) -> bool,
) -> Result<()> {
    let repo =
        gix::open(repository).with_context(|| format!("could not open repository at {}", repository.display()))?;
    let tips = if revisions.is_empty() {
        match repo
            .head()
            .context("could not read HEAD")?
            .try_peel_to_id()
            .context("could not resolve HEAD")?
        {
            Some(id) => vec![id.detach()],
            None => {
                emit(Event::Decorations(decorations(&repo)?));
                emit(Event::Complete);
                return Ok(());
            }
        }
    } else {
        revisions
            .iter()
            .map(|revision| {
                let revision = gix::path::os_str_into_bstr(revision)
                    .with_context(|| format!("revision {} is not valid UTF-8", revision.to_string_lossy()))?;
                repo.rev_parse_single(revision)
                    .with_context(|| format!("could not resolve revision {revision}"))?
                    .object()
                    .context("could not read revision")?
                    .peel_to_kind(gix::object::Kind::Commit)
                    .context("revision does not resolve to a commit")
                    .map(|object| object.id)
            })
            .collect::<Result<Vec<_>>>()?
    };

    if !emit(Event::Decorations(decorations(&repo)?)) {
        return Ok(());
    }
    let walk = repo
        .rev_walk(tips)
        .sorting(gix::revision::walk::Sorting::ByCommitTime(Default::default()))
        .all()
        .context("could not start revision walk")?;
    let mut rows = Vec::with_capacity(COMMIT_BATCH_SIZE);
    for info in walk {
        if cancelled.load(Ordering::Relaxed) {
            emit(Event::Cancelled);
            return Ok(());
        }
        let info = info.context("could not traverse revision history")?;
        let object = info.object().context("could not read commit")?;
        let mut committer_time = None;
        let mut author_name = None;
        let mut subject = None;
        for token in object.iter() {
            match token.context("could not decode commit")? {
                Token::Author { signature } => {
                    author_name = Some(signature.trim().name.to_owned());
                }
                Token::Committer { signature } => {
                    committer_time = Some(signature.time().context("could not decode committer time")?);
                }
                Token::Message(message) => {
                    subject = Some(
                        gix::objs::commit::MessageRef::from_bytes(message)
                            .summary()
                            .into_owned(),
                    );
                }
                _ => {}
            }
        }
        rows.push(CommitRow {
            id: info.id,
            parent_ids: info.parent_ids,
            lane: String::new(),
            committer_time: committer_time.context("commit has no committer time")?,
            author_name: author_name.context("commit has no author name")?,
            subject: subject.context("commit has no message")?,
        });
        if rows.len() == COMMIT_BATCH_SIZE
            && !emit(Event::Commits(std::mem::replace(
                &mut rows,
                Vec::with_capacity(COMMIT_BATCH_SIZE),
            )))
        {
            return Ok(());
        }
    }
    if !rows.is_empty() && !emit(Event::Commits(rows)) {
        return Ok(());
    }
    emit(Event::Complete);
    Ok(())
}

fn decorations(repo: &gix::Repository) -> Result<Decorations> {
    let mut out = Decorations::new();
    for reference in repo
        .references()
        .context("could not open references")?
        .all()
        .context("could not iterate references")?
    {
        let mut reference = reference.map_err(|err| anyhow::anyhow!("could not read reference: {err}"))?;
        let id = reference.peel_to_id().context("could not peel reference")?.detach();
        let mut name = reference.name().shorten().to_owned();
        if reference.name().as_bstr().starts_with_str("refs/tags/") {
            name.insert_str(0, "tag: ");
        }
        out.entry(id).or_default().push(name);
    }
    if let Some(id) = repo
        .head()
        .context("could not read HEAD")?
        .try_peel_to_id()
        .context("could not peel HEAD")?
    {
        out.entry(id.detach()).or_default().push("HEAD".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, process::Command};

    use super::*;

    fn fixture() -> gix_testtools::Result<std::path::PathBuf> {
        gix_testtools::scripted_fixture_read_only("history.sh")
    }

    fn loaded(path: &Path, revisions: &[&str]) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        load(
            path,
            &revisions.iter().map(OsString::from).collect::<Vec<_>>(),
            &AtomicBool::new(false),
            |event| {
                events.push(event);
                true
            },
        )?;
        Ok(events)
    }

    #[test]
    fn walks_the_same_reachable_set_as_git_for_multiple_tips() -> gix_testtools::Result {
        let fixture = fixture()?;
        let events = loaded(&fixture, &["main", "topic"])?;
        let actual: HashSet<_> = events
            .iter()
            .flat_map(|event| match event {
                Event::Commits(rows) => rows.iter().map(|row| row.id.to_hex().to_string()).collect(),
                _ => Vec::new(),
            })
            .collect();
        let output = Command::new("git")
            .current_dir(&fixture)
            .args(["rev-list", "main", "topic", "--"])
            .output()?;
        assert!(
            output.status.success(),
            "git rev-list provides the reference result: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = String::from_utf8(output.stdout)?.lines().map(str::to_owned).collect();
        assert_eq!(actual, expected, "all commits reachable from either tip are shown once");
        assert!(matches!(events.last(), Some(Event::Complete)), "the walk completes");
        let topic = events
            .iter()
            .filter_map(|event| match event {
                Event::Commits(rows) => rows.iter().find(|row| row.subject == "topic"),
                _ => None,
            })
            .next()
            .expect("the topic commit is reachable");
        assert_eq!(topic.author_name, "author", "the author name is retained");
        assert_eq!(
            topic.committer_time.format_or_unix(gix::date::time::format::SHORT),
            "2000-01-04",
            "the committer date is retained"
        );
        Ok(())
    }

    #[test]
    fn reports_decorations_and_honours_cancellation() -> gix_testtools::Result {
        let fixture = fixture()?;
        let events = loaded(&fixture, &["main"])?;
        let Event::Decorations(decorations) = &events[0] else {
            panic!("decorations are sent first")
        };
        assert!(
            decorations.values().flatten().any(|name| name == "tag: v1"),
            "annotated tags decorate their commit"
        );

        let mut cancelled = Vec::new();
        load(&fixture, &[], &AtomicBool::new(true), |event| {
            cancelled.push(event);
            true
        })?;
        assert!(
            matches!(cancelled.as_slice(), [Event::Decorations(_), Event::Cancelled]),
            "cancellation preserves decorations and stops before commits"
        );
        Ok(())
    }
}
