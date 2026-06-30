//! Lexer for the .akrs script language.
//!
//! Uses explicit structural markers (#, ?, |, end) for block delimiting.
//! Indentation is purely visual and has no semantic meaning.
//!
//! Symbol scheme:
//! - `#` section header
//! - `->` flow navigation
//! - `=>` visit with return
//! - `<=` return from visit (also LtEq in expressions; disambiguated by parser)
//! - `~~` story end
//! - `+` character entrance
//! - `-` character exit (or arithmetic minus in expressions)
//! - `--` comment

use crate::token::{LineCol, LocSpan, Token, TokenKind};

/// Lexer error with location.
#[derive(Debug, Clone)]
pub struct LexError {
    pub message: String,
    pub span: LocSpan,
}

pub struct Lexer<'a> {
    source: &'a str,
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
    tokens: Vec<Token>,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            chars: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
            tokens: Vec::new(),
        }
    }

    fn current_linecol(&self) -> LineCol {
        LineCol { line: self.line, col: self.col }
    }

    fn make_span(&self, start_pos: usize, start_lc: LineCol) -> LocSpan {
        LocSpan::new(start_pos, self.pos, start_lc, self.current_linecol())
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.chars.get(self.pos).copied();
        if let Some(c) = ch {
            self.pos += 1;
            if c == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        ch
    }

    /// Tokenize the entire source.
    pub fn tokenize(mut self) -> Result<Vec<Token>, Vec<LexError>> {
        let mut errors = Vec::new();

        while self.pos < self.chars.len() {
            let ch = self.chars[self.pos];

            match ch {
                // Skip whitespace (except newlines)
                ' ' | '\t' | '\r' => {
                    self.advance();
                }
                '\n' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    // Emit newline (but skip consecutive ones)
                    if !self.tokens.is_empty()
                        && let Some(last) = self.tokens.last()
                        && last.kind != TokenKind::Newline
                    {
                        self.tokens.push(Token::new(
                            TokenKind::Newline,
                            self.make_span(start, sp),
                        ));
                    }
                }
                // Comments: -- to end of line
                '-' if self.peek_at(1) == Some('-') => {
                    while self.pos < self.chars.len() && self.chars[self.pos] != '\n' {
                        self.advance();
                    }
                }
                // Section mark: # (hash)
                '#' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    self.tokens.push(Token::new(TokenKind::SectionMark, self.make_span(start, sp)));
                }
                // Flow arrows and story end: ->, =>, <=, ~~
                // ~ only used for ~~ (story end)
                '~' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance(); // consume ~
                    if self.peek() == Some('~') {
                        self.advance();
                        self.tokens.push(Token::new(TokenKind::StoryEnd, self.make_span(start, sp)));
                    } else {
                        errors.push(LexError {
                            message: format!("unexpected '~' followed by {:?} (only '~~' is valid)", self.peek()),
                            span: self.make_span(start, sp),
                        });
                    }
                }
                '?' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    self.tokens.push(Token::new(TokenKind::Question, self.make_span(start, sp)));
                }
                '|' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    self.tokens.push(Token::new(TokenKind::Pipe, self.make_span(start, sp)));
                }
                '@' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    self.tokens.push(Token::new(TokenKind::At, self.make_span(start, sp)));
                }
                '+' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    // Check for +=
                    if self.peek() == Some('=') {
                        self.advance();
                        self.tokens.push(Token::new(TokenKind::PlusEq, self.make_span(start, sp)));
                    } else {
                        self.tokens.push(Token::new(TokenKind::Plus, self.make_span(start, sp)));
                    }
                }
                '$' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    self.tokens.push(Token::new(TokenKind::Dollar, self.make_span(start, sp)));
                }
                ':' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    self.tokens.push(Token::new(TokenKind::Colon, self.make_span(start, sp)));
                }
                '(' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    self.tokens.push(Token::new(TokenKind::LParen, self.make_span(start, sp)));
                }
                ')' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    self.tokens.push(Token::new(TokenKind::RParen, self.make_span(start, sp)));
                }
                ',' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    self.tokens.push(Token::new(TokenKind::Comma, self.make_span(start, sp)));
                }
                '=' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    if self.peek() == Some('=') {
                        // == equality
                        self.advance();
                        self.tokens.push(Token::new(TokenKind::EqEq, self.make_span(start, sp)));
                    } else if self.peek() == Some('>') {
                        // => visit arrow
                        self.advance();
                        self.tokens.push(Token::new(TokenKind::VisitArrow, self.make_span(start, sp)));
                    } else {
                        // = assignment
                        self.tokens.push(Token::new(TokenKind::Assign, self.make_span(start, sp)));
                    }
                }
                '!' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        self.tokens.push(Token::new(TokenKind::NotEq, self.make_span(start, sp)));
                    } else {
                        // ! as not operator
                        self.tokens.push(Token::new(TokenKind::Not, self.make_span(start, sp)));
                    }
                }
                '<' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        self.tokens.push(Token::new(TokenKind::LtEq, self.make_span(start, sp)));
                    } else {
                        self.tokens.push(Token::new(TokenKind::Lt, self.make_span(start, sp)));
                    }
                }
                '>' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        self.tokens.push(Token::new(TokenKind::GtEq, self.make_span(start, sp)));
                    } else {
                        self.tokens.push(Token::new(TokenKind::Gt, self.make_span(start, sp)));
                    }
                }
                '-' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    if self.peek() == Some('>') {
                        // -> flow arrow
                        self.advance();
                        self.tokens.push(Token::new(TokenKind::FlowArrow, self.make_span(start, sp)));
                    } else if self.peek() == Some('=') {
                        // -= compound assignment
                        self.advance();
                        self.tokens.push(Token::new(TokenKind::MinusEq, self.make_span(start, sp)));
                    } else {
                        // - minus (arithmetic or character exit direction)
                        self.tokens.push(Token::new(TokenKind::Minus2, self.make_span(start, sp)));
                    }
                }
                '*' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    self.tokens.push(Token::new(TokenKind::Star, self.make_span(start, sp)));
                }
                '/' => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    self.advance();
                    self.tokens.push(Token::new(TokenKind::Slash, self.make_span(start, sp)));
                }
                '"' | '\'' => {
                    if let Err(e) = self.lex_string(ch) {
                        errors.push(e);
                    }
                }
                '0'..='9' => {
                    if let Err(e) = self.lex_number() {
                        errors.push(e);
                    }
                }
                'a'..='z' | 'A'..='Z' | '_' => {
                    self.lex_ident();
                }
                _ => {
                    let sp = self.current_linecol();
                    let start = self.pos;
                    errors.push(LexError {
                        message: format!("unexpected character: {:?}", ch),
                        span: self.make_span(start, sp),
                    });
                    self.advance();
                }
            }
        }

        // Final newline
        if !self.tokens.is_empty()
            && let Some(last) = self.tokens.last()
            && last.kind != TokenKind::Newline
        {
            let sp = self.current_linecol();
            self.tokens.push(Token::new(TokenKind::Newline, LocSpan::new(self.pos, self.pos, sp, sp)));
        }

        let sp = self.current_linecol();
        self.tokens.push(Token::new(TokenKind::Eof, LocSpan::new(self.pos, self.pos, sp, sp)));

        if errors.is_empty() {
            Ok(self.tokens)
        } else {
            Err(errors)
        }
    }

    fn lex_string(&mut self, quote: char) -> Result<(), LexError> {
        let sp = self.current_linecol();
        let start = self.pos;
        self.advance(); // opening quote
        let mut value = String::new();

        while self.pos < self.chars.len() {
            let ch = self.chars[self.pos];
            if ch == quote {
                self.advance();
                self.tokens.push(Token::new(TokenKind::String(value), self.make_span(start, sp)));
                return Ok(());
            }
            if ch == '\\' {
                self.advance();
                if self.pos >= self.chars.len() {
                    return Err(LexError {
                        message: "unterminated escape sequence".to_string(),
                        span: self.make_span(start, sp),
                    });
                }
                let esc = self.chars[self.pos];
                match esc {
                    'n' => value.push('\n'),
                    't' => value.push('\t'),
                    'r' => value.push('\r'),
                    '\\' => value.push('\\'),
                    '"' => value.push('"'),
                    '\'' => value.push('\''),
                    c => { value.push('\\'); value.push(c); }
                }
                self.advance();
            } else if ch == '\n' {
                return Err(LexError {
                    message: "unterminated string (newline in string)".to_string(),
                    span: self.make_span(start, sp),
                });
            } else {
                value.push(ch);
                self.advance();
            }
        }

        Err(LexError {
            message: "unterminated string literal".to_string(),
            span: self.make_span(start, sp),
        })
    }

    fn lex_number(&mut self) -> Result<(), LexError> {
        let sp = self.current_linecol();
        let start = self.pos;
        let mut s = String::new();
        let mut is_float = false;

        while self.pos < self.chars.len() {
            match self.chars[self.pos] {
                '0'..='9' => { s.push(self.chars[self.pos]); self.advance(); }
                '.' if !is_float && self.peek_at(1).map(|c| c.is_ascii_digit()).unwrap_or(false) => {
                    is_float = true;
                    s.push('.');
                    self.advance();
                }
                _ => break,
            }
        }

        if is_float {
            let v: f64 = s.parse().map_err(|_| LexError {
                message: format!("invalid float: {}", s),
                span: self.make_span(start, sp),
            })?;
            self.tokens.push(Token::new(TokenKind::Float(v), self.make_span(start, sp)));
        } else {
            let v: i64 = s.parse().map_err(|_| LexError {
                message: format!("invalid integer: {}", s),
                span: self.make_span(start, sp),
            })?;
            self.tokens.push(Token::new(TokenKind::Integer(v), self.make_span(start, sp)));
        }
        Ok(())
    }

    fn lex_ident(&mut self) {
        let sp = self.current_linecol();
        let start = self.pos;
        let mut s = String::new();

        while self.pos < self.chars.len() {
            match self.chars[self.pos] {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => {
                    s.push(self.chars[self.pos]);
                    self.advance();
                }
                _ => break,
            }
        }

        let kind = match s.as_str() {
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "end" => TokenKind::End,
            "wait" => TokenKind::Wait,
            "and" => TokenKind::And,
            "or" => TokenKind::Or,
            "not" => TokenKind::Not,
            "true" => TokenKind::Integer(1),
            "false" => TokenKind::Integer(0),
            _ => TokenKind::Ident(s),
        };
        self.tokens.push(Token::new(kind, self.make_span(start, sp)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(src: &str) -> Vec<Token> {
        Lexer::new(src).tokenize().unwrap_or_else(|e| panic!("lex error: {:?}", e))
    }

    #[test]
    fn test_section_header() {
        let tokens = lex("# Prologue\n");
        assert!(tokens.iter().any(|t| t.kind == TokenKind::SectionMark));
        assert!(tokens.iter().any(|t| matches!(&t.kind, TokenKind::Ident(s) if s == "Prologue")));
    }

    #[test]
    fn test_flow_arrow() {
        let tokens = lex("-> NextDay\n");
        assert!(tokens.iter().any(|t| t.kind == TokenKind::FlowArrow));
    }

    #[test]
    fn test_visit_return() {
        let tokens = lex("=> Greeting\n<=\n");
        assert!(tokens.iter().any(|t| t.kind == TokenKind::VisitArrow));
        // <= is lexed as LtEq; parser disambiguates at statement level
        assert!(tokens.iter().any(|t| t.kind == TokenKind::LtEq));
    }

    #[test]
    fn test_story_end() {
        let tokens = lex("~~\n");
        assert!(tokens.iter().any(|t| t.kind == TokenKind::StoryEnd));
    }

    #[test]
    fn test_choice_block() {
        let tokens = lex("? \"prompt\"\n| \"opt\"\n?\n");
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Question));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Pipe));
    }

    #[test]
    fn test_dialogue() {
        let tokens = lex("Aki: \"hello\"\n");
        assert!(tokens.iter().any(|t| matches!(&t.kind, TokenKind::Ident(s) if s == "Aki")));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Colon));
        assert!(tokens.iter().any(|t| matches!(&t.kind, TokenKind::String(s) if s == "hello")));
    }

    #[test]
    fn test_stage_commands() {
        let tokens = lex("@bg school\n+ Aki enters\n$affection = 5\n");
        assert!(tokens.iter().any(|t| t.kind == TokenKind::At));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Plus));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Dollar));
    }

    #[test]
    fn test_character_exit_syntax() {
        let tokens = lex("- Aki\n");
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Minus2));
        assert!(tokens.iter().any(|t| matches!(&t.kind, TokenKind::Ident(s) if s == "Aki")));
    }

    #[test]
    fn test_operators() {
        let tokens = lex("$x += 1\n$y -= 2\n$a == $b\n");
        assert!(tokens.iter().any(|t| t.kind == TokenKind::PlusEq));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::MinusEq));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::EqEq));
    }

    #[test]
    fn test_linecol_tracking() {
        let src = "line1\nline2\n";
        let tokens = lex(src);
        // First token should be at line 1
        assert_eq!(tokens[0].span.start_linecol.line, 1);
        // Find newline token
        let nl = tokens.iter().find(|t| t.kind == TokenKind::Newline).unwrap();
        assert_eq!(nl.span.start_linecol.line, 1);
    }
}
