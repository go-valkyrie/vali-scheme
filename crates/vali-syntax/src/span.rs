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
