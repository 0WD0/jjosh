// Copyright 2026 The Jujutsu Authors
// SPDX-License-Identifier: Apache-2.0

//! Operation-versioned, canonical selection and invertible working-copy layout.
//!
//! Rules are evaluated in order, starting with the empty selection. Mappings
//! describe domains, not the current tree: ownership must hold for future files.
#![expect(missing_docs)]

use prost::Message as _;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::content_hash::ContentHash;
use crate::content_hash::blake2b_hash;
use crate::matchers::DifferenceMatcher;
use crate::matchers::EverythingMatcher;
use crate::matchers::FilesMatcher;
use crate::matchers::GlobsMatcher;
use crate::matchers::IntersectionMatcher;
use crate::matchers::Matcher;
use crate::matchers::NothingMatcher;
use crate::matchers::PathGlobPattern;
use crate::matchers::PrefixMatcher;
use crate::matchers::UnionMatcher;
use crate::matchers::Visit;
use crate::matchers::VisitDirs;
use crate::matchers::VisitFiles;
use crate::op_store::WorkingCopyPatternsId;
use crate::repo_path::RepoPath;
use crate::repo_path::RepoPathBuf;

/// Wire representation of canonical working-copy configuration objects.
pub mod proto;

// Prost permits 100 nested messages; Configuration -> Rule -> Expression uses
// two levels before expression children. Do not accept objects we cannot read.
// Breadth and byte length are uncapped so large existing filesets are retained.
const MAX_DEPTH: usize = 98;

#[derive(Clone, Debug, Eq, PartialEq, Error)]
#[error("Invalid working-copy configuration: {0}")]
pub struct WorkingCopyPatternsError(pub String);

fn invalid(message: impl Into<String>) -> WorkingCopyPatternsError {
    WorkingCopyPatternsError(message.into())
}

// RepoPathBuf intentionally has no general Deserialize implementation. All
// persisted paths pass its checked constructor, including serde bundle imports.
mod path_serde {
    use super::*;
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<RepoPathBuf, D::Error> {
        let value = String::deserialize(d)?;
        RepoPathBuf::from_internal_string(value).map_err(serde::de::Error::custom)
    }
}

#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkingCopyPathBuf(#[serde(deserialize_with = "path_serde::deserialize")] RepoPathBuf);

impl WorkingCopyPathBuf {
    pub fn from_repo_path(path: RepoPathBuf) -> Self {
        Self(path)
    }
    pub fn as_repo_path(&self) -> &RepoPath {
        &self.0
    }
    pub fn into_repo_path(self) -> RepoPathBuf {
        self.0
    }
    pub fn as_internal_file_string(&self) -> &str {
        self.0.as_internal_file_string()
    }
}

#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkingCopyMapping {
    #[serde(deserialize_with = "path_serde::deserialize")]
    pub source: RepoPathBuf,
    pub destination: WorkingCopyPathBuf,
    pub recursive: bool,
}

#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SparseRule {
    pub include: bool,
    pub expression: SparseExpression,
}

/// Durable AST; glob directories are literal, even when containing metacharacters.
#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum SparseExpression {
    None,
    All,
    FilePath(#[serde(deserialize_with = "path_serde::deserialize")] RepoPathBuf),
    PrefixPath(#[serde(deserialize_with = "path_serde::deserialize")] RepoPathBuf),
    FileGlob {
        #[serde(deserialize_with = "path_serde::deserialize")]
        dir: RepoPathBuf,
        pattern: String,
        case_insensitive: bool,
    },
    PrefixGlob {
        #[serde(deserialize_with = "path_serde::deserialize")]
        dir: RepoPathBuf,
        pattern: String,
        case_insensitive: bool,
    },
    UnionAll(Vec<Self>),
    Intersection(Box<Self>, Box<Self>),
    Difference(Box<Self>, Box<Self>),
}

fn compile_glob(pattern: &str, icase: bool) -> Result<PathGlobPattern, WorkingCopyPatternsError> {
    (if icase {
        PathGlobPattern::parse_i(pattern)
    } else {
        PathGlobPattern::parse(pattern)
    })
    .map_err(|e| invalid(e.to_string()))
}

impl SparseExpression {
    /// Compiles the durable AST without parsing fileset syntax.
    pub fn to_matcher(&self) -> Box<dyn Matcher> {
        build_union_matcher(std::slice::from_ref(self))
    }

    fn validate(&self, depth: usize) -> Result<(), WorkingCopyPatternsError> {
        if depth > MAX_DEPTH {
            return Err(invalid("expression exceeds protobuf recursion limit"));
        }
        match self {
            Self::FileGlob {
                pattern,
                case_insensitive,
                ..
            }
            | Self::PrefixGlob {
                pattern,
                case_insensitive,
                ..
            } => {
                compile_glob(pattern, *case_insensitive)?;
            }
            Self::UnionAll(items) => {
                for item in items {
                    item.validate(depth + 1)?;
                }
            }
            Self::Intersection(a, b) | Self::Difference(a, b) => {
                a.validate(depth + 1)?;
                b.validate(depth + 1)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn matches(&self, path: &RepoPath) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::FilePath(p) => &**p == path,
            Self::PrefixPath(p) => path.starts_with(p),
            Self::FileGlob { .. } | Self::PrefixGlob { .. } => self.to_matcher().matches(path),
            Self::UnionAll(items) => items.iter().any(|item| item.matches(path)),
            Self::Intersection(a, b) => a.matches(path) && b.matches(path),
            Self::Difference(a, b) => a.matches(path) && !b.matches(path),
        }
    }

    // Sound abstract interpretation over an entire prefix domain (including
    // its root). Unknown glob languages never establish disjoint ownership.
    fn coverage(&self, path: &RepoPath, recursive: bool) -> Coverage {
        if !recursive {
            return if self.matches(path) {
                Coverage::All
            } else {
                Coverage::None
            };
        }
        match self {
            Self::None => Coverage::None,
            Self::All => Coverage::All,
            Self::FilePath(p) => {
                if p.starts_with(path) {
                    Coverage::Some
                } else {
                    Coverage::None
                }
            }
            Self::PrefixPath(p) => {
                if path.starts_with(p) {
                    Coverage::All
                } else if p.starts_with(path) {
                    Coverage::Some
                } else {
                    Coverage::None
                }
            }
            Self::FileGlob { dir, .. } | Self::PrefixGlob { dir, .. } => {
                if path.starts_with(dir) || dir.starts_with(path) {
                    Coverage::Some
                } else {
                    Coverage::None
                }
            }
            Self::UnionAll(items) => items
                .iter()
                .fold(Coverage::None, |a, b| a.union(b.coverage(path, recursive))),
            Self::Intersection(a, b) => a
                .coverage(path, recursive)
                .intersect(b.coverage(path, recursive)),
            Self::Difference(a, b) => a
                .coverage(path, recursive)
                .intersect(b.coverage(path, recursive).not()),
        }
    }

    fn to_proto(&self) -> proto::Expression {
        let mut out = proto::Expression::default();
        match self {
            Self::None => out.kind = 0,
            Self::All => out.kind = 1,
            Self::FilePath(p) | Self::PrefixPath(p) => {
                out.kind = if matches!(self, Self::FilePath(_)) {
                    2
                } else {
                    3
                };
                out.path = p.as_internal_file_string().to_owned();
            }
            Self::FileGlob {
                dir,
                pattern,
                case_insensitive,
            }
            | Self::PrefixGlob {
                dir,
                pattern,
                case_insensitive,
            } => {
                out.kind = if matches!(self, Self::FileGlob { .. }) {
                    4
                } else {
                    5
                };
                out.path = dir.as_internal_file_string().to_owned();
                out.glob = pattern.clone();
                out.case_insensitive = *case_insensitive;
            }
            Self::UnionAll(items) => {
                out.kind = 6;
                out.children = items.iter().map(Self::to_proto).collect();
            }
            Self::Intersection(a, b) | Self::Difference(a, b) => {
                out.kind = if matches!(self, Self::Intersection(..)) {
                    7
                } else {
                    8
                };
                out.children = vec![a.to_proto(), b.to_proto()];
            }
        }
        out
    }

    fn from_proto(p: proto::Expression, depth: usize) -> Result<Self, WorkingCopyPatternsError> {
        if depth > MAX_DEPTH {
            return Err(invalid("expression exceeds protobuf recursion limit"));
        }
        if (!(2..=5).contains(&p.kind) && !p.path.is_empty())
            || (!(4..=5).contains(&p.kind) && (!p.glob.is_empty() || p.case_insensitive))
            || (p.kind < 6 && !p.children.is_empty())
            || ((7..=8).contains(&p.kind) && p.children.len() != 2)
        {
            return Err(invalid("invalid expression fields or arity"));
        }
        let path = || {
            RepoPathBuf::from_internal_string(p.path.clone()).map_err(|e| invalid(e.to_string()))
        };
        Ok(match p.kind {
            0 => Self::None,
            1 => Self::All,
            2 => Self::FilePath(path()?),
            3 => Self::PrefixPath(path()?),
            4 => Self::FileGlob {
                dir: path()?,
                pattern: p.glob,
                case_insensitive: p.case_insensitive,
            },
            5 => Self::PrefixGlob {
                dir: path()?,
                pattern: p.glob,
                case_insensitive: p.case_insensitive,
            },
            6 => Self::UnionAll(
                p.children
                    .into_iter()
                    .map(|p| Self::from_proto(p, depth + 1))
                    .collect::<Result<_, _>>()?,
            ),
            7 | 8 => {
                let mut children = p.children.into_iter();
                let a = Box::new(Self::from_proto(children.next().unwrap(), depth + 1)?);
                let b = Box::new(Self::from_proto(children.next().unwrap(), depth + 1)?);
                if p.kind == 7 {
                    Self::Intersection(a, b)
                } else {
                    Self::Difference(a, b)
                }
            }
            _ => return Err(invalid("unknown expression kind")),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Coverage {
    None,
    Some,
    All,
}
impl Coverage {
    fn union(self, other: Self) -> Self {
        match (self, other) {
            (Self::All, _) | (_, Self::All) => Self::All,
            (Self::None, Self::None) => Self::None,
            _ => Self::Some,
        }
    }
    fn intersect(self, other: Self) -> Self {
        match (self, other) {
            (Self::None, _) | (_, Self::None) => Self::None,
            (Self::All, Self::All) => Self::All,
            _ => Self::Some,
        }
    }
    fn not(self) -> Self {
        match self {
            Self::None => Self::All,
            Self::All => Self::None,
            Self::Some => Self::Some,
        }
    }
}

#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkingCopyPatterns {
    pub rules: Vec<SparseRule>,
    pub mappings: Vec<WorkingCopyMapping>,
}

impl<'de> Deserialize<'de> for WorkingCopyPatterns {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            rules: Vec<SparseRule>,
            mappings: Vec<WorkingCopyMapping>,
        }
        let fields = Fields::deserialize(d)?;
        let value = Self {
            rules: fields.rules,
            mappings: fields.mappings,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// Compiled rules remain flat regardless of the number of selection edits.
#[derive(Debug)]
struct OrderedRulesMatcher(Vec<(bool, Box<dyn Matcher>)>);

impl Matcher for OrderedRulesMatcher {
    fn matches(&self, path: &RepoPath) -> bool {
        self.0
            .iter()
            .rev()
            .find(|(_, matcher)| matcher.matches(path))
            .is_some_and(|(include, _)| *include)
    }
    fn visit(&self, dir: &RepoPath) -> Visit {
        let mut state = Coverage::None;
        for (include, matcher) in &self.0 {
            let matched = match matcher.visit(dir) {
                Visit::Nothing => Coverage::None,
                Visit::AllRecursively => Coverage::All,
                Visit::Specific { .. } => Coverage::Some,
            };
            state = if *include {
                state.union(matched)
            } else {
                state.intersect(matched.not())
            };
        }
        match state {
            Coverage::None => Visit::Nothing,
            Coverage::All => Visit::AllRecursively,
            Coverage::Some => Visit::Specific {
                dirs: VisitDirs::All,
                files: VisitFiles::All,
            },
        }
    }
}

fn append(base: &RepoPath, suffix: &RepoPath) -> RepoPathBuf {
    if suffix.is_root() {
        return base.to_owned();
    }
    if base.is_root() {
        return suffix.to_owned();
    }
    RepoPathBuf::from_internal_string(format!(
        "{}/{}",
        base.as_internal_file_string(),
        suffix.as_internal_file_string()
    ))
    .unwrap()
}

fn translate(
    path: &RepoPath,
    source: &RepoPath,
    destination: &RepoPath,
    recursive: bool,
) -> Option<RepoPathBuf> {
    let suffix = path.strip_prefix(source)?;
    (recursive || suffix.is_root()).then(|| append(destination, suffix))
}

// Intersection of exact/prefix domains.
fn overlap<'a>(
    a: &'a RepoPath,
    ar: bool,
    b: &'a RepoPath,
    br: bool,
) -> Option<(&'a RepoPath, bool)> {
    if a == b {
        Some((a, ar && br))
    } else if ar && b.starts_with(a) {
        Some((b, br))
    } else if br && a.starts_with(b) {
        Some((a, ar))
    } else {
        None
    }
}

impl WorkingCopyPatterns {
    pub fn all() -> Self {
        Self {
            rules: vec![SparseRule {
                include: true,
                expression: SparseExpression::All,
            }],
            mappings: vec![],
        }
    }
    pub fn none() -> Self {
        Self {
            rules: vec![],
            mappings: vec![],
        }
    }
    pub fn is_identity(&self) -> bool {
        self.mappings.is_empty()
            || self
                .mappings
                .iter()
                .all(|m| &*m.source == m.destination.as_repo_path())
    }
    pub fn to_matcher(&self) -> Box<dyn Matcher> {
        let selected = OrderedRulesMatcher(
            self.rules
                .iter()
                .map(|r| (r.include, r.expression.to_matcher()))
                .collect(),
        );
        if self.mappings.is_empty() {
            return Box::new(selected);
        }
        let mut domains: Vec<Option<Box<dyn Matcher>>> = self
            .mappings
            .iter()
            .map(|m| {
                let domain: Box<dyn Matcher> = if m.recursive {
                    Box::new(PrefixMatcher::new([&m.source]))
                } else {
                    Box::new(FilesMatcher::new([&m.source]))
                };
                // A root projection cannot represent a file at its anchor.
                let domain: Box<dyn Matcher> = if m.destination.as_repo_path().is_root() {
                    Box::new(DifferenceMatcher::new(
                        domain,
                        FilesMatcher::new([&m.source]),
                    ))
                } else {
                    domain
                };
                Some(domain)
            })
            .collect();
        let domains = union_all_matchers(&mut domains);
        Box::new(IntersectionMatcher::new(selected, domains))
    }
    fn coverage(&self, path: &RepoPath, recursive: bool) -> Coverage {
        if !recursive {
            return if self
                .rules
                .iter()
                .rev()
                .find(|r| r.expression.matches(path))
                .is_some_and(|r| r.include)
            {
                Coverage::All
            } else {
                Coverage::None
            };
        }
        self.rules.iter().fold(Coverage::None, |selected, rule| {
            let matched = rule.expression.coverage(path, recursive);
            if rule.include {
                selected.union(matched)
            } else {
                selected.intersect(matched.not())
            }
        })
    }
    pub fn validate(&self) -> Result<(), WorkingCopyPatternsError> {
        for rule in &self.rules {
            rule.expression.validate(0)?;
        }
        for m in &self.mappings {
            for path in [&*m.source, m.destination.as_repo_path()] {
                path.to_fs_path(std::path::Path::new("."))
                    .map_err(|e| invalid(e.to_string()))?;
                if path
                    .components()
                    .any(|c| matches!(c.as_internal_str(), ".jj" | ".git"))
                {
                    return Err(invalid("mapping paths must not contain .jj or .git"));
                }
            }
            if !m.recursive && (m.source.is_root() || m.destination.as_repo_path().is_root()) {
                return Err(invalid("exact file mappings cannot map the root directory"));
            }
        }
        for (i, a) in self.mappings.iter().enumerate() {
            for b in &self.mappings[i + 1..] {
                if let Some((path, recursive)) =
                    overlap(&a.source, a.recursive, &b.source, b.recursive)
                {
                    if self.coverage(path, recursive) != Coverage::None {
                        return Err(invalid(
                            "mapping sources overlap on a potentially selected path",
                        ));
                    }
                }
                if let Some((path, recursive)) = overlap(
                    a.destination.as_repo_path(),
                    a.recursive,
                    b.destination.as_repo_path(),
                    b.recursive,
                ) {
                    let ap = translate(path, a.destination.as_repo_path(), &a.source, a.recursive)
                        .unwrap();
                    let bp = translate(path, b.destination.as_repo_path(), &b.source, b.recursive)
                        .unwrap();
                    if self.coverage(&ap, recursive) != Coverage::None
                        && self.coverage(&bp, recursive) != Coverage::None
                    {
                        return Err(invalid(
                            "mapping destinations overlap; exclude the overlapping source domain \
                             explicitly",
                        ));
                    }
                }
                // Destination ancestors must remain directories for every
                // future tree, not merely for the files present at validation.
                for (parent_mapping, child_mapping) in [(a, b), (b, a)] {
                    if self.coverage(&child_mapping.source, child_mapping.recursive)
                        == Coverage::None
                    {
                        continue;
                    }
                    for parent in child_mapping.destination.as_repo_path().ancestors().skip(1) {
                        if parent.is_root() {
                            continue;
                        }
                        let Some(source) = translate(
                            parent,
                            parent_mapping.destination.as_repo_path(),
                            &parent_mapping.source,
                            parent_mapping.recursive,
                        ) else {
                            continue;
                        };
                        // Canonical tree ancestry itself can establish that a
                        // file here and the child mapping cannot coexist.
                        if child_mapping.source.starts_with(&source) {
                            continue;
                        }
                        if self.coverage(&source, false) != Coverage::None {
                            return Err(invalid(format!(
                                "mapped file {} would own another mapping's parent directory; \
                                 exclude root-file:{}",
                                source.as_internal_file_string(),
                                source.as_internal_file_string(),
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }
    /// Maps a canonical domain path; selection resolves overlapping domains.
    /// Callers still apply the effective selection matcher to file entries.
    pub fn repo_to_wc(
        &self,
        path: &RepoPath,
    ) -> Result<Option<WorkingCopyPathBuf>, WorkingCopyPatternsError> {
        self.repo_to_wc_impl(path, |p| self.coverage(p, false) != Coverage::None)
    }
    pub fn repo_to_wc_with_matcher(
        &self,
        path: &RepoPath,
        matcher: &dyn Matcher,
    ) -> Result<Option<WorkingCopyPathBuf>, WorkingCopyPatternsError> {
        self.repo_to_wc_impl(path, |p| matcher.matches(p))
    }
    fn repo_to_wc_impl(
        &self,
        path: &RepoPath,
        selected: impl Fn(&RepoPath) -> bool,
    ) -> Result<Option<WorkingCopyPathBuf>, WorkingCopyPatternsError> {
        if self.mappings.is_empty() {
            return Ok(Some(WorkingCopyPathBuf(path.to_owned())));
        }
        let mut candidates = self
            .mappings
            .iter()
            .filter_map(|m| translate(path, &m.source, m.destination.as_repo_path(), m.recursive));
        let Some(first) = candidates.next() else {
            return Ok(None);
        };
        if candidates.next().is_none() {
            return Ok(Some(WorkingCopyPathBuf(first)));
        }
        if !selected(path) {
            return Ok(None);
        }
        Err(invalid("ambiguous canonical mapping ownership"))
    }
    pub fn wc_to_repo(
        &self,
        path: &WorkingCopyPathBuf,
    ) -> Result<Option<RepoPathBuf>, WorkingCopyPatternsError> {
        self.wc_to_repo_impl(path, |p| self.coverage(p, false) != Coverage::None)
    }
    pub fn wc_to_repo_with_matcher(
        &self,
        path: &WorkingCopyPathBuf,
        matcher: &dyn Matcher,
    ) -> Result<Option<RepoPathBuf>, WorkingCopyPatternsError> {
        self.wc_to_repo_impl(path, |p| matcher.matches(p))
    }
    fn wc_to_repo_impl(
        &self,
        path: &WorkingCopyPathBuf,
        is_selected: impl Fn(&RepoPath) -> bool,
    ) -> Result<Option<RepoPathBuf>, WorkingCopyPatternsError> {
        if self.mappings.is_empty() {
            return Ok(Some(path.0.clone()));
        }
        let mut candidates = self
            .mappings
            .iter()
            .filter_map(|m| {
                translate(
                    path.as_repo_path(),
                    m.destination.as_repo_path(),
                    &m.source,
                    m.recursive,
                )
            })
            .peekable();
        let Some(first) = candidates.next() else {
            return Ok(None);
        };
        if candidates.peek().is_none() {
            return Ok(Some(first));
        }
        let mut selected = std::iter::once(first)
            .chain(candidates)
            .filter(|p| is_selected(p));
        let result = selected.next();
        if selected.next().is_some() {
            return Err(invalid("ambiguous physical mapping ownership"));
        }
        Ok(result)
    }
    pub fn encode(&self) -> Vec<u8> {
        proto::Configuration {
            version: 1,
            rules: self
                .rules
                .iter()
                .map(|r| proto::Rule {
                    include: r.include,
                    expression: Some(r.expression.to_proto()),
                })
                .collect(),
            mappings: self
                .mappings
                .iter()
                .map(|m| proto::Mapping {
                    source: m.source.as_internal_file_string().to_owned(),
                    destination: m.destination.as_internal_file_string().to_owned(),
                    recursive: m.recursive,
                })
                .collect(),
        }
        .encode_to_vec()
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WorkingCopyPatternsError> {
        let p = proto::Configuration::decode(bytes).map_err(|e| invalid(e.to_string()))?;
        if p.version != 1 {
            return Err(invalid("unsupported configuration version"));
        }
        let rules = p
            .rules
            .into_iter()
            .map(|r| {
                Ok(SparseRule {
                    include: r.include,
                    expression: SparseExpression::from_proto(
                        r.expression
                            .ok_or_else(|| invalid("missing rule expression"))?,
                        0,
                    )?,
                })
            })
            .collect::<Result<_, WorkingCopyPatternsError>>()?;
        let mappings = p
            .mappings
            .into_iter()
            .map(|m| {
                Ok(WorkingCopyMapping {
                    source: RepoPathBuf::from_internal_string(m.source)
                        .map_err(|e| invalid(e.to_string()))?,
                    destination: WorkingCopyPathBuf(
                        RepoPathBuf::from_internal_string(m.destination)
                            .map_err(|e| invalid(e.to_string()))?,
                    ),
                    recursive: m.recursive,
                })
            })
            .collect::<Result<_, WorkingCopyPatternsError>>()?;
        let value = Self { rules, mappings };
        value.validate()?;
        // Reject unknown fields, duplicate scalar fields, and alternate encodings.
        // Configuration objects have one canonical byte representation.
        if value.encode() != bytes {
            return Err(invalid("noncanonical configuration encoding"));
        }
        Ok(value)
    }
    pub fn id(&self) -> WorkingCopyPatternsId {
        WorkingCopyPatternsId::new(blake2b_hash(self).to_vec())
    }
}

// Group paths and globs as in fileset compilation, keeping large unions flat
// while constructing a balanced tree for the remaining boolean expressions.
fn build_union_matcher(expressions: &[SparseExpression]) -> Box<dyn Matcher> {
    let mut file_paths = Vec::new();
    let mut prefix_paths = Vec::new();
    let mut file_globs = Vec::new();
    let mut prefix_globs = Vec::new();
    let mut matchers: Vec<Option<Box<dyn Matcher>>> = Vec::new();
    for expression in expressions {
        let matcher: Box<dyn Matcher> = match expression {
            SparseExpression::None => Box::new(NothingMatcher),
            SparseExpression::All => Box::new(EverythingMatcher),
            SparseExpression::FilePath(path) => {
                file_paths.push(path);
                continue;
            }
            SparseExpression::PrefixPath(path) => {
                prefix_paths.push(path);
                continue;
            }
            SparseExpression::FileGlob {
                dir,
                pattern,
                case_insensitive,
            } => {
                file_globs.push((
                    dir,
                    compile_glob(pattern, *case_insensitive).expect("validated sparse glob"),
                ));
                continue;
            }
            SparseExpression::PrefixGlob {
                dir,
                pattern,
                case_insensitive,
            } => {
                prefix_globs.push((
                    dir,
                    compile_glob(pattern, *case_insensitive).expect("validated sparse glob"),
                ));
                continue;
            }
            SparseExpression::UnionAll(items) => build_union_matcher(items),
            SparseExpression::Intersection(a, b) => {
                Box::new(IntersectionMatcher::new(a.to_matcher(), b.to_matcher()))
            }
            SparseExpression::Difference(a, b) => {
                Box::new(DifferenceMatcher::new(a.to_matcher(), b.to_matcher()))
            }
        };
        matchers.push(Some(matcher));
    }
    if !file_paths.is_empty() {
        matchers.push(Some(Box::new(FilesMatcher::new(file_paths))));
    }
    if !prefix_paths.is_empty() {
        matchers.push(Some(Box::new(PrefixMatcher::new(prefix_paths))));
    }
    for (globs, prefix) in [(&file_globs, false), (&prefix_globs, true)] {
        if !globs.is_empty() {
            let mut builder = GlobsMatcher::builder().prefix_paths(prefix);
            for (dir, pattern) in globs {
                builder.add(dir, pattern);
            }
            matchers.push(Some(Box::new(builder.build())));
        }
    }
    union_all_matchers(&mut matchers)
}

fn union_all_matchers(matchers: &mut [Option<Box<dyn Matcher>>]) -> Box<dyn Matcher> {
    match matchers {
        [] => Box::new(NothingMatcher),
        [matcher] => matcher.take().expect("matcher should still be available"),
        _ => {
            let (left, right) = matchers.split_at_mut(matchers.len() / 2);
            Box::new(UnionMatcher::new(
                union_all_matchers(left),
                union_all_matchers(right),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(value: &str) -> RepoPathBuf {
        RepoPathBuf::from_internal_string(value).unwrap()
    }

    fn mapping(source: &str, destination: &str) -> WorkingCopyMapping {
        WorkingCopyMapping {
            source: path(source),
            destination: WorkingCopyPathBuf::from_repo_path(path(destination)),
            recursive: true,
        }
    }

    #[test]
    fn root_projection_with_dependency_holes_owns_future_paths() {
        let mut config = WorkingCopyPatterns::all();
        config.rules.extend([
            SparseRule {
                include: false,
                expression: SparseExpression::PrefixPath(path("app/third-party/a")),
            },
            SparseRule {
                include: false,
                expression: SparseExpression::PrefixPath(path("app/third-party/b")),
            },
            SparseRule {
                include: false,
                expression: SparseExpression::FilePath(path("app/third-party")),
            },
        ]);
        config.mappings = vec![
            mapping("app", ""),
            mapping("shared/a", "third-party/a"),
            mapping("shared/b", "third-party/b"),
        ];
        config.validate().unwrap();
        let config = WorkingCopyPatterns::decode(&config.encode()).unwrap();
        for (source, destination) in [
            ("app/main.rs", "main.rs"),
            ("shared/a/future/deep.rs", "third-party/a/future/deep.rs"),
            ("shared/b/new.rs", "third-party/b/new.rs"),
        ] {
            let physical = WorkingCopyPathBuf::from_repo_path(path(destination));
            assert_eq!(
                config.repo_to_wc(&path(source)).unwrap(),
                Some(physical.clone())
            );
            assert_eq!(config.wc_to_repo(&physical).unwrap(), Some(path(source)));
        }
        assert!(
            !config
                .to_matcher()
                .matches(&path("app/third-party/a/future.rs"))
        );
        assert!(!config.to_matcher().matches(&path("elsewhere/file")));
        let mut collision = config.clone();
        collision.rules.pop();
        assert!(
            collision.validate().is_err(),
            "ancestor file could replace the physical directory"
        );
        let mut collision = config.clone();
        collision.rules.push(SparseRule {
            include: true,
            expression: SparseExpression::FilePath(path("app/third-party/a/not-yet-created")),
        });
        assert!(
            collision.validate().is_err(),
            "ownership must not depend on existing files"
        );
        let mut alias = config;
        alias.mappings.push(mapping("shared/a", "second-output"));
        assert!(
            alias.validate().is_err(),
            "one source cannot have two writable aliases"
        );
    }

    #[test]
    fn malformed_configuration_is_rejected() {
        let config = WorkingCopyPatterns::all();
        let mut bytes = config.encode();
        bytes.extend([0x78, 1]); // Unknown protobuf field.
        assert!(WorkingCopyPatterns::decode(&bytes).is_err());
        let mut proto = proto::Configuration::decode(config.encode().as_slice()).unwrap();
        proto.rules[0].expression.as_mut().unwrap().kind = 7; // Binary node without operands.
        assert!(WorkingCopyPatterns::decode(&proto.encode_to_vec()).is_err());
        proto.rules[0].expression.as_mut().unwrap().kind = 42;
        assert!(WorkingCopyPatterns::decode(&proto.encode_to_vec()).is_err());
    }

    #[test]
    fn long_ordered_rules_are_stack_safe_and_preserve_precedence() {
        let mut config = WorkingCopyPatterns {
            rules: vec![SparseRule {
                include: true,
                expression: SparseExpression::PrefixPath(path("included")),
            }],
            mappings: vec![],
        };
        for i in 0..8192 {
            config.rules.push(SparseRule {
                include: i % 2 == 1,
                expression: SparseExpression::FilePath(path("included/file")),
            });
        }
        let matcher = config.to_matcher();
        assert!(matcher.matches(&path("included/other")));
        assert!(!matcher.matches(&path("outside")));
        assert!(matcher.matches(&path("included/file")));
        config.rules.push(SparseRule {
            include: false,
            expression: SparseExpression::PrefixPath(path("included")),
        });
        let matcher = config.to_matcher();
        assert!(!matcher.matches(&path("included/file")));
        assert_eq!(matcher.visit(&path("included")), Visit::Nothing);
    }

    #[test]
    fn protobuf_depth_boundary_roundtrips() {
        let mut expression = SparseExpression::All;
        for _ in 0..MAX_DEPTH {
            expression = SparseExpression::UnionAll(vec![expression]);
        }
        let mut config = WorkingCopyPatterns {
            rules: vec![SparseRule {
                include: true,
                expression,
            }],
            mappings: vec![],
        };
        assert_eq!(
            WorkingCopyPatterns::decode(&config.encode()).unwrap(),
            config
        );
        let expression = config.rules.pop().unwrap().expression;
        config.rules.push(SparseRule {
            include: true,
            expression: SparseExpression::UnionAll(vec![expression]),
        });
        assert!(config.validate().is_err());
    }
}
