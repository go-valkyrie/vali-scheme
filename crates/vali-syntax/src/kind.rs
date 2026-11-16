/// Every kind of token, and later every kind of tree node.
///
/// One enum, because Rowan identifies tokens and nodes by a single kind
/// type. Node kinds arrive with the parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
#[non_exhaustive]
pub enum SyntaxKind {
    // Trivia
    /// Spaces, tabs, newlines and other Unicode whitespace.
    Whitespace,
    /// `; …` to the end of the line.
    LineComment,
    /// `#| … |#`, which may nest.
    BlockComment,
    /// A first line starting `#!/` or `#! `, read as a comment.
    Shebang,

    /// `#!fold-case`, `#!no-fold-case` or `#!r6rs`.
    Directive,

    // Delimiters
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `#(`, opening a vector.
    VectorOpen,
    /// `#u8(`, opening a bytevector.
    BytevectorOpen,

    // Abbreviations
    /// `'`
    Quote,
    /// `` ` ``
    Quasiquote,
    /// `,`
    Unquote,
    /// `,@`
    UnquoteSplicing,
    /// `#'`, for `syntax`.
    Syntax,
    /// `` #` ``, for `quasisyntax`.
    Quasisyntax,
    /// `#,`, for `unsyntax`.
    Unsyntax,
    /// `#,@`, for `unsyntax-splicing`.
    UnsyntaxSplicing,

    // Structure the parser resolves
    /// `#;`, commenting out the next datum.
    DatumComment,
    /// `#12=`, labelling the next datum.
    LabelDef,
    /// `#12#`, referring to a labelled datum.
    LabelRef,
    /// A lone `.`, as in a dotted pair.
    Dot,

    // Atoms
    /// A symbol, written plainly or between vertical bars.
    Identifier,
    /// An SRFI 88 keyword: an identifier ending in `:`, such as `foo:`.
    Keyword,
    /// A number in R7RS syntax.
    Number,
    /// `#t`, `#f`, `#true` or `#false`.
    Boolean,
    /// A character such as `#\a`, `#\space` or `#\x41`.
    Character,
    /// A string literal.
    String,

    /// Text that is not a valid token. A matching [`LexError`] says why.
    ///
    /// [`LexError`]: crate::LexError
    Error,
}

impl SyntaxKind {
    /// Whether the kind is trivia: whitespace or a comment, which carries no
    /// meaning for the program.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            SyntaxKind::Whitespace
                | SyntaxKind::LineComment
                | SyntaxKind::BlockComment
                | SyntaxKind::Shebang
        )
    }
}
