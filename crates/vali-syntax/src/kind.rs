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

    /// Text that is not a valid token, with a matching [`LexError`] saying
    /// why; or, as a node, tokens the parser could not place.
    ///
    /// [`LexError`]: crate::LexError
    Error,

    // Nodes
    /// The whole source.
    Root,
    /// `(` … `)` or `[` … `]`, including a `Dot` and its tail if dotted.
    List,
    /// `#(` … `)`
    Vector,
    /// `#u8(` … `)`
    Bytevector,
    /// A prefix such as `'` or `#'`, then the datum it applies to.
    Abbreviation,
    /// A `LabelDef` token, then the datum it labels.
    Labelled,
    /// `#;` and the datum it comments out. Tools treat it as trivia.
    CommentedDatum,
}

impl SyntaxKind {
    /// Every kind, in declaration order, so that `ALL[k as usize] == k`.
    pub(crate) const ALL: &[SyntaxKind] = &[
        SyntaxKind::Whitespace,
        SyntaxKind::LineComment,
        SyntaxKind::BlockComment,
        SyntaxKind::Shebang,
        SyntaxKind::Directive,
        SyntaxKind::LParen,
        SyntaxKind::RParen,
        SyntaxKind::LBracket,
        SyntaxKind::RBracket,
        SyntaxKind::VectorOpen,
        SyntaxKind::BytevectorOpen,
        SyntaxKind::Quote,
        SyntaxKind::Quasiquote,
        SyntaxKind::Unquote,
        SyntaxKind::UnquoteSplicing,
        SyntaxKind::Syntax,
        SyntaxKind::Quasisyntax,
        SyntaxKind::Unsyntax,
        SyntaxKind::UnsyntaxSplicing,
        SyntaxKind::DatumComment,
        SyntaxKind::LabelDef,
        SyntaxKind::LabelRef,
        SyntaxKind::Dot,
        SyntaxKind::Identifier,
        SyntaxKind::Keyword,
        SyntaxKind::Number,
        SyntaxKind::Boolean,
        SyntaxKind::Character,
        SyntaxKind::String,
        SyntaxKind::Error,
        SyntaxKind::Root,
        SyntaxKind::List,
        SyntaxKind::Vector,
        SyntaxKind::Bytevector,
        SyntaxKind::Abbreviation,
        SyntaxKind::Labelled,
        SyntaxKind::CommentedDatum,
    ];

    /// The kind with this `u16` value, if there is one.
    pub fn from_u16(raw: u16) -> Option<SyntaxKind> {
        SyntaxKind::ALL.get(usize::from(raw)).copied()
    }

    /// Whether a token of this kind can start a datum.
    pub(crate) fn starts_datum(self) -> bool {
        use SyntaxKind::*;
        matches!(
            self,
            Identifier
                | Keyword
                | Number
                | Boolean
                | Character
                | String
                | Error
                | LabelRef
                | LabelDef
                | LParen
                | LBracket
                | VectorOpen
                | BytevectorOpen
                | Quote
                | Quasiquote
                | Unquote
                | UnquoteSplicing
                | Syntax
                | Quasisyntax
                | Unsyntax
                | UnsyntaxSplicing
        )
    }

    /// For an abbreviation prefix, the symbol it abbreviates: `'x` is
    /// `(quote x)`.
    pub(crate) fn abbreviation_symbol(self) -> Option<&'static str> {
        use SyntaxKind::*;
        Some(match self {
            Quote => "quote",
            Quasiquote => "quasiquote",
            Unquote => "unquote",
            UnquoteSplicing => "unquote-splicing",
            Syntax => "syntax",
            Quasisyntax => "quasisyntax",
            Unsyntax => "unsyntax",
            UnsyntaxSplicing => "unsyntax-splicing",
            _ => return None,
        })
    }
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
