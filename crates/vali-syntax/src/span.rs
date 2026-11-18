use text_size::TextRange;

/// A small handle for a source's identity (ADR 0009), interned elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(pub u32);

/// Where something is: a source and a byte range in it (ADR 0014).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    /// The source.
    pub source: SourceId,
    /// The byte range in that source.
    pub range: TextRange,
}

/// Where a span is, as people and tools count (ADR 0014 #6): its source's
/// identity, its line from 1, its column from 0, its position from 1, and
/// its length, all counting Unicode scalar values, as Racket's
/// `syntax-line` and the rest do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// The source's identity.
    pub source: std::sync::Arc<str>,
    /// The line, from 1.
    pub line: u32,
    /// The column, from 0.
    pub column: u32,
    /// The position in the source, from 1.
    pub position: u32,
    /// How many characters it covers.
    pub span: u32,
}

/// `source:line:column`, the column from 1, as compilers print positions.
impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.source, self.line, self.column + 1)
    }
}

/// One source: its identity, its text, and where its lines start.
#[derive(Debug)]
struct Source {
    identity: std::sync::Arc<str>,
    text: std::sync::Arc<str>,
    line_starts: Vec<u32>,
}

/// The sources read so far, which spans' `SourceId`s index from 1:
/// `SourceId(0)` is no source, as for data built at run time.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    sources: Vec<std::sync::Arc<Source>>,
}

impl Sources {
    /// No sources.
    pub fn new() -> Sources {
        Sources::default()
    }

    /// Adds the text of the source `identity`, returning its id.
    pub fn add(&mut self, identity: &str, text: &str) -> SourceId {
        let line_starts = std::iter::once(0)
            .chain(
                text.match_indices('\n')
                    .map(|(at, _)| u32::try_from(at + 1).expect("a source under 4 GiB")),
            )
            .collect();
        self.sources.push(std::sync::Arc::new(Source {
            identity: identity.into(),
            text: text.into(),
            line_starts,
        }));
        SourceId(u32::try_from(self.sources.len()).expect("fewer than 2^32 sources"))
    }

    /// The identity, line (from 1) and column (from 0) of where `span`
    /// starts: what [`Sources::locate`] gives, without counting characters
    /// from the start of the source, so asking for every call in a source
    /// stays linear.
    pub fn line_column(&self, span: Span) -> Option<(std::sync::Arc<str>, u32, u32)> {
        let source = self
            .sources
            .get(usize::try_from(span.source.0).ok()?.checked_sub(1)?)?;
        let start = u32::from(span.range.start());
        let line = source.line_starts.partition_point(|&s| s <= start);
        let line_start = source.line_starts[line - 1] as usize;
        let column = source
            .text
            .get(line_start..start as usize)
            .map_or(0, |s| s.chars().count());
        Some((
            std::sync::Arc::clone(&source.identity),
            u32::try_from(line).unwrap_or(u32::MAX),
            u32::try_from(column).unwrap_or(u32::MAX),
        ))
    }

    /// Where `span` is, if its source is one of these.
    pub fn locate(&self, span: Span) -> Option<Location> {
        let source = self
            .sources
            .get(usize::try_from(span.source.0).ok()?.checked_sub(1)?)?;
        let start = u32::from(span.range.start());
        let end = u32::from(span.range.end());
        let text = &*source.text;
        let line = source.line_starts.partition_point(|&s| s <= start);
        let line_start = source.line_starts[line - 1] as usize;
        let chars = |from: usize, to: usize| {
            u32::try_from(text.get(from..to).map_or(0, |s| s.chars().count())).unwrap_or(u32::MAX)
        };
        Some(Location {
            source: std::sync::Arc::clone(&source.identity),
            line: u32::try_from(line).unwrap_or(u32::MAX),
            column: chars(line_start, start as usize),
            position: chars(0, start as usize) + 1,
            span: chars(start as usize, end as usize),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use text_size::TextSize;

    #[test]
    fn locations_count_characters() {
        let mut sources = Sources::new();
        let id = sources.add("test", "(a\n  é b)\nc");
        let at = |start: u32, end: u32| Span {
            source: id,
            range: TextRange::new(TextSize::from(start), TextSize::from(end)),
        };
        // `é` is two bytes: `b` is at byte 8, character 7.
        let b = sources.locate(at(8, 9)).unwrap();
        assert_eq!((b.line, b.column, b.position, b.span), (2, 4, 8, 1));
        assert_eq!(b.to_string(), "test:2:5");
        let c = sources.locate(at(11, 12)).unwrap();
        assert_eq!((c.line, c.column, c.position), (3, 0, 11));
        assert_eq!(
            sources.locate(Span {
                source: SourceId(0),
                ..at(0, 1)
            }),
            None
        );
    }
}
