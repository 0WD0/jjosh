/// A minimal URL parser that extracts only what we need for git URLs.
/// This is a replacement for the `url` crate dependency.
#[derive(Debug)]
pub(crate) struct ParsedUrl<'a> {
    scheme: String, // Owned to allow normalization to lowercase
    username: &'a str,
    password: Option<&'a str>,
    host: Option<String>, // Owned to allow normalization to lowercase
    port: Option<u16>,
    path: &'a str,
}

/// Minimal parse error type to replace url::ParseError
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("relative URL without a base")]
    RelativeUrlWithoutBase,
    #[error("invalid port number")]
    InvalidPort,
    #[error("invalid domain character")]
    InvalidDomainCharacter,
}

/// Check if a character is valid in a URL scheme.
/// Valid scheme characters: alphanumeric, +, -, or .
fn is_valid_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.'
}

impl<'a> ParsedUrl<'a> {
    /// Parse a URL string into its components.
    /// Expected format: scheme://[user[:password]@]host[:port]/path
    pub(crate) fn parse(input: &'a str) -> Result<Self, ParseError> {
        // Find scheme by looking for first ':'
        let first_colon = input.find(':').ok_or(ParseError::RelativeUrlWithoutBase)?;

        let scheme_str = &input[..first_colon];
        // Normalize scheme to lowercase for case-insensitive matching (matches url crate behavior)
        let scheme = scheme_str.to_ascii_lowercase();

        // Verify it's followed by "//"
        if !input[first_colon..].starts_with("://") {
            return Err(ParseError::RelativeUrlWithoutBase);
        }

        // Start after "://"
        let after_scheme = &input[first_colon + 3..];

        // Check for relative URL (scheme without proper authority)
        if scheme_str.is_empty() {
            return Err(ParseError::RelativeUrlWithoutBase);
        }

        // Validate scheme characters (check original before lowercase conversion)
        if !scheme_str.chars().all(is_valid_scheme_char) {
            return Err(ParseError::RelativeUrlWithoutBase);
        }

        // Find path start (first '/' after scheme)
        let path_start = after_scheme.find('/').unwrap_or(after_scheme.len());
        let authority = &after_scheme[..path_start];
        let path = if path_start < after_scheme.len() {
            &after_scheme[path_start..]
        } else {
            // No path specified - leave empty (caller can default to / if needed)
            ""
        };

        // Parse authority: [user[:password]@]host[:port]
        let (username, password, host, port) = if let Some(at_pos) = authority.rfind('@') {
            // Has user info
            let user_info = &authority[..at_pos];
            let host_port = &authority[at_pos + 1..];

            let (user, pass) = if let Some(colon_pos) = user_info.find(':') {
                let pass_str = &user_info[colon_pos + 1..];
                // Treat empty password as None
                let pass = if pass_str.is_empty() { None } else { Some(pass_str) };
                (&user_info[..colon_pos], pass)
            } else {
                (user_info, None)
            };

            let (h, p) = Self::parse_host_port(host_port)?;
            // If we have user info, we must have a host
            if h.is_none() {
                return Err(ParseError::InvalidDomainCharacter);
            }
            (user, pass, h, p)
        } else {
            // No user info
            let (h, p) = Self::parse_host_port(authority)?;
            ("", None, h, p)
        };

        // Standard schemes (http, https, git, ssh) require a host
        // Scheme is already lowercase at this point
        let requires_host = matches!(scheme.as_str(), "http" | "https" | "git" | "ssh" | "ftp" | "ftps");
        if requires_host && host.is_none() {
            return Err(ParseError::InvalidDomainCharacter);
        }

        Ok(ParsedUrl {
            scheme,
            username,
            password,
            host,
            port,
            path,
        })
    }

    fn parse_host_port(host_port: &str) -> Result<(Option<String>, Option<u16>), ParseError> {
        if host_port.is_empty() {
            return Ok((None, None));
        }

        // Handle IPv6 addresses: [::1] or [::1]:port
        if host_port.starts_with('[') {
            if let Some(bracket_end) = host_port.find(']') {
                // IPv6 addresses are case-insensitive, normalize to lowercase
                let host = Some(host_port[..bracket_end + 1].to_ascii_lowercase());
                let remaining = &host_port[bracket_end + 1..];

                if remaining.is_empty() {
                    return Ok((host, None));
                } else if let Some(port_str) = remaining.strip_prefix(':') {
                    let port = port_str.parse::<u16>().map_err(|_| ParseError::InvalidPort)?;
                    // Validate port is in valid range (1-65535, port 0 is invalid)
                    if port == 0 {
                        return Err(ParseError::InvalidPort);
                    }
                    return Ok((host, Some(port)));
                } else {
                    return Err(ParseError::InvalidDomainCharacter);
                }
            } else {
                return Err(ParseError::InvalidDomainCharacter);
            }
        }

        // Handle regular host:port
        // Use rfind to handle IPv6 addresses without brackets (edge case)
        if let Some(colon_pos) = host_port.rfind(':') {
            // Check if this looks like a port (all digits after colon)
            let potential_port = &host_port[colon_pos + 1..];
            if potential_port.is_empty() {
                // Empty port like "host:" - strip the trailing colon
                let host_str = &host_port[..colon_pos];
                return Ok((Some(Self::normalize_hostname(host_str)?), None));
            } else if potential_port.chars().all(|c| c.is_ascii_digit()) {
                let host_str = &host_port[..colon_pos];
                let host = Self::normalize_hostname(host_str)?;
                let port = potential_port.parse::<u16>().map_err(|_| ParseError::InvalidPort)?;
                // Validate port is in valid range (1-65535, port 0 is invalid)
                if port == 0 {
                    return Err(ParseError::InvalidPort);
                }
                return Ok((Some(host), Some(port)));
            }
        }

        // No port, just host
        Ok((Some(Self::normalize_hostname(host_port)?), None))
    }

    /// Check if a string looks like a valid DNS hostname (for normalization purposes)
    /// Valid DNS names contain only alphanumeric, hyphens, dots, underscores, and wildcards
    fn is_normalizable_hostname(host: &str) -> bool {
        // Allow alphanumeric, -, ., _, and * (for patterns)
        host.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '*'))
    }

    /// Validate and possibly normalize a hostname
    /// Valid DNS hostnames are normalized to lowercase
    /// Invalid strings (like injection attempts) are preserved as-is but ? is rejected
    fn normalize_hostname(host: &str) -> Result<String, ParseError> {
        // Reject ? character which git's url parser rejects
        if host.contains('?') {
            return Err(ParseError::InvalidDomainCharacter);
        }

        // Only normalize if it looks like a valid DNS hostname
        // Preserve case for security checks if it contains special characters
        if Self::is_normalizable_hostname(host) {
            Ok(host.to_ascii_lowercase())
        } else {
            Ok(host.to_owned())
        }
    }

    pub(crate) fn scheme(&self) -> &str {
        &self.scheme
    }

    pub(crate) fn username(&self) -> &str {
        self.username
    }

    pub(crate) fn password(&self) -> Option<&str> {
        self.password
    }

    pub(crate) fn host_str(&self) -> Option<&str> {
        self.host.as_deref()
    }

    pub(crate) fn port(&self) -> Option<u16> {
        self.port
    }

    pub(crate) fn path(&self) -> &str {
        self.path
    }

    /// Check if this URL cannot be a base (is relative).
    /// For our purposes, since we only parse URLs with "://", they can all be a base.
    pub(crate) fn cannot_be_a_base(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_url() {
        let url = ParsedUrl::parse("http://example.com/path").unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("example.com"));
        assert_eq!(url.path(), "/path");
        assert_eq!(url.username(), "");
        assert_eq!(url.password(), None);
        assert_eq!(url.port(), None);
    }

    #[test]
    fn test_url_with_port() {
        let url = ParsedUrl::parse("http://example.com:8080/path").unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("example.com"));
        assert_eq!(url.port(), Some(8080));
        assert_eq!(url.path(), "/path");
    }

    #[test]
    fn test_url_with_user() {
        let url = ParsedUrl::parse("http://user@example.com/path").unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.username(), "user");
        assert_eq!(url.host_str(), Some("example.com"));
        assert_eq!(url.path(), "/path");
    }

    #[test]
    fn test_url_with_user_and_password() {
        let url = ParsedUrl::parse("http://user:pass@example.com/path").unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.username(), "user");
        assert_eq!(url.password(), Some("pass"));
        assert_eq!(url.host_str(), Some("example.com"));
        assert_eq!(url.path(), "/path");
    }

    #[test]
    fn test_url_with_ipv6() {
        let url = ParsedUrl::parse("http://[::1]/path").unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("[::1]"));
        assert_eq!(url.path(), "/path");
    }

    #[test]
    fn test_url_with_ipv6_and_port() {
        let url = ParsedUrl::parse("http://[::1]:8080/path").unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("[::1]"));
        assert_eq!(url.port(), Some(8080));
        assert_eq!(url.path(), "/path");
    }
}
