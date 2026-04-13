use std::ops::Deref;

use winnow::{
    combinator::{eof, separated_pair, terminated},
    error::ParserError,
    prelude::*,
    token::{rest, take_until},
};

use crate::{
    bstr::{BStr, ByteSlice},
    commit::message::BodyRef,
};

/// An iterator over trailers as parsed from a commit message body.
///
/// lines with parsing failures will be skipped
pub struct Trailers<'a> {
    pub(crate) cursor: &'a [u8],
}

/// A trailer as parsed from the commit message body.
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TrailerRef<'a> {
    /// The name of the trailer, like "Signed-off-by", up to the separator `: `.
    #[cfg_attr(feature = "serde", serde(borrow))]
    pub token: &'a BStr,
    /// The value right after the separator `: `, with leading and trailing whitespace trimmed.
    /// Note that multi-line values aren't currently supported.
    pub value: &'a BStr,
}

fn parse_single_line_trailer<'a, E: ParserError<&'a [u8]>>(i: &mut &'a [u8]) -> ModalResult<(&'a BStr, &'a BStr), E> {
    *i = i.trim_end();
    let (token, value) = separated_pair(take_until(1.., b":".as_ref()), b": ", rest).parse_next(i)?;

    if token.trim_end().len() != token.len() || value.trim_start().len() != value.len() {
        Err(winnow::error::ErrMode::from_input(i).cut())
    } else {
        Ok((token.as_bstr(), value.as_bstr()))
    }
}

impl<'a> Iterator for Trailers<'a> {
    type Item = TrailerRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor.is_empty() {
            return None;
        }
        for mut line in self.cursor.lines_with_terminator() {
            self.cursor = &self.cursor[line.len()..];
            if let Some(trailer) = terminated(parse_single_line_trailer::<()>, eof)
                .parse_next(&mut line)
                .ok()
                .map(|(token, value)| TrailerRef {
                    token: token.trim().as_bstr(),
                    value: value.trim().as_bstr(),
                })
            {
                return Some(trailer);
            }
        }
        None
    }
}

impl<'a> BodyRef<'a> {
    /// Parse `body` bytes into the trailer and the actual body.
    pub fn from_bytes(body: &'a [u8]) -> Self {
        body.rfind(b"\n\n")
            .map(|pos| (2, pos))
            .or_else(|| body.rfind(b"\r\n\r\n").map(|pos| (4, pos)))
            .and_then(|(sep_len, pos)| {
                let trailer = &body[pos + sep_len..];
                let body = &body[..pos];
                Trailers { cursor: trailer }.next().map(|_| BodyRef {
                    body_without_trailer: body.as_bstr(),
                    start_of_trailer: trailer,
                })
            })
            .unwrap_or_else(|| {
                // No blank-line separator found within the body. This happens when
                // the commit message body consists entirely of trailers with no
                // preceding body text, e.g.:
                //
                //   Fix the thing\n\nSigned-off-by: Alice <alice@example.com>
                //
                // `MessageRef::from_bytes` already consumed the first `\n\n` when
                // splitting the title from the body, so the body bytes here are
                // just `"Signed-off-by: …"` with no further `\n\n`. Git itself
                // recognises this as a valid trailer block; we match that behaviour
                // by treating the entire body as the trailer block when *every*
                // non-empty line is a valid trailer.
                let all_trailers = {
                    let mut found_any = false;
                    let mut ok = true;
                    for line in body.lines() {
                        if line.is_empty() {
                            continue;
                        }
                        let mut probe = line;
                        if terminated(parse_single_line_trailer::<()>, eof)
                            .parse_next(&mut probe)
                            .is_ok()
                        {
                            found_any = true;
                        } else {
                            ok = false;
                            break;
                        }
                    }
                    found_any && ok
                };
                if all_trailers {
                    BodyRef {
                        body_without_trailer: b"".as_bstr(),
                        start_of_trailer: body,
                    }
                } else {
                    BodyRef {
                        body_without_trailer: body.as_bstr(),
                        start_of_trailer: &[],
                    }
                }
            })
    }

    /// Returns the body with the trailers stripped.
    ///
    /// You can iterate trailers with the [`trailers()`][BodyRef::trailers()] method.
    pub fn without_trailer(&self) -> &'a BStr {
        self.body_without_trailer
    }

    /// Return an iterator over the trailers parsed from the last paragraph of the body. Maybe empty.
    pub fn trailers(&self) -> Trailers<'a> {
        Trailers {
            cursor: self.start_of_trailer,
        }
    }
}

impl AsRef<BStr> for BodyRef<'_> {
    fn as_ref(&self) -> &BStr {
        self.body_without_trailer
    }
}

impl Deref for BodyRef<'_> {
    type Target = BStr;

    fn deref(&self) -> &Self::Target {
        self.body_without_trailer
    }
}

/// Convenience methods
impl TrailerRef<'_> {
    /// Check if this trailer is a `Signed-off-by` trailer (case-insensitive).
    pub fn is_signed_off_by(&self) -> bool {
        self.token.eq_ignore_ascii_case(b"Signed-off-by")
    }

    /// Check if this trailer is a `Co-authored-by` trailer (case-insensitive).
    pub fn is_co_authored_by(&self) -> bool {
        self.token.eq_ignore_ascii_case(b"Co-authored-by")
    }

    /// Check if this trailer is an `Acked-by` trailer (case-insensitive).
    pub fn is_acked_by(&self) -> bool {
        self.token.eq_ignore_ascii_case(b"Acked-by")
    }

    /// Check if this trailer is a `Reviewed-by` trailer (case-insensitive).
    pub fn is_reviewed_by(&self) -> bool {
        self.token.eq_ignore_ascii_case(b"Reviewed-by")
    }

    /// Check if this trailer is a `Tested-by` trailer (case-insensitive).
    pub fn is_tested_by(&self) -> bool {
        self.token.eq_ignore_ascii_case(b"Tested-by")
    }

    /// Check if this trailer represents any kind of authorship or attribution
    /// (`Signed-off-by`, `Co-authored-by`, etc.).
    pub fn is_attribution(&self) -> bool {
        self.is_signed_off_by()
            || self.is_co_authored_by()
            || self.is_acked_by()
            || self.is_reviewed_by()
            || self.is_tested_by()
    }
}

/// Convenience methods
impl<'a> Trailers<'a> {
    /// Filter trailers to only include `Signed-off-by` entries.
    pub fn signed_off_by(self) -> impl Iterator<Item = TrailerRef<'a>> {
        self.filter(TrailerRef::is_signed_off_by)
    }

    /// Filter trailers to only include `Co-authored-by` entries.
    pub fn co_authored_by(self) -> impl Iterator<Item = TrailerRef<'a>> {
        self.filter(TrailerRef::is_co_authored_by)
    }

    /// Filter trailers to only include attribution-related entries.
    /// (`Signed-off-by`, `Co-authored-by`, `Acked-by`, `Reviewed-by`, `Tested-by`).
    pub fn attributions(self) -> impl Iterator<Item = TrailerRef<'a>> {
        self.filter(TrailerRef::is_attribution)
    }

    /// Filter trailers to only include authors from `Signed-off-by` and `Co-authored-by` entries.
    pub fn authors(self) -> impl Iterator<Item = TrailerRef<'a>> {
        self.filter(|trailer| trailer.is_signed_off_by() || trailer.is_co_authored_by())
    }
}

#[cfg(test)]
mod test_parse_trailer {
    use super::*;

    fn parse(input: &str) -> (&BStr, &BStr) {
        parse_single_line_trailer::<()>.parse_peek(input.as_bytes()).unwrap().1
    }

    #[test]
    fn simple_newline() {
        assert_eq!(parse("foo: bar\n"), ("foo".into(), "bar".into()));
    }

    #[test]
    fn simple_non_ascii_no_newline() {
        assert_eq!(parse("🤗: 🎉"), ("🤗".into(), "🎉".into()));
    }

    #[test]
    fn with_lots_of_whitespace_newline() {
        assert_eq!(
            parse("hello foo: bar there   \n"),
            ("hello foo".into(), "bar there".into())
        );
    }

    #[test]
    fn extra_whitespace_before_token_or_value_is_error() {
        assert!(parse_single_line_trailer::<()>.parse_peek(b"foo : bar").is_err());
        assert!(parse_single_line_trailer::<()>.parse_peek(b"foo:  bar").is_err());
    }

    #[test]
    fn simple_newline_windows() {
        assert_eq!(parse("foo: bar\r\n"), ("foo".into(), "bar".into()));
    }
}
