use std::cmp::Ordering;

use gix::bstr::{BStr, ByteSlice};

#[derive(Eq, PartialEq)]
enum VersionPart {
    String(String),
    Number(usize),
}

impl Ord for VersionPart {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (VersionPart::String(a), VersionPart::String(b)) => a.cmp(b),
            (VersionPart::String(_), VersionPart::Number(_)) => Ordering::Less,
            (VersionPart::Number(_), VersionPart::String(_)) => Ordering::Greater,
            (VersionPart::Number(a), VersionPart::Number(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for VersionPart {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(Ord::cmp(self, other))
    }
}

/// `Version` is used to store multi-part version numbers. It does so in a rather naive way,
/// only distinguishing between parts that can be parsed as an integer and those that cannot.
///
/// `Version` does not parse version numbers in any structure-aware way, so `v0.a` is parsed into
/// `v`, `0`, `.a`.
///
/// Comparing two `Version`s comes down to comparing their `parts`. `parts` are either compared
/// numerically or lexicographically, depending on whether they are an integer or not. That way,
/// `v0.9` sorts before `v0.10` as one would expect from a version number.
///
/// When comparing versions of different lengths, shorter versions sort before longer ones (e.g.,
/// `v1.0` < `v1.0.1`). String parts always sort before numeric parts when compared directly.
///
/// The sorting does not respect `versionsort.suffix` yet.
#[derive(Eq, PartialEq)]
struct Version {
    parts: Vec<VersionPart>,
}

impl Version {
    fn parse(version: &BStr) -> Self {
        let parts = version
            .chunk_by(|a, b| a.is_ascii_digit() == b.is_ascii_digit())
            .map(|part| {
                if let Ok(part) = part.to_str() {
                    match part.parse::<usize>() {
                        Ok(number) => VersionPart::Number(number),
                        Err(_) => VersionPart::String(part.to_string()),
                    }
                } else {
                    VersionPart::String(String::from_utf8_lossy(part).to_string())
                }
            })
            .collect();

        Self { parts }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let mut a_iter = self.parts.iter();
        let mut b_iter = other.parts.iter();

        loop {
            match (a_iter.next(), b_iter.next()) {
                (Some(a), Some(b)) => match a.cmp(b) {
                    Ordering::Equal => continue,
                    other => return other,
                },
                (Some(_), None) => return Ordering::Greater,
                (None, Some(_)) => return Ordering::Less,
                (None, None) => return Ordering::Equal,
            }
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(Ord::cmp(self, other))
    }
}

pub fn list(repo: gix::Repository, out: &mut dyn std::io::Write) -> anyhow::Result<()> {
    let platform = repo.references()?;

    let mut tags: Vec<_> = platform
        .tags()?
        .flatten()
        .map(|mut reference| {
            let tag = reference.peel_to_tag();
            let tag_ref = tag.as_ref().map(gix::Tag::decode);

            // `name` is the name of the file in `refs/tags/`.
            // This applies to both lightweight and annotated tags.
            let name = reference.name().shorten();
            let mut fields = Vec::new();
            let version = Version::parse(name);
            match tag_ref {
                Ok(Ok(tag_ref)) => {
                    // `tag_name` is the name provided by the user via `git tag -a/-s/-u`.
                    // It is only present for annotated tags.
                    fields.push(format!(
                        "tag name: {}",
                        if name == tag_ref.name { "*".into() } else { tag_ref.name }
                    ));
                    if tag_ref.pgp_signature.is_some() {
                        fields.push("signed".into());
                    }

                    (version, format!("{name} [{fields}]", fields = fields.join(", ")))
                }
                _ => (version, name.to_string()),
            }
        })
        .collect();

    tags.sort_by(|a, b| a.0.cmp(&b.0));

    for (_, tag) in tags {
        writeln!(out, "{tag}")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix::bstr::BStr;

    #[test]
    fn sorts_versions_correctly() {
        let mut actual = vec![
            "v2.0.0",
            "v1.10.0",
            "v1.2.1",
            "v1.0.0-beta",
            "v1.2",
            "v0.10.0",
            "v0.9.0",
            "v1.2.0",
            "v0.1.a",
            "v0.1.0",
            "v10.0.0",
            "1.0.0",
            "v1.0.0-alpha",
            "v1.0.0",
        ];

        actual.sort_by(|a, b| {
            let version_a = Version::parse(BStr::new(a.as_bytes()));
            let version_b = Version::parse(BStr::new(b.as_bytes()));
            version_a.cmp(&version_b)
        });

        let expected = vec![
            "v0.1.0",
            "v0.1.a",
            "v0.9.0",
            "v0.10.0",
            "v1.0.0",
            "v1.0.0-alpha",
            "v1.0.0-beta",
            "v1.2",
            "v1.2.0",
            "v1.2.1",
            "v1.10.0",
            "v2.0.0",
            "v10.0.0",
            "1.0.0",
        ];

        assert_eq!(actual, expected);
    }

    #[test]
    fn sorts_versions_with_different_lengths_correctly() {
        let v1 = Version::parse(BStr::new(b"v1.0"));
        let v2 = Version::parse(BStr::new(b"v1.0.1"));

        assert_eq!(v1.cmp(&v2), Ordering::Less);
        assert_eq!(v2.cmp(&v1), Ordering::Greater);
    }
}
