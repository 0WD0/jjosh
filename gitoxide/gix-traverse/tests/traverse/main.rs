mod util;
pub use util::{Result, hex_to_id};

mod commit;
mod tree;

/// Normalize debug-formatted `value` so one snapshot can be reused for SHA-1 and SHA-256 fixtures,
/// as elaborate find & replace, returning the stringified and SHA-1-replaced result.
///
/// The helper rewrites `Sha256(<hex>)` occurrences to their corresponding `Sha1(<hex>)` hashes
/// based on the mapping defined in `SHA1_TO_SHA256_HASHES` while leaving the surrounding
/// pretty-debug formatting untouched.
fn normalize_debug_snapshot(value: &dyn std::fmt::Debug) -> String {
    let input = format!("{value:#?}");
    let mut out = String::with_capacity(input.len());
    let mut cursor = input.as_str();

    while !cursor.is_empty() {
        let (prefix_len, id_start) = if cursor.starts_with("Sha256(") {
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

        let sha1_id = util::SHA1_TO_SHA256_HASHES
            .iter()
            .find(|(_k, v)| **v == oid)
            // We don't panic here, expecting that the result of `normalize_debug_snapshot` will be
            // used in a diff, giving us the opportunity to add the missing pair to the mapping.
            .map_or("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", |v| v.0);

        out.push_str("Sha1(");
        out.push_str(sha1_id);
        out.push(')');
        cursor = &cursor[id_end + 1..];
    }
    out
}
