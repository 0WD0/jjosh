use gix_object::bstr::{BStr, ByteSlice};

type ParseResult<T> = Result<T, ()>;

fn is_hex_digit(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

/// Copy from `gix-object`, intentionally accepting all supported hash lengths.
pub fn hex_hash<'a>(i: &mut &'a [u8]) -> ParseResult<&'a BStr> {
    let max = gix_hash::Kind::longest().len_in_hex();
    let len = i.iter().take(max).take_while(|b| is_hex_digit(**b)).count();
    if len < gix_hash::Kind::shortest().len_in_hex() {
        return Err(());
    }
    let (hex, rest) = i.split_at(len);
    *i = rest;
    Ok(hex.as_bstr())
}

/// Parse CRLF or LF, independently of the platform.
pub fn newline<'a>(i: &mut &'a [u8]) -> ParseResult<&'a [u8]> {
    if let Some(rest) = i.strip_prefix(b"\r\n") {
        let out = &i[..2];
        *i = rest;
        Ok(out)
    } else if let Some(rest) = i.strip_prefix(b"\n") {
        let out = &i[..1];
        *i = rest;
        Ok(out)
    } else {
        Err(())
    }
}
