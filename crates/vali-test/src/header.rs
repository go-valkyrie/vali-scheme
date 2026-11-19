//! The metadata header at the top of each test file.

/// What a test file declares about itself, in `;;; key: value` comment lines
/// at its very top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// The requirement it checks, as worded in the requirements table.
    pub requirement: String,
    /// Where the problem was found, if it came from evaluating another
    /// implementation.
    pub found_in: Option<String>,
    /// What kind of test it is.
    pub kind: Kind,
    /// For [`Kind::Host`], the host fixture it needs.
    pub host: Option<String>,
    /// For [`Kind::Host`], the capabilities the host grants the test, which
    /// it has none of unless it asks: [`Grant::Clock`], and [`Grant::Read`]
    /// for files below the test's own directory.
    pub grants: Vec<Grant>,
    /// If the test is expected to fail, why.
    pub expect_fail: Option<String>,
}

/// The kinds of test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Plain Scheme, asserting with SRFI 64.
    Scheme,
    /// Needs a host set up a particular way, named by `host:`.
    Host,
    /// A scaling check: the file's work must grow no faster than allowed.
    Performance,
    /// Tracked over time rather than passed or failed.
    Benchmark,
}

/// A capability a host test can ask for with `grant:` (ADR 0009).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grant {
    /// The clock, serving `(scheme time)`.
    Clock,
    /// Reading files, and asking whether they exist, below the test's own
    /// directory, serving `(scheme file)`.
    Read,
}

/// A problem with a header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderError {
    /// The 1-based line it is on.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "header line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for HeaderError {}

/// Parses the header at the top of `source`: every leading line starting
/// with `;;;`.
///
/// Keys are `requirement` and `kind` (both required), `found-in`, `host`
/// (required for, and only allowed with, `kind: host`), `grant` (only with
/// `kind: host`: `clock`, `read`, or both, comma separated) and `expect`, whose
/// value is `fail` or `fail, <reason>`. Unknown or repeated keys are errors,
/// so a typo can't silently drop an `expect` line.
pub fn parse_header(source: &str) -> Result<Header, HeaderError> {
    let mut requirement = None;
    let mut found_in = None;
    let mut kind = None;
    let mut host = None;
    let mut grant = None;
    let mut expect_fail = None;
    let mut last_line = 0;

    for (index, line) in source.lines().enumerate() {
        let Some(rest) = line.strip_prefix(";;;") else {
            break;
        };
        let number = index + 1;
        last_line = number;
        let error = |message: String| HeaderError {
            line: number,
            message,
        };
        let (key, value) = rest
            .split_once(':')
            .ok_or_else(|| error("expected `;;; key: value`".to_owned()))?;
        let (key, value) = (key.trim(), value.trim().to_owned());
        let slot = match key {
            "requirement" => &mut requirement,
            "found-in" => &mut found_in,
            "kind" => &mut kind,
            "host" => &mut host,
            "grant" => &mut grant,
            "expect" => &mut expect_fail,
            _ => return Err(error(format!("unknown key `{key}`"))),
        };
        if slot.is_some() {
            return Err(error(format!("`{key}` appears twice")));
        }
        if value.is_empty() {
            return Err(error(format!("`{key}` has no value")));
        }
        *slot = Some(value);
    }

    let missing = |key: &str| HeaderError {
        line: last_line + 1,
        message: format!("missing `{key}`"),
    };
    let requirement = requirement.ok_or_else(|| missing("requirement"))?;
    let kind = match kind.ok_or_else(|| missing("kind"))?.as_str() {
        "scheme" => Kind::Scheme,
        "host" => Kind::Host,
        "performance" => Kind::Performance,
        "benchmark" => Kind::Benchmark,
        other => {
            return Err(HeaderError {
                line: last_line,
                message: format!(
                    "unknown kind `{other}`; use scheme, host, performance or benchmark"
                ),
            });
        }
    };
    if (kind == Kind::Host) != host.is_some() {
        return Err(HeaderError {
            line: last_line,
            message: "`host` is required for, and only allowed with, `kind: host`".to_owned(),
        });
    }
    let mut grants = Vec::new();
    if let Some(list) = grant {
        if kind != Kind::Host {
            return Err(HeaderError {
                line: last_line,
                message: "`grant` is only allowed with `kind: host`".to_owned(),
            });
        }
        for name in list.split(',').map(str::trim) {
            grants.push(match name {
                "clock" => Grant::Clock,
                "read" => Grant::Read,
                other => {
                    return Err(HeaderError {
                        line: last_line,
                        message: format!("unknown grant `{other}`; use clock or read"),
                    });
                }
            });
        }
    }
    let expect_fail = match expect_fail {
        None => None,
        Some(value) => match value.strip_prefix("fail") {
            Some("") => Some(String::new()),
            Some(rest) if rest.starts_with(',') => Some(rest[1..].trim().to_owned()),
            _ => {
                return Err(HeaderError {
                    line: last_line,
                    message: "`expect` must be `fail` or `fail, <reason>`".to_owned(),
                });
            }
        },
    };

    Ok(Header {
        requirement,
        found_in,
        kind,
        host,
        grants,
        expect_fail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_complete_header() {
        let header = parse_header(
            ";;; requirement: Pair fields need no locks\n\
             ;;; found-in: scheme-rs\n\
             ;;; kind: benchmark\n\
             ;;; expect: fail, until M1\n\
             (display 1)\n",
        )
        .unwrap();
        assert_eq!(header.requirement, "Pair fields need no locks");
        assert_eq!(header.found_in.as_deref(), Some("scheme-rs"));
        assert_eq!(header.kind, Kind::Benchmark);
        assert_eq!(header.expect_fail.as_deref(), Some("until M1"));
    }

    #[test]
    fn grants() {
        let header = parse_header(
            ";;; requirement: r\n;;; kind: host\n;;; host: h\n;;; grant: clock, read\n",
        )
        .unwrap();
        assert_eq!(header.grants, [Grant::Clock, Grant::Read]);
    }

    #[test]
    fn the_header_ends_at_the_first_other_line() {
        let header =
            parse_header(";;; requirement: r\n;;; kind: scheme\n\n;;; expect: fail\n").unwrap();
        assert_eq!(header.expect_fail, None);
    }

    #[test]
    fn mistakes_are_errors() {
        let err = |source: &str| parse_header(source).unwrap_err().message;
        assert_eq!(err(";;; kind: scheme\n"), "missing `requirement`");
        assert_eq!(err(";;; requirement: r\n"), "missing `kind`");
        assert_eq!(
            err(";;; requirement: r\n;;; kidn: scheme\n"),
            "unknown key `kidn`"
        );
        assert_eq!(
            err(";;; requirement: r\n;;; requirement: s\n"),
            "`requirement` appears twice"
        );
        assert!(err(";;; requirement: r\n;;; kind: host\n").contains("`host`"));
        assert!(err(";;; requirement: r\n;;; kind: scheme\n;;; host: x\n").contains("`host`"));
        assert!(
            err(";;; requirement: r\n;;; kind: scheme\n;;; expect: pass\n").contains("`expect`")
        );
        assert!(err(";;; requirement: r\n;;; kind: quick\n").contains("unknown kind"));
        assert_eq!(err(";;; nothing here\n"), "expected `;;; key: value`");
        assert!(
            err(";;; requirement: r\n;;; kind: scheme\n;;; grant: clock\n").contains("`grant`")
        );
        assert!(
            err(";;; requirement: r\n;;; kind: host\n;;; host: h\n;;; grant: write\n")
                .contains("unknown grant")
        );
    }
}
