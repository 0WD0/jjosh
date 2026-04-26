/// Parser errors for actor identities and signatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum Error {
    /// A closing `>` was not found before the end of the line.
    MissingClosingBracket,
    /// An opening `<` was not found before the closing `>`.
    MissingOpeningBracket,
    /// Duplicate delimiters overlap.
    DelimiterOverlap,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::MissingClosingBracket => "Closing '>' not found",
            Error::MissingOpeningBracket => "Opening '<' not found",
            Error::DelimiterOverlap => "Skipped parts run into each other",
        })
    }
}

impl std::error::Error for Error {}
