use std::io::Write;

use gix::{bstr::BStr, utils::AsBStr};

/// Write arbitrary bytes losslessly, quoting the entire value with its debug representation if it contains
/// control characters, quotes, backslashes, or invalid UTF-8 that must be escaped.
pub(crate) fn write_bstr(mut out: impl Write, input: &BStr, scratch: &mut Vec<u8>) -> std::io::Result<()> {
    scratch.clear();
    write!(scratch, "{input:?}").expect("writing to a byte buffer cannot fail");
    let debug_matches_input = scratch
        .strip_prefix(b"\"")
        .and_then(|debug| debug.strip_suffix(b"\""))
        .is_some_and(|debug| debug == input);
    let input = if debug_matches_input { input } else { scratch.as_bstr() };
    out.write_all(input)
}

#[cfg(test)]
mod tests {
    use gix::bstr::ByteSlice;

    use super::write_bstr;

    fn render(input: &[u8], scratch: &mut Vec<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        write_bstr(&mut out, input.as_bstr(), scratch).expect("in-memory writes succeed");
        out
    }

    #[test]
    fn safe_ascii_and_unicode_remain_unchanged() {
        let mut scratch = Vec::new();
        for input in [b"hello world".as_slice(), "hello 💡".as_bytes()] {
            assert_eq!(
                render(input, &mut scratch),
                input,
                "safe text should remain easy to read"
            );
        }
    }

    #[test]
    fn terminal_controls_and_ambiguous_bytes_are_quoted_losslessly() {
        let mut scratch = Vec::new();
        assert_eq!(
            render(b"control-\0\x08\t\n\r\x1b\x7f\"\\end", &mut scratch),
            br#""control-\0\x08\t\n\r\x1b\x7f\"\\end""#,
            "terminal-active and syntax bytes must be escaped"
        );
        assert_eq!(
            render(b"invalid-\xff", &mut scratch),
            br#""invalid-\xff""#,
            "invalid UTF-8 must remain recoverable"
        );
    }
}
