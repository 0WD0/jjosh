// Copyright 2026 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Utilities for converting between `RepoPath`s and plain strings as displayed
//! to the user (e.g. relative to CWD).

use std::iter;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;

use crate::file_util;
use crate::matchers::Matcher;
use crate::merge::Diff;
use crate::repo_path::RelativePathParseError;
use crate::repo_path::RepoPath;
use crate::repo_path::RepoPathBuf;
use crate::working_copy_patterns::WorkingCopyPathBuf;
use crate::working_copy_patterns::WorkingCopyPatterns;

/// An error which occurs when we're parsing paths.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error(r#"Path "{input}" is not in the repo "{base}""#)]
pub struct FsPathParseError {
    /// Repository or workspace root path relative to the `cwd`.
    pub base: Box<Path>,
    /// Input path without normalization.
    pub input: Box<Path>,
    /// Source error.
    pub source: RelativePathParseError,
}

/// An error from `RepoPathUiConverter::parse_file_path`.
#[derive(Debug, Error)]
pub enum UiPathParseError {
    /// Failure to parse a path a relative path inside the repo.
    #[error(transparent)]
    Fs(FsPathParseError),
    /// A physical path has no unambiguous canonical interpretation.
    #[error("{0}; use a canonical root: fileset instead")]
    Mapping(String),
}

/// Converts `RepoPath`s to and from plain strings as displayed to the user
/// (e.g. relative to CWD).
#[derive(Debug, Clone)]
pub enum RepoPathUiConverter {
    /// Variant for a local file system. Paths are interpreted relative to `cwd`
    /// with the repo rooted in `base`.
    ///
    /// The `cwd` and `base` paths are supposed to be absolute and normalized in
    /// the same manner.
    Fs {
        /// The directory to which relative paths are interpreted.
        cwd: PathBuf,
        /// The repository root path.
        base: PathBuf,
    },
    /// Filesystem coordinates translated using the actual checkout layout.
    MappedFs {
        /// The current directory.
        cwd: PathBuf,
        /// The physical workspace root.
        base: PathBuf,
        /// Actual, not desired, working-copy configuration.
        patterns: Box<WorkingCopyPatterns>,
        /// Compiled canonical selection, shared by cloned converters.
        matcher: Arc<dyn Matcher>,
    },
    // TODO: Add a no-op variant that uses the internal `RepoPath` representation. Can be useful
    // on a server.
}

impl RepoPathUiConverter {
    /// Format a path for display in the UI.
    pub fn format_file_path(&self, file: &RepoPath) -> String {
        match self {
            Self::Fs { cwd, base } => {
                file_util::relative_path(cwd, &file.to_fs_path_unchecked(base))
                    .display()
                    .to_string()
            }
            Self::MappedFs {
                cwd,
                base,
                patterns,
                matcher,
            } => {
                if !matcher.matches(file) {
                    return format!("root:{}", file.as_internal_file_string());
                }
                match patterns.repo_to_wc_with_matcher(file, matcher.as_ref()) {
                    Ok(Some(path)) => file_util::relative_path(
                        cwd,
                        &path.as_repo_path().to_fs_path_unchecked(base),
                    )
                    .display()
                    .to_string(),
                    _ => format!("root:{}", file.as_internal_file_string()),
                }
            }
        }
    }

    /// Format a copy from `before` to `after` for display in the UI by
    /// extracting common components and producing something like
    /// "common/prefix/{before => after}/common/suffix".
    ///
    /// If `before == after`, this is equivalent to `format_file_path()`.
    pub fn format_copied_path(&self, paths: Diff<&RepoPath>) -> String {
        match self {
            Self::Fs { .. } | Self::MappedFs { .. } => {
                let paths = paths.map(|path| self.format_file_path(path));
                collapse_copied_path(paths.as_deref(), std::path::MAIN_SEPARATOR)
            }
        }
    }

    /// Parses a path from the UI.
    ///
    /// It's up to the implementation whether absolute paths are allowed, and
    /// where relative paths are interpreted as relative to.
    pub fn parse_file_path(&self, input: &str) -> Result<RepoPathBuf, UiPathParseError> {
        match self {
            Self::Fs { cwd, base } => parse_fs_path(cwd, base, input).map_err(UiPathParseError::Fs),
            Self::MappedFs {
                cwd,
                base,
                patterns,
                matcher,
            } => {
                let path = parse_fs_path(cwd, base, input).map_err(UiPathParseError::Fs)?;
                patterns
                    .wc_to_repo_with_matcher(
                        &WorkingCopyPathBuf::from_repo_path(path),
                        matcher.as_ref(),
                    )
                    .map_err(|err| UiPathParseError::Mapping(err.to_string()))?
                    .ok_or_else(|| {
                        UiPathParseError::Mapping(format!(
                            "Path {input:?} is outside the mapped working copy"
                        ))
                    })
            }
        }
    }

    /// Physical current directory.
    pub fn cwd(&self) -> &Path {
        match self {
            Self::Fs { cwd, .. } | Self::MappedFs { cwd, .. } => cwd,
        }
    }

    /// Physical workspace root.
    pub fn base(&self) -> &Path {
        match self {
            Self::Fs { base, .. } | Self::MappedFs { base, .. } => base,
        }
    }

    /// Set the actual layout after a checkout or deferred synchronization.
    pub fn with_patterns(&self, patterns: WorkingCopyPatterns) -> Self {
        if patterns.is_identity() {
            Self::Fs {
                cwd: self.cwd().to_owned(),
                base: self.base().to_owned(),
            }
        } else {
            let matcher = Arc::from(patterns.to_matcher());
            Self::MappedFs {
                cwd: self.cwd().to_owned(),
                base: self.base().to_owned(),
                patterns: Box::new(patterns),
                matcher,
            }
        }
    }

    /// Reject broad physical patterns whose canonical image is not one prefix.
    /// Callers can always express the same intent using canonical root filesets.
    pub fn check_prefix_path(&self, input: &str) -> Result<(), UiPathParseError> {
        let Self::MappedFs {
            cwd,
            base,
            patterns,
            ..
        } = self
        else {
            return Ok(());
        };
        let physical = parse_fs_path(cwd, base, input).map_err(UiPathParseError::Fs)?;
        let canonical = self.parse_file_path(input)?;
        if patterns.mappings.iter().any(|mapping| {
            (mapping.destination.as_repo_path() != physical.as_ref()
                && mapping.destination.as_repo_path().starts_with(&physical))
                || (mapping.source != canonical && mapping.source.starts_with(&canonical))
        }) {
            return Err(UiPathParseError::Mapping(format!(
                "Path {input:?} spans mapping boundaries"
            )));
        }
        Ok(())
    }

    /// Physical globs need a matcher image, not substitution of a literal prefix.
    pub fn check_glob(&self) -> Result<(), UiPathParseError> {
        if matches!(self, Self::MappedFs { .. }) {
            return Err(UiPathParseError::Mapping(
                "Working-copy globs are not supported with path mappings".to_owned(),
            ));
        }
        Ok(())
    }
}

fn collapse_copied_path(paths: Diff<&str>, separator: char) -> String {
    // The last component should never match middle components. This is ensured
    // by including trailing separators. e.g. ("a/b", "a/b/x") => ("a/", _)
    let components = paths.map(|path| path.split_inclusive(separator));
    let prefix_len: usize = iter::zip(components.before, components.after)
        .take_while(|(before, after)| before == after)
        .map(|(_, after)| after.len())
        .sum();
    if paths.before.len() == prefix_len && paths.after.len() == prefix_len {
        return paths.after.to_owned();
    }

    // The first component should never match middle components, but the first
    // uncommon middle component can. e.g. ("a/b", "x/a/b") => ("", "/b"),
    // ("a/b", "a/x/b") => ("a/", "/b")
    let components = paths.map(|path| {
        let mut remainder = &path[prefix_len.saturating_sub(1)..];
        iter::from_fn(move || {
            let pos = remainder.rfind(separator)?;
            let (prefix, last) = remainder.split_at(pos);
            remainder = prefix;
            Some(last)
        })
    });
    let suffix_len: usize = iter::zip(components.before, components.after)
        .take_while(|(before, after)| before == after)
        .map(|(_, after)| after.len())
        .sum();

    // Middle range may be invalid (start > end) because the same separator char
    // can be distributed to both common prefix and suffix. e.g.
    // ("a/b", "a/x/b") == ("a//b", "a/x/b") => ("a/", "/b")
    let middle = paths.map(|path| path.get(prefix_len..path.len() - suffix_len).unwrap_or(""));

    let mut collapsed = String::new();
    collapsed.push_str(&paths.after[..prefix_len]);
    collapsed.push('{');
    collapsed.push_str(middle.before);
    collapsed.push_str(" => ");
    collapsed.push_str(middle.after);
    collapsed.push('}');
    collapsed.push_str(&paths.after[paths.after.len() - suffix_len..]);
    collapsed
}

/// Parses an `input` path into a `RepoPathBuf` relative to `base`.
///
/// The `cwd` and `base` paths are supposed to be absolute and normalized in
/// the same manner. The `input` path may be either relative to `cwd` or
/// absolute.
pub fn parse_fs_path(
    cwd: &Path,
    base: &Path,
    input: impl AsRef<Path>,
) -> Result<RepoPathBuf, FsPathParseError> {
    let input = input.as_ref();
    let abs_input_path = file_util::normalize_path(&cwd.join(input));
    let repo_relative_path = file_util::relative_path(base, &abs_input_path);
    RepoPathBuf::from_relative_path(repo_relative_path).map_err(|source| FsPathParseError {
        base: file_util::relative_path(cwd, base).into(),
        input: input.into(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::*;
    use crate::tests::new_temp_dir;

    fn repo_path(value: &str) -> &RepoPath {
        RepoPath::from_internal_string(value).unwrap()
    }

    #[test]
    fn test_format_copied_path() {
        let ui = RepoPathUiConverter::Fs {
            cwd: PathBuf::from("."),
            base: PathBuf::from("."),
        };

        let format = |before, after| {
            ui.format_copied_path(Diff::new(repo_path(before), repo_path(after)))
                .replace('\\', "/")
        };

        assert_eq!(format("one/two/three", "one/two/three"), "one/two/three");
        assert_eq!(format("one/two", "one/two/three"), "one/{two => two/three}");
        assert_eq!(format("one/two", "zero/one/two"), "{one => zero/one}/two");
        assert_eq!(format("one/two/three", "one/two"), "one/{two/three => two}");
        assert_eq!(format("zero/one/two", "one/two"), "{zero/one => one}/two");
        assert_eq!(
            format("one/two", "one/two/three/one/two"),
            "one/{ => two/three/one}/two"
        );

        assert_eq!(format("two/three", "four/three"), "{two => four}/three");
        assert_eq!(
            format("one/two/three", "one/four/three"),
            "one/{two => four}/three"
        );
        assert_eq!(format("one/two/three", "one/three"), "one/{two => }/three");
        assert_eq!(format("one/two", "one/four"), "one/{two => four}");
        assert_eq!(format("two", "four"), "{two => four}");
        assert_eq!(format("file1", "file2"), "{file1 => file2}");
        assert_eq!(format("file-1", "file-2"), "{file-1 => file-2}");
        assert_eq!(
            format("x/something/something/2to1.txt", "x/something/2to1.txt"),
            "x/something/{something => }/2to1.txt"
        );
        assert_eq!(
            format("x/something/1to2.txt", "x/something/something/1to2.txt"),
            "x/something/{ => something}/1to2.txt"
        );
    }

    #[test]
    fn parse_fs_path_wc_in_cwd() {
        let temp_dir = new_temp_dir();
        let cwd_path = temp_dir.path().join("repo");
        let wc_path = &cwd_path;

        assert_eq!(
            parse_fs_path(&cwd_path, wc_path, "").as_deref(),
            Ok(RepoPath::root())
        );
        assert_eq!(
            parse_fs_path(&cwd_path, wc_path, ".").as_deref(),
            Ok(RepoPath::root())
        );
        assert_eq!(
            parse_fs_path(&cwd_path, wc_path, "file").as_deref(),
            Ok(repo_path("file"))
        );
        // Both slash and the platform's separator are allowed
        assert_eq!(
            parse_fs_path(
                &cwd_path,
                wc_path,
                format!("dir{}file", std::path::MAIN_SEPARATOR)
            )
            .as_deref(),
            Ok(repo_path("dir/file"))
        );
        assert_eq!(
            parse_fs_path(&cwd_path, wc_path, "dir/file").as_deref(),
            Ok(repo_path("dir/file"))
        );
        assert_matches!(
            parse_fs_path(&cwd_path, wc_path, ".."),
            Err(FsPathParseError {
                source: RelativePathParseError::InvalidComponent { .. },
                ..
            })
        );
        assert_eq!(
            parse_fs_path(&cwd_path, &cwd_path, "../repo").as_deref(),
            Ok(RepoPath::root())
        );
        assert_eq!(
            parse_fs_path(&cwd_path, &cwd_path, "../repo/file").as_deref(),
            Ok(repo_path("file"))
        );
        // Input may be absolute path with ".."
        assert_eq!(
            parse_fs_path(
                &cwd_path,
                &cwd_path,
                cwd_path.join("../repo").to_str().unwrap()
            )
            .as_deref(),
            Ok(RepoPath::root())
        );
    }

    #[test]
    fn parse_fs_path_wc_in_cwd_parent() {
        let temp_dir = new_temp_dir();
        let cwd_path = temp_dir.path().join("dir");
        let wc_path = cwd_path.parent().unwrap().to_path_buf();

        assert_eq!(
            parse_fs_path(&cwd_path, &wc_path, "").as_deref(),
            Ok(repo_path("dir"))
        );
        assert_eq!(
            parse_fs_path(&cwd_path, &wc_path, ".").as_deref(),
            Ok(repo_path("dir"))
        );
        assert_eq!(
            parse_fs_path(&cwd_path, &wc_path, "file").as_deref(),
            Ok(repo_path("dir/file"))
        );
        assert_eq!(
            parse_fs_path(&cwd_path, &wc_path, "subdir/file").as_deref(),
            Ok(repo_path("dir/subdir/file"))
        );
        assert_eq!(
            parse_fs_path(&cwd_path, &wc_path, "..").as_deref(),
            Ok(RepoPath::root())
        );
        assert_matches!(
            parse_fs_path(&cwd_path, &wc_path, "../.."),
            Err(FsPathParseError {
                source: RelativePathParseError::InvalidComponent { .. },
                ..
            })
        );
        assert_eq!(
            parse_fs_path(&cwd_path, &wc_path, "../other-dir/file").as_deref(),
            Ok(repo_path("other-dir/file"))
        );
    }

    #[test]
    fn parse_fs_path_wc_in_cwd_child() {
        let temp_dir = new_temp_dir();
        let cwd_path = temp_dir.path().join("cwd");
        let wc_path = cwd_path.join("repo");

        assert_matches!(
            parse_fs_path(&cwd_path, &wc_path, ""),
            Err(FsPathParseError {
                source: RelativePathParseError::InvalidComponent { .. },
                ..
            })
        );
        assert_matches!(
            parse_fs_path(&cwd_path, &wc_path, "not-repo"),
            Err(FsPathParseError {
                source: RelativePathParseError::InvalidComponent { .. },
                ..
            })
        );
        assert_eq!(
            parse_fs_path(&cwd_path, &wc_path, "repo").as_deref(),
            Ok(RepoPath::root())
        );
        assert_eq!(
            parse_fs_path(&cwd_path, &wc_path, "repo/file").as_deref(),
            Ok(repo_path("file"))
        );
        assert_eq!(
            parse_fs_path(&cwd_path, &wc_path, "repo/dir/file").as_deref(),
            Ok(repo_path("dir/file"))
        );
    }
}
