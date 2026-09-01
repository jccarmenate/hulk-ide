//! Lexer for the HULK programming language.
//!
//! Converts raw source code (`&str`) into a flat list of [`Token`]s.
//! This is the first phase of the compiler pipeline:
//!
//! ```text
//! source code -> Lexer -> Vec<Token> -> Parser
//! ```

/// Every distinct kind of token the HULK lexer can produce.
///
/// Variants that carry data store the literal value from source.
/// Variants without data are self-descriptive.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // ── Literals ──────────────────────────────────────────────────────
    /// A numeric literal, e.g. `42` or `3.14`.
    Number(f64),
    /// A string literal, e.g. `"hello world"`
    StringLit(String),
    /// The boolean literal `true`.
    True,
    /// The boolean literal `false`.
    False,

    // ── Arithmetic operators ──────────────────────────────────────────
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Percent,

    // ── String operators ──────────────────────────────────────────────
    /// Single concatenation `@`
    At,
    /// Concatenation with space `@@`
    AtAt,

    // ── Comparison operators ──────────────────────────────────────────
    EqEq,
    Neq,
    Lt,
    Gt,
    Leq,
    Geq,

    // ── Boolean operators ─────────────────────────────────────────────
    And,
    Or,
    Not,

    // ── Assignment ────────────────────────────────────────────────────
    /// Simple assignment `=`
    Assign,
    /// Destructive assignment `:=`
    ColonEq,

    // ── Delimiters ────────────────────────────────────────────────────
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Semicolon,
    Comma,
    Colon,
    Dot,
    /// Thin arrow `->` used in functor type annotations
    Arrow,
    /// Fat arrow `=>` used in inline functions
    FatArrow,

    // ── Keywords ──────────────────────────────────────────────────────
    Let,
    In,
    If,
    Elif,
    Else,
    While,
    For,
    Function,
    Type,
    Inherits,
    New,
    /// The `self` keyword — renamed to avoid clash with Rust's `Self`
    SelfKw,
    Base,
    Is,
    As,
    Protocol,
    Extends,

    // ── Macros ─────────────────────────────────────────────────
    /// `def` keyword for macro definitions
    Def,
    /// `$` sigil for macro variable placeholder parameters.
    Dollar,


    // ── Extra feature: pattern matching ───────────────────────────────
    Match,
    Case,
    Underscore,

    // ── Identifier & end-of-file ──────────────────────────────────────
    /// Any user-defined name: variable, function, type, etc.
    Ident(String),
    /// Signals the end of the token stream.
    Eof,
}

/// A location in the source file, used for error reporting.
///
/// Both `line` and `col` are 1-based, matching what humans
/// expect in error messages: "error at line 3, col 7".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    /// Line number, starting at 1.
    pub line: usize,
    /// Column number, starting at 1.
    pub col: usize,
}

/// A single token produced by the lexer.
///
/// Bundles a [`TokenKind`] with the [`Span`] where it was found,
/// so every later compiler phase can report precise error locations.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// What kind of token this is.
    pub kind: TokenKind,
    /// Where in the source this token starts.
    pub span: Span,
}

/// An error produced during lexing.
///
/// The lexer is intentionally strict: it halts on the first
/// unrecognisable input rather than guessing.
#[derive(Debug, Clone, PartialEq)]
pub enum LexError {
    /// A character that belongs to no HULK token was encountered.
    ///
    /// # Example
    /// `#` or `$` in HULK source code.
    UnexpectedChar {
        /// The offending character.
        ch: char,
        /// Where it appeared.
        span: Span,
    },

    /// A string literal was opened but never closed before end-of-file.
    ///
    /// # Example
    /// `"hello` with no closing quote.
    UnterminatedString {
        /// Where the string literal started.
        span: Span,
    },

    /// An unrecognised escape sequence was found inside a string literal.
    ///
    /// HULK spec §A.2.2 defines only `\"`, `\\`, `\n`, and `\t` as valid escapes.
    ///
    /// # Example
    /// `"hello\qworld"` — `\q` is not a defined escape sequence.
    InvalidEscape {
        /// The character that followed the backslash.
        ch: char,
        /// Where the backslash appeared in the source.
        span: Span,
    },
}

/// The HULK lexer.
///
/// Created with [`Lexer::new`] and consumed by [`Lexer::tokenize`].
///
/// # Example
/// ```
/// use hulk_lexer::Lexer;
/// let mut lexer = Lexer::new("1 + 2;");
/// let tokens = lexer.tokenize().unwrap();
/// ```
pub struct Lexer {
    /// Source code as individual characters for index-based access.
    source: Vec<char>,
    /// Index of the next character to be read.
    current: usize,
    /// Current line number (1-based).
    line: usize,
    /// Current column number (1-based).
    col: usize,
}

impl Lexer {
    /// Creates a new [`Lexer`] ready to tokenize `source`.
    ///
    /// # Arguments
    /// * `source` - The raw HULK source code to tokenize.
    ///
    /// # Example
    /// ```
    /// use hulk_lexer::Lexer;
    /// let mut lexer = Lexer::new("1 + 2;");
    /// ```
    pub fn new(source: &str) -> Self {
        Self {
            source: source.chars().collect(),
            current: 0,
            line: 1,
            col: 1,
        }
    }
    /// Returns `true` if all characters have been consumed.
    fn is_at_end(&self) -> bool {
        self.current >= self.source.len()
    }
    /// Returns the current character without consuming it.
    ///
    /// Returns `'\0'` as a sentinel when the source is exhausted,
    /// so callers can check `peek() != '\0'` without a separate
    /// bounds check.
    fn peek(&self) -> char {
        if self.is_at_end() {
            '\0'
        } else {
            self.source[self.current]
        }
    }

    /// Returns the character at `current + 1` without consuming it.
    ///
    /// Used to distinguish two-character tokens like `==`, `:=`, `=>`.
    /// Returns `'\0'` as a sentinel when the lookahead is out of bounds.
    fn peek_next(&self) -> char {
        if self.current + 1 >= self.source.len() {
            '\0'
        } else {
            self.source[self.current + 1]
        }
    }
    /// Consumes the current character and advances the lexer state.
    ///
    /// Updates `line` and `col` for accurate error reporting.
    /// Increments `line` and resets `col` on newlines.
    ///
    /// # Returns
    /// The consumed character, or `'\0'` if already at end of source.
    fn advance(&mut self) -> char {
        let ch = self.peek();
        if !self.is_at_end() {
            self.current += 1;
            if ch == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            ch
        } else {
            '\0'
        }
    }

    /// Returns a [`Span`] representing the current cursor position.
    ///
    /// Used to stamp every token with where it started in the source.
    fn current_span(&self) -> Span {
        Span {
            line: self.line,
            col: self.col,
        }
    }

    /// Tokenizes the entire source string into a flat list of tokens.
    ///
    /// Skips whitespace and comments. Returns [`LexError`] on the first
    /// unrecognised character or unterminated string literal.
    ///
    /// # Errors
    /// - [`LexError::UnexpectedChar`] — character belongs to no HULK token.
    /// - [`LexError::UnterminatedString`] — string literal never closed.
    pub fn tokenize(&mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();
        while !self.is_at_end() {
            let span: Span = self.current_span();
            let ch: char = self.advance();

            let kind = match ch {
                // ── Whitespace ────────────────────────────────────────────
                // Skip silently — whitespace carries no meaning in HULK.
                ' ' | '\t' | '\r' | '\n' => continue,

                // ── Single-character tokens ───────────────────────────────
                '+' => TokenKind::Plus,
                '-' => {
                    if self.peek() == '>' {
                        self.advance();
                        TokenKind::Arrow
                    } else {
                        TokenKind::Minus
                    }
                }
                '*' => TokenKind::Star,
                '/' => {
                    if self.peek() == '/' {
                        // This is a comment — skip everything until end of line.
                        // WHY: HULK uses // for single-line comments like most languages.
                        while self.peek() != '\n' && !self.is_at_end() {
                            self.advance();
                        }
                        continue;
                    } else {
                        TokenKind::Slash
                    }
                }
                '^' => TokenKind::Caret,
                '%' => TokenKind::Percent,
                '(' => TokenKind::LParen,
                ')' => TokenKind::RParen,
                '{' => TokenKind::LBrace,
                '}' => TokenKind::RBrace,
                '[' => TokenKind::LBracket,
                ']' => TokenKind::RBracket,
                ';' => TokenKind::Semicolon,
                ',' => TokenKind::Comma,
                '.' => TokenKind::Dot,
                '|' => TokenKind::Or,

                // ── One-or-two character tokens ───────────────────────────
                '=' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::EqEq
                    } else if self.peek() == '>' {
                        self.advance();
                        TokenKind::FatArrow
                    } else {
                        TokenKind::Assign
                    }
                }
                '!' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::Neq
                    } else {
                        TokenKind::Not
                    }
                }
                '<' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::Leq
                    } else {
                        TokenKind::Lt
                    }
                }
                '>' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::Geq
                    } else {
                        TokenKind::Gt
                    }
                }
                ':' => {
                    if self.peek() == '=' {
                        self.advance();
                        TokenKind::ColonEq
                    } else {
                        TokenKind::Colon
                    }
                }
                '&' => TokenKind::And,
                '@' => {
                    if self.peek() == '@' {
                        self.advance();
                        TokenKind::AtAt
                    } else {
                        TokenKind::At
                    }
                }
                '$' => TokenKind::Dollar,

                // ── String literals ───────────────────────────────────────
                '"' => self.lex_string(span)?,

                // ── Numbers ───────────────────────────────────────────────
                '0'..='9' => self.lex_number(ch),

                // ── Identifiers and keywords ──────────────────────────────
                'a'..='z' | 'A'..='Z' => self.lex_ident(ch),
                '_' => TokenKind::Underscore,

                // ── Unknown character ─────────────────────────────────────
                _ => {
                    return Err(LexError::UnexpectedChar { ch, span });
                }
            };

            tokens.push(Token { kind, span });
        }

        tokens.push(Token {
            kind: TokenKind::Eof,
            span: self.current_span(),
        });

        Ok(tokens)
    }

    /// Consumes an identifier or keyword starting with `first_char`.
    ///
    /// Reads letters, digits, and underscores until a non-identifier
    /// character is found. Then checks if the result is a HULK keyword.
    ///
    /// # Arguments
    /// * `first_char` - The first character, already consumed by `tokenize`.
    fn lex_ident(&mut self, first_char: char) -> TokenKind {
        // Start building the identifier string with the first character.
        let mut text = String::new();
        text.push(first_char);

        // Keep consuming as long as the character can be part of an identifier.
        // WHY: identifiers can contain letters, digits, and underscores after
        // the first character (which is always a letter, checked in tokenize).
        while self.peek().is_alphanumeric() || self.peek() == '_' {
            text.push(self.advance());
        }

        // Check if the collected text is a reserved keyword.
        // WHY: keywords look like identifiers but have special meaning.
        // We check here instead of in the main match to keep tokenize clean.

        match text.as_str() {
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "let" => TokenKind::Let,
            "in" => TokenKind::In,
            "if" => TokenKind::If,
            "elif" => TokenKind::Elif,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "function" => TokenKind::Function,
            "type" => TokenKind::Type,
            "inherits" => TokenKind::Inherits,
            "new" => TokenKind::New,
            "self" => TokenKind::SelfKw,
            "base" => TokenKind::Base,
            "is" => TokenKind::Is,
            "as" => TokenKind::As,
            "protocol" => TokenKind::Protocol,
            "extends" => TokenKind::Extends,
            "def" | "define" => TokenKind::Def,
            "match" => TokenKind::Match,
            "case" => TokenKind::Case,
            // Not a keyword — it's a user-defined identifier.
            _ => TokenKind::Ident(text),
        }
    }

    /// Consumes a numeric literal starting with `first_digit`.
    ///
    /// Collects digits and an optional single decimal point.
    /// All HULK numbers are 64-bit floats — there is no integer type.
    ///
    /// # Arguments
    /// * `first_digit` - The first digit, already consumed by `tokenize`.
    fn lex_number(&mut self, first_digit: char) -> TokenKind {
        // Start with the first digit we already consumed.
        let mut number = String::new();
        number.push(first_digit);

        // Consume all following digits.
        while self.peek().is_ascii_digit() {
            number.push(self.advance());
        }

        // Consume a decimal point and the digits after it, if present.
        // WHY: peek_next check ensures we don't consume a `..` range or
        // a trailing dot with no digits after it.
        if self.peek() == '.' && self.peek_next().is_ascii_digit() {
            number.push(self.advance()); // consume the dot
            while self.peek().is_ascii_digit() {
                number.push(self.advance());
            }
        }

        // Parse the collected text into an f64.
        // WHY unwrap is safe here: we only collected digits and one dot,
        // so the string is guaranteed to be a valid float.
        let value = number.parse::<f64>().unwrap();
        TokenKind::Number(value)
    }

    /// Consumes a string literal after the opening `"` has been consumed.
    ///
    /// Valid escape sequences per HULK spec §A.2.2: `\"` (quote), `\\` (backslash),
    /// `\n` (newline), `\t` (tab). Any other `\X` sequence is a hard error.
    ///
    /// # Arguments
    /// * `open_span` - The [`Span`] of the opening `"`, used in error
    ///   reporting so the error points to where the string started,
    ///   not where the file ended.
    ///
    /// # Errors
    /// - [`LexError::UnterminatedString`] if EOF is reached before the closing `"`.
    /// - [`LexError::InvalidEscape`] if a backslash is followed by an unrecognised character.
    fn lex_string(&mut self, open_span: Span) -> Result<TokenKind, LexError> {
        let mut text = String::new();

        loop {
            // Check for EOF before advancing — an unclosed string is an error.
            // WHY: we report open_span (where the string started) not current
            // position, because that's where the programmer made the mistake.

            if self.is_at_end() {
                return Err(LexError::UnterminatedString { span: open_span });
            }

            let ch = self.advance();

            match ch {
                // Closing quote — string is complete, return what we collected.
                '"' => return Ok(TokenKind::StringLit(text)),

                // Escape sequence — the next character has special meaning.
                '\\' => {
                    // Capture the span of the character after the backslash before consuming it,
                    // so that InvalidEscape points at the unrecognised character, not past it.
                    let escape_span = self.current_span();
                    let escaped = self.advance();

                    match escaped {
                        '"' => text.push('"'),   // literal quote
                        '\\' => text.push('\\'), // literal backslash
                        'n' => text.push('\n'),  // new line
                        't' => text.push('\t'),  // tab

                        // HULK spec §A.2.2: only \", \\, \n, \t are valid escapes.
                        // Any other \X is a hard error — silent pass-through would hide bugs.
                        other => {
                            return Err(LexError::InvalidEscape {
                                ch: other,
                                span: escape_span,
                            })
                        }
                    }
                }

                _ => text.push(ch),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokenizes source and returns only the token kinds.
    ///
    /// WHY: most tests only care about kinds, not spans.
    /// Avoids repeating Lexer::new(...).tokenize().unwrap() everywhere.
    fn lex(source: &str) -> Vec<TokenKind> {
        Lexer::new(source)
            .tokenize()
            .unwrap()
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    /// Tokenizes source and expects an error.
    ///
    /// WHY: error tests need the LexError value to assert on.
    fn lex_err(source: &str) -> LexError {
        Lexer::new(source).tokenize().unwrap_err()
    }

    #[test]
    fn test_plus() {
        assert_eq!(lex("+"), vec![TokenKind::Plus, TokenKind::Eof]);
    }

    #[test]
    fn test_minus() {
        assert_eq!(lex("-"), vec![TokenKind::Minus, TokenKind::Eof]);
    }

    #[test]
    fn test_integer_number() {
        assert_eq!(lex("42"), vec![TokenKind::Number(42.0), TokenKind::Eof]);
    }

    #[test]
    fn test_float_number() {
        assert_eq!(lex("1.5"), vec![TokenKind::Number(1.5), TokenKind::Eof]);
    }

    #[test]
    fn test_string_literal() {
        assert_eq!(
            lex("\"hello\""),
            vec![TokenKind::StringLit("hello".to_string()), TokenKind::Eof]
        );
    }

    #[test]
    fn test_keyword_let() {
        assert_eq!(lex("let"), vec![TokenKind::Let, TokenKind::Eof]);
    }

    #[test]
    fn test_keyword_if() {
        assert_eq!(lex("if"), vec![TokenKind::If, TokenKind::Eof]);
    }

    #[test]
    fn test_keyword_def() {
        assert_eq!(lex("def"), vec![TokenKind::Def, TokenKind::Eof]);
    }

    #[test]
    fn test_identifier() {
        assert_eq!(
            lex("myVar"),
            vec![TokenKind::Ident("myVar".to_string()), TokenKind::Eof]
        );
    }

    #[test]
    fn test_two_char_eq() {
        assert_eq!(lex("=="), vec![TokenKind::EqEq, TokenKind::Eof]);
    }

    #[test]
    fn test_two_char_colon_eq() {
        assert_eq!(lex(":="), vec![TokenKind::ColonEq, TokenKind::Eof]);
    }

    #[test]
    fn test_two_char_fat_arrow() {
        assert_eq!(lex("=>"), vec![TokenKind::FatArrow, TokenKind::Eof]);
    }

    #[test]
    fn test_whitespace_ignored() {
        assert_eq!(lex("  +  "), vec![TokenKind::Plus, TokenKind::Eof]);
    }

    #[test]
    fn test_comment_ignored() {
        assert_eq!(
            lex("// this is a comment\n+"),
            vec![TokenKind::Plus, TokenKind::Eof]
        );
    }

    #[test]
    fn test_unexpected_char_error() {
        let err = lex_err("#");
        assert!(matches!(err, LexError::UnexpectedChar { .. }));
    }

    #[test]
    fn test_unterminated_string_error() {
        let err = lex_err("\"hello");
        assert!(matches!(err, LexError::UnterminatedString { .. }));
    }

    #[test]
    fn test_full_expression() {
        assert_eq!(
            lex("let x = 5;"),
            vec![
                TokenKind::Let,
                TokenKind::Ident("x".to_string()),
                TokenKind::Assign,
                TokenKind::Number(5.0),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }
}
