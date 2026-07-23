use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result};
use gix::{
    ObjectId,
    bstr::{BStr, BString, ByteSlice, ByteVec},
    objs::commit::ref_iter::Token,
};

use crate::app::{Commit, LoadedCommit};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Decoration {
    pub name: BString,
    pub kind: DecorationKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecorationKind {
    Head,
    Local,
    Remote,
    Tag,
    AnnotatedTag,
    Special,
}

pub(crate) type Decorations = HashMap<ObjectId, Vec<Decoration>>;
pub(crate) type AuthorNames = HashSet<&'static [u8]>;
const COMMIT_BATCH_SIZE: usize = 1024;

#[derive(Debug)]
pub(crate) enum Event {
    Decorations(Decorations),
    Commits(Vec<LoadedCommit>),
    Complete,
    Cancelled,
}

pub(crate) fn load(
    repo: &gix::Repository,
    revisions: &[OsString],
    hidden_revisions: &[OsString],
    author_names: &mut AuthorNames,
    cancelled: &AtomicBool,
    mut emit: impl FnMut(Event) -> bool,
) -> Result<()> {
    let tips = if revisions.is_empty() {
        match repo
            .head()
            .context("could not read HEAD")?
            .try_peel_to_id()
            .context("could not resolve HEAD")?
        {
            Some(id) => vec![id.detach()],
            None => {
                emit(Event::Decorations(decorations(repo)?));
                emit(Event::Complete);
                return Ok(());
            }
        }
    } else {
        resolve_revisions(repo, revisions, "")?
    };
    let hidden_tips = resolve_revisions(repo, hidden_revisions, "hidden ")?;

    if !emit(Event::Decorations(decorations(repo)?)) {
        return Ok(());
    }
    let walk = repo
        .rev_walk(tips)
        .with_hidden(hidden_tips)
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
        let mut title = None;
        for token in object.iter() {
            match token.context("could not decode commit")? {
                Token::Author { signature } => {
                    author_name = Some(intern(author_names, signature.trim().name));
                }
                Token::Committer { signature } => {
                    committer_time = Some(signature.time().context("could not decode committer time")?);
                }
                Token::Message(message) => {
                    title = Some(
                        gix::objs::commit::MessageRef::from_bytes(message)
                            .summary()
                            .into_owned(),
                    );
                }
                _ => {}
            }
        }
        rows.push(Commit {
            id: info.id,
            parent_ids: info.parent_ids,
            lane: String::new(),
            committer_time: committer_time.context("commit has no committer time")?,
            author_name: author_name.context("commit has no author name")?,
            title: title.context("commit has no message")?,
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

fn resolve_revisions(repo: &gix::Repository, revisions: &[OsString], kind: &str) -> Result<Vec<ObjectId>> {
    revisions
        .iter()
        .map(|revision| {
            let revision = gix::path::os_str_into_bstr(revision)
                .with_context(|| format!("{kind}revision {} is not valid UTF-8", revision.to_string_lossy()))?;
            repo.rev_parse_single(revision)
                .with_context(|| format!("could not resolve {kind}revision {revision}"))?
                .object()
                .with_context(|| format!("could not read {kind}revision"))?
                .peel_to_kind(gix::object::Kind::Commit)
                .with_context(|| format!("{kind}revision does not resolve to a commit"))
                .map(|object| object.id)
        })
        .collect()
}

fn intern(names: &mut AuthorNames, name: &[u8]) -> &'static BStr {
    match names.get(name) {
        Some(name) => name.as_bstr(),
        None => {
            let name: &'static [u8] = Box::leak(name.to_vec().into_boxed_slice());
            names.insert(name);
            name.as_bstr()
        }
    }
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
        let mut kind = decoration_kind(reference.name().as_bstr());
        if kind == DecorationKind::Tag {
            let annotated = match reference.try_id() {
                Some(id) => id.header().context("could not inspect tag")?.kind() == gix::objs::Kind::Tag,
                None => false,
            };
            if annotated {
                kind = DecorationKind::AnnotatedTag;
            }
        }
        let id = reference.peel_to_id().context("could not peel reference")?.detach();
        let mut name = reference.name().shorten().to_owned();
        if matches!(kind, DecorationKind::Tag | DecorationKind::AnnotatedTag) {
            name.insert_str(0, "tag: ");
        }
        out.entry(id).or_default().push(Decoration { name, kind });
    }
    if let Some(id) = repo
        .head()
        .context("could not read HEAD")?
        .try_peel_to_id()
        .context("could not peel HEAD")?
    {
        out.entry(id.detach()).or_default().push(Decoration {
            name: "HEAD".into(),
            kind: DecorationKind::Head,
        });
    }
    Ok(out)
}

fn decoration_kind(name: &[u8]) -> DecorationKind {
    if name.starts_with(b"refs/heads/") {
        DecorationKind::Local
    } else if name.starts_with(b"refs/tags/") {
        DecorationKind::Tag
    } else if name.starts_with(b"refs/remotes/") {
        DecorationKind::Remote
    } else {
        DecorationKind::Special
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, process::Command};

    use super::*;

    fn fixture() -> gix_testtools::Result<std::path::PathBuf> {
        gix_testtools::scripted_fixture_read_only("history.sh")
    }

    fn loaded(path: &std::path::Path, revisions: &[&str], hidden_revisions: &[&str]) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        let mut author_names = AuthorNames::new();
        let repo = gix::open(path)?;
        load(
            &repo,
            &revisions.iter().map(OsString::from).collect::<Vec<_>>(),
            &hidden_revisions.iter().map(OsString::from).collect::<Vec<_>>(),
            &mut author_names,
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
        let events = loaded(&fixture, &["main", "topic"], &[])?;
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
                Event::Commits(rows) => rows.iter().find(|row| row.title == "topic"),
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
    fn hides_tips_and_every_commit_reachable_from_them() -> gix_testtools::Result {
        let fixture = fixture()?;
        let events = loaded(&fixture, &["topic"], &["main"])?;
        let actual: HashSet<_> = events
            .iter()
            .flat_map(|event| match event {
                Event::Commits(rows) => rows.iter().map(|row| row.id.to_hex().to_string()).collect(),
                _ => Vec::new(),
            })
            .collect();
        let output = Command::new("git")
            .current_dir(&fixture)
            .args(["rev-list", "topic", "--not", "main", "--"])
            .output()?;
        assert!(
            output.status.success(),
            "git rev-list provides the reference result: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = String::from_utf8(output.stdout)?.lines().map(str::to_owned).collect();
        assert_eq!(actual, expected, "hidden tips use Git's exclusion semantics");
        assert!(
            matches!(events.last(), Some(Event::Complete)),
            "the filtered walk completes"
        );
        Ok(())
    }

    #[test]
    fn reports_decorations_and_honours_cancellation() -> gix_testtools::Result {
        let fixture = fixture()?;
        let events = loaded(&fixture, &["main"], &[])?;
        let Event::Decorations(decorations) = &events[0] else {
            panic!("decorations are sent first")
        };
        assert!(
            decorations
                .values()
                .flatten()
                .any(|decoration| { decoration.name == "tag: v1" && decoration.kind == DecorationKind::AnnotatedTag }),
            "annotated tags decorate their commit"
        );

        let mut cancelled = Vec::new();
        let mut author_names = AuthorNames::new();
        let repo = gix::open(&fixture)?;
        load(&repo, &[], &[], &mut author_names, &AtomicBool::new(true), |event| {
            cancelled.push(event);
            true
        })?;
        assert!(
            matches!(cancelled.as_slice(), [Event::Decorations(_), Event::Cancelled]),
            "cancellation preserves decorations and stops before commits"
        );
        Ok(())
    }

    #[test]
    fn classifies_reference_kinds() {
        assert_eq!(decoration_kind(b"refs/heads/main"), DecorationKind::Local);
        assert_eq!(decoration_kind(b"refs/tags/v1"), DecorationKind::Tag);
        assert_eq!(decoration_kind(b"refs/remotes/origin/main"), DecorationKind::Remote);
        assert_eq!(decoration_kind(b"refs/patches/main/patch"), DecorationKind::Special);
        assert_eq!(decoration_kind(b"refs/stash"), DecorationKind::Special);
    }

    #[test]
    fn interns_author_names_as_raw_bytes() {
        let mut names = HashSet::new();

        let first = intern(&mut names, b"author\xff");
        let second = intern(&mut names, b"author\xff");
        let other = intern(&mut names, b"other");

        assert!(std::ptr::eq(first, second), "equal names share one allocation");
        assert!(!std::ptr::eq(first, other), "different names remain distinct");
        assert_eq!(names.len(), 2);
        assert_eq!(first, b"author\xff".as_bstr(), "Git names remain byte strings");
    }
}
