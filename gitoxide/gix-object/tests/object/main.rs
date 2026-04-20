use std::{collections::HashMap, path::PathBuf, sync::atomic::AtomicBool};

use gix_hash::ObjectId;
use gix_object::WriteTo;

mod commit;
mod encode;
mod object_ref;
mod tag;
mod tree;

#[test]
fn compute_hash() {
    for hk in gix_hash::Kind::all() {
        assert_eq!(
            gix_object::compute_hash(*hk, gix_object::Kind::Blob, &[]).expect("empty hash doesn’t collide"),
            gix_hash::ObjectId::empty_blob(*hk)
        );
        assert_eq!(
            gix_object::compute_hash(*hk, gix_object::Kind::Tree, &[]).expect("empty hash doesn’t collide"),
            gix_hash::ObjectId::empty_tree(*hk)
        );
    }
}

#[test]
fn compute_stream_hash() {
    for hk in gix_hash::Kind::all() {
        assert_eq!(
            gix_object::compute_stream_hash(
                *hk,
                gix_object::Kind::Blob,
                &mut &[][..],
                0,
                &mut gix_features::progress::Discard,
                &AtomicBool::default()
            )
            .expect("in-memory works"),
            gix_hash::ObjectId::empty_blob(*hk)
        );
        assert_eq!(
            gix_object::compute_stream_hash(
                *hk,
                gix_object::Kind::Tree,
                &mut &[][..],
                0,
                &mut gix_features::progress::Discard,
                &AtomicBool::default()
            )
            .expect("in-memory works"),
            gix_hash::ObjectId::empty_tree(*hk)
        );
    }
}

use gix_testtools::Result;

#[cfg(not(windows))]
fn fixup(v: Vec<u8>) -> Vec<u8> {
    v
}

#[cfg(windows)]
fn fixup(v: Vec<u8>) -> Vec<u8> {
    // Git checks out text files with line ending conversions, git itself will of course not put '\r\n' anywhere,
    // so that wouldn't be expected in an object and doesn't have to be parsed.
    use bstr::ByteSlice;
    v.replace(b"\r\n", "\n")
}

pub fn fixture(path: &str) -> PathBuf {
    PathBuf::from("tests/fixtures").join(path)
}

pub fn fixture_hash_kind() -> gix_hash::Kind {
    gix_testtools::hash_kind_from_env().unwrap_or_default()
}

fn fixture_bytes(path: &str) -> Vec<u8> {
    fixup(std::fs::read(fixture(path)).unwrap())
}

fn fixture_name(kind: &str, path: &str) -> Vec<u8> {
    fixup(fixture_bytes(PathBuf::from(kind).join(path).to_str().unwrap()))
}

/// Return the object id expected in fixture assertions for the active fixture hash kind.
///
/// Tree fixtures in this test module are authored as SHA-1 data and are rewritten on demand for
/// SHA-256 runs. This helper mirrors that rewrite on the expectation side so tree parsing tests can
/// use one set of source ids for both hash kinds.
pub fn fixture_oid(hex: &str) -> ObjectId {
    let oid = hex_to_id(hex);
    match fixture_hash_kind() {
        gix_hash::Kind::Sha1 => oid,
        kind => {
            let mut hasher = gix_hash::hasher(kind);
            hasher.update(oid.as_bytes());
            hasher.try_finalize().expect("sha256 hashing is available for tests")
        }
    }
}

/// Load a tree fixture and, if needed, rewrite its embedded entry ids for the active fixture hash kind.
///
/// The on-disk `tree/*.tree` fixtures contain SHA-1-sized ids. For SHA-256 test runs we parse the
/// SHA-1 fixture, rewrite each entry id into the synthetic SHA-256 ids produced by [`fixture_oid()`],
/// and re-encode the tree so parsers see correctly-sized object ids.
pub fn tree_fixture(path: &str) -> Result<Vec<u8>> {
    let fixture = fixture_name("tree", path);
    match fixture_hash_kind() {
        gix_hash::Kind::Sha1 => Ok(fixture),
        kind => {
            let mut tree: gix_object::Tree = gix_object::TreeRef::from_bytes(&fixture, gix_hash::Kind::Sha1)?.into();
            for entry in &mut tree.entries {
                let mut hasher = gix_hash::hasher(kind);
                hasher.update(entry.oid.as_bytes());
                entry.oid = hasher.try_finalize()?;
            }
            let mut out = Vec::with_capacity(
                fixture.len() + tree.entries.len() * (kind.len_in_bytes() - gix_hash::Kind::Sha1.len_in_bytes()),
            );
            tree.write_to(&mut out)?;
            Ok(out)
        }
    }
}

pub fn generated_tree_root_id() -> Result<ObjectId> {
    let root = gix_testtools::scripted_fixture_read_only("make_trees.sh")?;
    let tree = std::fs::read(root.join("tree.baseline"))?;
    Ok(gix_object::compute_hash(
        fixture_hash_kind(),
        gix_object::Kind::Tree,
        &tree,
    )?)
}

/// Normalize rendered tree snapshots so one inline `insta` expectation can be reused for
/// SHA-1 and SHA-256 fixtures.
///
/// Every object id whose hex length matches one of [`gix_hash::Kind::all()`] is rewritten to a
/// stable `Oid(<n>)` placeholder in first-seen order while the tree rendering itself remains
/// unchanged.
pub fn normalize_tree_snapshot(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut seen = HashMap::<&str, usize>::new();
    let mut next_id = 1usize;
    let mut cursor = input;

    while !cursor.is_empty() {
        let hex_len = cursor.bytes().take_while(u8::is_ascii_hexdigit).count();
        if hex_len >= 40 && gix_hash::Kind::all().iter().any(|kind| kind.len_in_hex() == hex_len) {
            let oid = &cursor[..hex_len];
            let normalized = *seen.entry(oid).or_insert_with(|| {
                let current = next_id;
                next_id += 1;
                current
            });
            out.push_str("Oid(");
            out.push_str(&normalized.to_string());
            out.push(')');
            cursor = &cursor[hex_len..];
            continue;
        }

        let ch = cursor.chars().next().expect("not empty");
        out.push(ch);
        cursor = &cursor[ch.len_utf8()..];
    }
    out
}

#[test]
fn size_in_memory() {
    let actual = std::mem::size_of::<gix_object::Object>();
    let sha1 = 272;
    let sha256_extra = 16;
    let expected = sha1 + sha256_extra;
    assert!(
        actual <= expected,
        "{actual} <= {expected}: Prevent unexpected growth of what should be lightweight objects"
    );
}

fn hex_to_id(hex: &str) -> ObjectId {
    ObjectId::from_hex(hex.as_bytes()).expect("40 bytes hex")
}

fn signature(time: &str) -> gix_actor::SignatureRef<'_> {
    use gix_object::bstr::ByteSlice;
    gix_actor::SignatureRef {
        name: b"Sebastian Thiel".as_bstr(),
        email: b"sebastian.thiel@icloud.com".as_bstr(),
        time,
    }
}

fn linus_signature(time: &str) -> gix_actor::SignatureRef<'_> {
    use gix_object::bstr::ByteSlice;
    gix_actor::SignatureRef {
        name: b"Linus Torvalds".as_bstr(),
        email: b"torvalds@linux-foundation.org".as_bstr(),
        time,
    }
}
