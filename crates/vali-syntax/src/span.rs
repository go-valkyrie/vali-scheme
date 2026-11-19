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

/// One source: its identity, its text, where its lines start, and where
/// its characters longer than one byte start.
#[derive(Debug)]
struct Source {
    identity: std::sync::Arc<str>,
    text: std::sync::Arc<str>,
    line_starts: Vec<u32>,
    /// Each character longer than one byte: where it starts, and how many
    /// bytes beyond one it and those before it take. Counting the
    /// characters between two offsets is then two searches, however long
    /// the line: one line of generated code can hold a whole program.
    wide: Vec<(u32, u32)>,
}

impl Source {
    /// How many characters there are from byte `from` to byte `to`: 0
    /// unless both are character boundaries, in order.
    fn chars(&self, from: u32, to: u32) -> u32 {
        if self.text.get(from as usize..to as usize).is_none() {
            return 0;
        }
        let extra = |at: u32| match self.wide.partition_point(|&(start, _)| start < at) {
            0 => 0,
            n => self.wide[n - 1].1,
        };
        (to - from) - (extra(to) - extra(from))
    }
}

/// The sources read so far, which spans' `SourceId`s index from 1:
/// `SourceId(0)` is no source, as for data built at run time.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    sources: Vec<std::sync::Arc<Source>>,
    /// Where each identity and text pair first is in `sources`, so finding
    /// a source does not scan the others.
    index: std::collections::HashMap<(std::sync::Arc<str>, std::sync::Arc<str>), usize>,
}

impl Sources {
    /// No sources.
    pub fn new() -> Sources {
        Sources::default()
    }

    /// Adds every source of `other` that this does not already hold, the
    /// same identity with the same text, and returns where each of
    /// `other`'s ids is now.
    pub fn absorb(&mut self, other: &Sources) -> std::collections::HashMap<SourceId, SourceId> {
        let id = |i: usize| SourceId(u32::try_from(i + 1).expect("fewer than 2^32 sources"));
        let mut map = std::collections::HashMap::new();
        for (i, source) in other.sources.iter().enumerate() {
            let key = (
                std::sync::Arc::clone(&source.identity),
                std::sync::Arc::clone(&source.text),
            );
            let at = match self.index.get(&key) {
                Some(&at) => at,
                None => {
                    self.sources.push(std::sync::Arc::clone(source));
                    self.index.insert(key, self.sources.len() - 1);
                    self.sources.len() - 1
                }
            };
            map.insert(id(i), id(at));
        }
        map
    }

    /// The identity and text of the source `id`, if there is one.
    pub fn source(&self, id: SourceId) -> Option<(&str, &str)> {
        let source = self
            .sources
            .get(usize::try_from(id.0).ok()?.checked_sub(1)?)?;
        Some((&source.identity, &source.text))
    }

    /// The id of the source `identity` with `text`, adding it unless it is
    /// held already.
    pub fn intern(&mut self, identity: &str, text: &str) -> SourceId {
        match self.index.get(&(identity.into(), text.into())) {
            Some(&at) => SourceId(u32::try_from(at + 1).expect("fewer than 2^32 sources")),
            None => self.add(identity, text),
        }
    }

    /// Adds the text of the source `identity`, returning its id.
    pub fn add(&mut self, identity: &str, text: &str) -> SourceId {
        let line_starts = std::iter::once(0)
            .chain(
                text.match_indices('\n')
                    .map(|(at, _)| u32::try_from(at + 1).expect("a source under 4 GiB")),
            )
            .collect();
        let mut extra = 0;
        let wide = text
            .char_indices()
            .filter(|(_, c)| c.len_utf8() > 1)
            .map(|(at, c)| {
                extra += c.len_utf8() as u32 - 1;
                (u32::try_from(at).expect("a source under 4 GiB"), extra)
            })
            .collect();
        let source = std::sync::Arc::new(Source {
            identity: identity.into(),
            text: text.into(),
            line_starts,
            wide,
        });
        self.index
            .entry((
                std::sync::Arc::clone(&source.identity),
                std::sync::Arc::clone(&source.text),
            ))
            .or_insert(self.sources.len());
        self.sources.push(source);
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
        let column = source.chars(source.line_starts[line - 1], start);
        Some((
            std::sync::Arc::clone(&source.identity),
            u32::try_from(line).unwrap_or(u32::MAX),
            column,
        ))
    }

    /// Where `span` is, if its source is one of these.
    pub fn locate(&self, span: Span) -> Option<Location> {
        let source = self
            .sources
            .get(usize::try_from(span.source.0).ok()?.checked_sub(1)?)?;
        let start = u32::from(span.range.start());
        let end = u32::from(span.range.end());
        let line = source.line_starts.partition_point(|&s| s <= start);
        Some(Location {
            source: std::sync::Arc::clone(&source.identity),
            line: u32::try_from(line).unwrap_or(u32::MAX),
            column: source.chars(source.line_starts[line - 1], start),
            position: source.chars(0, start) + 1,
            span: source.chars(start, end),
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

    #[test]
    fn columns_are_counted_without_walking_the_line() {
        let mut text = String::from("(a\n  é b)\n");
        for i in 0..200 {
            text.push_str(["x ", "λ ", "日本 ", "😀"][i % 4]);
        }
        text.push_str("\nend");
        let mut sources = Sources::new();
        let id = sources.add("test", &text);
        for (start, _) in text.char_indices() {
            let span = Span {
                source: id,
                range: TextRange::new(TextSize::from(start as u32), TextSize::from(start as u32)),
            };
            let line_start = text[..start].rfind('\n').map_or(0, |n| n + 1);
            let column = text[line_start..start].chars().count() as u32;
            let (_, line, c) = sources.line_column(span).unwrap();
            assert_eq!(c, column, "at {start}");
            let located = sources.locate(span).unwrap();
            assert_eq!((located.line, located.column), (line, column));
            assert_eq!(located.position, text[..start].chars().count() as u32 + 1);
        }
        // Not a character boundary: inside `é`.
        assert_eq!(sources.sources[0].chars(5, 6), 0);
    }

    /// Finding a source among many is not a scan of them all: 16,000 took
    /// 2.2 s that way in a debug build.
    #[test]
    fn many_sources_are_interned_in_linear_time() {
        let start = std::time::Instant::now();
        let mut sources = Sources::new();
        let n = 80_000u32;
        for i in 0..n {
            assert_eq!(sources.intern(&format!("s{i}"), "x"), SourceId(i + 1));
        }
        // Again: all found.
        for i in 0..n {
            assert_eq!(sources.intern(&format!("s{i}"), "x"), SourceId(i + 1));
        }
        // The same identity with other text is another source.
        assert_eq!(sources.intern("s0", "y"), SourceId(n + 1));
        let mut other = Sources::new();
        other.intern("s5", "x");
        other.intern("fresh", "x");
        let map = sources.absorb(&other);
        assert_eq!(map[&SourceId(1)], SourceId(6));
        assert_eq!(map[&SourceId(2)], SourceId(n + 2));
        assert!(start.elapsed().as_secs() < 5, "took {:?}", start.elapsed());
    }
}
