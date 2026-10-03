// Copyright 2026 The Jujutsu Authors
// SPDX-License-Identifier: Apache-2.0

//! Operation-versioned, canonical selection and invertible working-copy layout.
//!
//! Rules are evaluated in order, starting with the empty selection. Mappings
//! describe domains, not the current tree: ownership must hold for future files.

pub use jj_core::working_copy_patterns::{
    SparseExpression, SparseRule, WorkingCopyMapping, WorkingCopyPathBuf, WorkingCopyPatterns,
    WorkingCopyPatternsError,
};

use crate::fileset::{FilePattern, FilesetExpression};
use crate::matchers::PathGlobPattern;

impl From<FilesetExpression> for SparseExpression {
    fn from(expression: FilesetExpression) -> Self {
        match expression {
            FilesetExpression::None => Self::None,
            FilesetExpression::All => Self::All,
            FilesetExpression::Pattern(pattern) => match pattern {
                FilePattern::FilePath(path) => Self::FilePath(path),
                FilePattern::PrefixPath(path) => {
                    if path.is_root() {
                        Self::All
                    } else {
                        Self::PrefixPath(path)
                    }
                }
                FilePattern::FileGlob { dir, pattern } => Self::FileGlob {
                    dir,
                    pattern: pattern.as_str().to_owned(),
                    case_insensitive: pattern.is_case_insensitive(),
                },
                FilePattern::PrefixGlob { dir, pattern } => Self::PrefixGlob {
                    dir,
                    pattern: pattern.as_str().to_owned(),
                    case_insensitive: pattern.is_case_insensitive(),
                },
            },
            FilesetExpression::UnionAll(items) => {
                let mut items: Vec<_> = items.into_iter().map(Self::from).collect();
                match items.len() {
                    0 => Self::None,
                    1 => items.pop().unwrap(),
                    _ => Self::UnionAll(items),
                }
            }
            FilesetExpression::Intersection(a, b) => {
                Self::Intersection(Box::new(Self::from(*a)), Box::new(Self::from(*b)))
            }
            FilesetExpression::Difference(a, b) => {
                Self::Difference(Box::new(Self::from(*a)), Box::new(Self::from(*b)))
            }
        }
    }
}

impl From<&SparseExpression> for FilesetExpression {
    fn from(expression: &SparseExpression) -> Self {
        match expression {
            SparseExpression::None => FilesetExpression::None,
            SparseExpression::All => FilesetExpression::All,
            SparseExpression::FilePath(path) => FilesetExpression::file_path(path.clone()),
            SparseExpression::PrefixPath(path) => FilesetExpression::prefix_path(path.clone()),
            SparseExpression::FileGlob {
                dir,
                pattern,
                case_insensitive,
            } => FilesetExpression::pattern(FilePattern::FileGlob {
                dir: dir.clone(),
                pattern: Box::new(
                    (if *case_insensitive {
                        PathGlobPattern::parse_i(pattern)
                    } else {
                        PathGlobPattern::parse(pattern)
                    })
                    .expect("validated sparse glob"),
                ),
            }),
            SparseExpression::PrefixGlob {
                dir,
                pattern,
                case_insensitive,
            } => FilesetExpression::pattern(FilePattern::PrefixGlob {
                dir: dir.clone(),
                pattern: Box::new(
                    (if *case_insensitive {
                        PathGlobPattern::parse_i(pattern)
                    } else {
                        PathGlobPattern::parse(pattern)
                    })
                    .expect("validated sparse glob"),
                ),
            }),
            SparseExpression::UnionAll(items) => {
                FilesetExpression::union_all(items.iter().map(FilesetExpression::from).collect())
            }
            SparseExpression::Intersection(a, b) => {
                FilesetExpression::from(&**a).intersection(FilesetExpression::from(&**b))
            }
            SparseExpression::Difference(a, b) => {
                FilesetExpression::from(&**a).difference(FilesetExpression::from(&**b))
            }
        }
    }
}

impl From<FilesetExpression> for WorkingCopyPatterns {
    fn from(expression: FilesetExpression) -> Self {
        let expression = SparseExpression::from(expression);
        if expression == SparseExpression::None {
            return Self::none();
        }
        Self {
            rules: vec![SparseRule {
                include: true,
                expression,
            }],
            mappings: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_path::RepoPathBuf;

    fn path(value: &str) -> RepoPathBuf {
        RepoPathBuf::from_internal_string(value).unwrap()
    }

    #[test]
    fn structured_expression_preserves_literal_glob_directories() {
        let expression = FilesetExpression::union_all(vec![
            FilesetExpression::pattern(FilePattern::FileGlob {
                dir: path("literal[dir]"),
                pattern: Box::new(PathGlobPattern::parse_i("*.RS").unwrap()),
            }),
            FilesetExpression::pattern(FilePattern::PrefixGlob {
                dir: path("other"),
                pattern: Box::new(PathGlobPattern::parse("lib*").unwrap()),
            }),
        ])
        .intersection(FilesetExpression::all())
        .difference(FilesetExpression::file_path(path("literal[dir]/skip.rs")));
        let expected = expression.to_matcher();
        let config = WorkingCopyPatterns::from(expression);
        let decoded = WorkingCopyPatterns::decode(&config.encode()).unwrap();
        assert_eq!(decoded, config);
        assert_eq!(decoded.id(), config.id());
        let actual = decoded.to_matcher();
        let restored = FilesetExpression::from(&decoded.rules[0].expression);
        assert_eq!(
            SparseExpression::from(restored.clone()),
            decoded.rules[0].expression
        );
        let restored = restored.to_matcher();
        for candidate in [
            "literal[dir]/a.rs",
            "literald/a.rs",
            "literal[dir]/skip.rs",
            "literal[dir]/a.txt",
            "other/library/a",
            "other/no/a",
            "unrelated",
        ] {
            assert_eq!(
                actual.matches(&path(candidate)),
                expected.matches(&path(candidate)),
                "{candidate}"
            );
            assert_eq!(
                restored.matches(&path(candidate)),
                expected.matches(&path(candidate)),
                "{candidate}"
            );
        }
        assert!(actual.matches(&path("literal[dir]/a.rs")));
        assert!(!actual.matches(&path("literald/a.rs")));
    }
}
