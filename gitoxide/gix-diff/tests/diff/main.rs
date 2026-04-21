use gix_testtools::Result;
use std::collections::HashMap;

fn hex_to_id(hex: &str) -> gix_hash::ObjectId {
    gix_hash::ObjectId::from_hex(hex.as_bytes()).expect("valid hex id")
}

fn fixture_hash_kind() -> gix_hash::Kind {
    gix_testtools::hash_kind_from_env().unwrap_or_default()
}

fn open_odb(objects_dir: impl Into<std::path::PathBuf>) -> std::io::Result<gix_odb::Handle> {
    gix_odb::at_opts(
        objects_dir,
        Vec::new(),
        gix_odb::store::init::Options {
            object_hash: fixture_hash_kind(),
            ..Default::default()
        },
    )
}

/// Normalize debug-formatted `value` so one snapshot can be reused for
/// SHA-1 and SHA-256 fixtures, as elaborate find & replace, returning the
/// stringified and Oid-replaced result.
///
/// The helper rewrites `Sha1(<hex>)` and `Sha256(<hex>)` occurrences to stable
/// `Oid(<n>)` placeholders in first-seen order while leaving the surrounding
/// pretty-debug formatting untouched.
fn normalize_debug_snapshot(value: &dyn std::fmt::Debug) -> String {
    let input = format!("{value:#?}");
    let mut out = String::with_capacity(input.len());
    let mut seen = HashMap::<&str, usize>::new();
    let mut next_id = 1usize;
    let mut cursor = input.as_str();

    while !cursor.is_empty() {
        let (prefix_len, id_start) = if cursor.starts_with("Sha1(") {
            (5usize, 5usize)
        } else if cursor.starts_with("Sha256(") {
            (7usize, 7usize)
        } else {
            let ch = cursor.chars().next().expect("not empty");
            out.push(ch);
            cursor = &cursor[ch.len_utf8()..];
            continue;
        };

        let Some(id_end) = cursor[id_start..].find(')') else {
            out.push_str(&cursor[..prefix_len]);
            cursor = &cursor[prefix_len..];
            continue;
        };
        let id_end = id_start + id_end;
        let oid = &cursor[id_start..id_end];
        if !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
            out.push_str(&cursor[..prefix_len]);
            cursor = &cursor[prefix_len..];
            continue;
        }

        let normalized = *seen.entry(oid).or_insert_with(|| {
            let current = next_id;
            next_id += 1;
            current
        });
        out.push_str("Oid(");
        out.push_str(&normalized.to_string());
        out.push(')');
        cursor = &cursor[id_end + 1..];
    }
    out
}

fn normalize_patch_snapshot(input: &str) -> String {
    fn normalize_hex_token<'a>(token: &'a str, seen: &mut HashMap<&'a str, usize>, next_id: &mut usize) -> String {
        if token == "0000000" || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return token.to_owned();
        }
        let normalized = *seen.entry(token).or_insert_with(|| {
            let current = *next_id;
            *next_id += 1;
            current
        });
        format!("Oid({normalized})")
    }

    let mut seen = HashMap::<&str, usize>::new();
    let mut next_id = 1usize;

    input
        .lines()
        .map(|line| {
            if let Some(commit) = line.strip_prefix("commit ") {
                return format!("commit {}", normalize_hex_token(commit, &mut seen, &mut next_id));
            }

            if let Some(index) = line.strip_prefix("index ") {
                let (range, mode) = index
                    .split_once(' ')
                    .map_or((index, None), |(range, mode)| (range, Some(mode)));
                if let Some((lhs, rhs)) = range.split_once("..") {
                    let lhs = normalize_hex_token(lhs, &mut seen, &mut next_id);
                    let rhs = normalize_hex_token(rhs, &mut seen, &mut next_id);
                    return match mode {
                        Some(mode) => format!("index {lhs}..{rhs} {mode}"),
                        None => format!("index {lhs}..{rhs}"),
                    };
                }
            }

            line.to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn assert_hash_agnostic_patch_eq(actual: &str, expected: &str) {
    pretty_assertions::assert_eq!(normalize_patch_snapshot(expected), normalize_patch_snapshot(actual));
}

macro_rules! assert_hash_agnostic_eq {
    ($left:expr, $right:expr $(, $($arg:tt)+)?) => {{
        pretty_assertions::assert_eq!(
            crate::normalize_debug_snapshot(&($left)),
            crate::normalize_debug_snapshot(&($right))
            $(, $($arg)+)?
        );
    }};
}

mod blob;
mod index;
mod rewrites;
mod tree;
mod tree_with_rewrites;

mod util {
    use std::collections::HashMap;

    use gix_hash::oid;
    use gix_object::{bstr::BString, find::Error};

    #[derive(Default)]
    pub struct ObjectDb {
        data_by_id: HashMap<gix_hash::ObjectId, BString>,
    }

    impl gix_object::FindHeader for ObjectDb {
        fn try_header(&self, id: &oid) -> Result<Option<gix_object::Header>, Error> {
            match self.data_by_id.get(&id.to_owned()) {
                Some(data) => Ok(Some(gix_object::Header {
                    kind: gix_object::Kind::Blob,
                    size: data.len() as u64,
                })),
                None => Ok(None),
            }
        }
    }

    impl gix_object::Find for ObjectDb {
        fn try_find<'a>(&self, id: &oid, buffer: &'a mut Vec<u8>) -> Result<Option<gix_object::Data<'a>>, Error> {
            match self.data_by_id.get(&id.to_owned()) {
                Some(data) => {
                    buffer.clear();
                    buffer.extend_from_slice(data);
                    Ok(Some(gix_object::Data {
                        kind: gix_object::Kind::Blob,
                        hash_kind: id.kind(),
                        data: buffer.as_slice(),
                    }))
                }
                None => Ok(None),
            }
        }
    }

    impl ObjectDb {
        /// Insert `data` and return its hash. That can be used to find it again.
        pub fn insert(&mut self, data: &str) -> Result<gix_hash::ObjectId, Error> {
            let id = gix_object::compute_hash(
                gix_testtools::hash_kind_from_env().unwrap_or_default(),
                gix_object::Kind::Blob,
                data.as_bytes(),
            )?;
            self.data_by_id.insert(id, data.into());
            Ok(id)
        }
    }
}
