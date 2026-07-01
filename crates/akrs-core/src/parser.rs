//! Recursive descent parser for the .akrs script language.
//!
//! Block structure uses explicit markers:
//! - `?` ... `?` for choice blocks (open and close with same symbol)
//! - `if` ... `else` ... `end` for conditionals
//! - `#` for section headers
//! - `->` for flow, `=>` for visit, `<=` for return
//! - `+` for character entrance, `-` for character exit
//! Indentation is visual only, not semantic.

use crate::ast::*;
use crate::token::{LocSpan, Token, TokenKind};

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub span: LocSpan,
    pub hint: Option<String>,
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    errors: Vec<ParseError>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0, errors: Vec::new() }
    }

    pub fn parse(mut self) -> Result<Program, Vec<ParseError>> {
        let mut sections = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() {
            if self.check(&TokenKind::SectionMark) {
                match self.parse_section() {
                    Ok(s) => sections.push(s),
                    Err(e) => { self.errors.push(e); self.sync_to_section(); }
                }
            } else {
                // Nodes outside a section - error
                self.errors.push(ParseError {
                    message: "nodes must be inside a section (# Name)".to_string(),
                    span: self.current_span(),
                    hint: Some("add a section header like '# Start' before this".to_string()),
                });
                self.sync_to_section();
            }
            self.skip_newlines();
        }

        let entry = sections.first().map(|s| s.name.clone());
        if self.errors.is_empty() {
            Ok(Program { sections, entry })
        } else {
            Err(self.errors)
        }
    }

    fn parse_section(&mut self) -> Result<Section, ParseError> {
        let span = self.current_span();
        self.advance(); // #
        self.skip_newlines();

        let name = self.expect_ident("section name")?;
        self.skip_newlines();

        // Optional closing #
        if self.check(&TokenKind::SectionMark) {
            self.advance();
        }
        self.skip_newlines();

        let mut nodes = Vec::new();
        while !self.is_at_end() && !self.check(&TokenKind::SectionMark) {
            match self.parse_node() {
                Ok(node) => nodes.push(node),
                Err(e) => { self.errors.push(e); self.sync_in_section(); }
            }
            self.skip_newlines();
        }

        Ok(Section { name, nodes, span })
    }

    fn parse_node(&mut self) -> Result<Node, ParseError> {
        match self.peek().kind.clone() {
            TokenKind::SectionMark => Err(ParseError {
                message: "unexpected section header".to_string(),
                span: self.current_span(),
                hint: Some("close the current section before starting a new one".to_string()),
            }),
            TokenKind::String(_) => self.parse_narration(),
            TokenKind::Ident(_) => self.parse_dialogue_or_keyword(),
            TokenKind::At => self.parse_command(),
            TokenKind::Plus => self.parse_direction(),
            TokenKind::Minus2 => self.parse_exit_direction(),
            TokenKind::Dollar => self.parse_varop(),
            TokenKind::Question => self.parse_choice(),
            TokenKind::If => self.parse_conditional(),
            TokenKind::FlowArrow => self.parse_flow(),
            TokenKind::VisitArrow => self.parse_visit(),
            TokenKind::LtEq => self.parse_return(),
            TokenKind::StoryEnd => self.parse_story_end(),
            TokenKind::Wait => self.parse_wait(),
            _ => Err(ParseError {
                message: format!("unexpected token: {}", self.peek().kind),
                span: self.current_span(),
                hint: Some("expected dialogue, narration, @command, +enter, -exit, $var, ?, if, ->, =>, <=, ~~, or wait".to_string()),
            }),
        }
    }

    fn parse_narration(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        let text = self.expect_string("narration text")?;
        self.consume_newline();
        Ok(Node::Narration { text, span })
    }

    fn parse_dialogue_or_keyword(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        let speaker = self.expect_ident("speaker name")?;

        // Optional pose: `Aki (happy)`
        let pose = if self.check(&TokenKind::LParen) {
            self.advance();
            let p = self.expect_ident("pose name")?;
            self.expect(&TokenKind::RParen, ")")?;
            Some(p)
        } else {
            None
        };

        // Expect colon
        self.expect(&TokenKind::Colon, ":")?;
        let text = self.expect_string("dialogue text")?;
        self.consume_newline();

        Ok(Node::Dialogue { speaker, pose, text, span })
    }

    fn parse_command(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        self.advance(); // @
        let cmd = self.expect_ident("command name")?;
        let mut args = Vec::new();

        // Collect string or ident args until newline or "with"
        while !self.is_at_end() && !matches!(self.peek().kind, TokenKind::Newline | TokenKind::Eof) {
            if let TokenKind::Ident(s) = &self.peek().kind {
                if s == "with" {
                    break;
                }
                let s = s.clone();
                self.advance();
                args.push(s);
            } else if let TokenKind::String(s) = &self.peek().kind.clone() {
                let s = s.clone();
                self.advance();
                args.push(s);
            } else {
                break;
            }
        }

        // Optional transition: `with fade`
        let transition = if let TokenKind::Ident(s) = &self.peek().kind {
            if s == "with" {
                self.advance();
                let tname = self.expect_ident("transition name")?;
                match Transition::from_name(&tname) {
                    Some(t) => Some(t),
                    None => {
                        return Err(ParseError {
                            message: format!("unknown transition: '{}'", tname),
                            span: self.current_span(),
                            hint: Some("valid: fade, fade_black, fade_white, slide_left, slide_right, slide_up, slide_down, dissolve, wipe_left, wipe_right, blur, instant".to_string()),
                        });
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        self.consume_newline();
        Ok(Node::Command { cmd, args, transition, span })
    }

    fn parse_direction(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        self.advance(); // +
        self.skip_newlines();

        // Parse: Character [(pose)] [enters|exits] [position] [with transition]
        // - `+ 心夏 (kokonabody1) 居中`           (新语法：立绘 + 中文位置)
        // - `+ 心夏 居左`                         (新语法：仅位置)
        // - `+ 心夏`                              (新语法：默认居中)
        // - `+ Aki enters from left with fade`   (旧语法：向后兼容)
        let character = self.expect_ident("character name")?;

        // 可选立绘名：` (kokonabody1)`
        let mut pose = None;
        if self.check(&TokenKind::LParen) {
            self.advance();
            pose = Some(self.expect_ident("pose/sprite name")?);
            self.expect(&TokenKind::RParen, ")")?;
        }

        // `+` 默认表示入场；若显式写出 exits/leave 则为出场
        let mut kind = DirectionKind::Enter;
        let mut position = None;
        let mut transition = None;

        while !self.is_at_end() && !matches!(self.peek().kind, TokenKind::Newline | TokenKind::Eof) {
            if let TokenKind::Ident(s) = &self.peek().kind {
                match s.as_str() {
                    "enters" | "enter" => { self.advance(); }
                    "exits" | "exit" | "leaves" | "leave" => {
                        kind = DirectionKind::Exit;
                        self.advance();
                    }
                    "from" | "to" | "at" => {
                        self.advance();
                        if let TokenKind::Ident(pos_name) = &self.peek().kind.clone() {
                            position = Position::from_name(pos_name)
                                .or_else(|| pos_name.parse::<f32>().ok().map(Position::Custom));
                            self.advance();
                            if position.is_none() {
                                return Err(ParseError {
                                    message: format!("unknown position: '{}'", pos_name),
                                    span: self.current_span(),
                                    hint: Some("use 'left'/'居左', 'center'/'居中', 'right'/'居右', or a number".to_string()),
                                });
                            }
                        }
                    }
                    "with" => {
                        self.advance();
                        let tname = self.expect_ident("transition name")?;
                        transition = match Transition::from_name(&tname) {
                            Some(t) => Some(t),
                            None => return Err(ParseError {
                                message: format!("unknown transition: '{}'", tname),
                                span: self.current_span(),
                                hint: Some("valid: fade, dissolve, slide_left, etc.".to_string()),
                            }),
                        };
                    }
                    _ => {
                        // 直接位置词：居左/居中/居右/left/center/right，或自定义数字
                        if let Some(p) = Position::from_name(s) {
                            position = Some(p);
                            self.advance();
                        } else if let Ok(p) = s.parse::<f32>() {
                            position = Some(Position::Custom(p));
                            self.advance();
                        } else {
                            self.advance(); // 跳过无法识别的词
                        }
                    }
                }
            } else {
                break;
            }
        }

        self.consume_newline();
        Ok(Node::Direction {
            action: DirectionAction { kind, character, pose, position, transition },
            span,
        })
    }

    /// Parse character exit direction: `- Character [exits] [with transition]`
    /// The `-` prefix indicates character exit. Optional "exits"/"leaves" keyword
    /// is accepted for clarity but not required.
    fn parse_exit_direction(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        self.advance(); // -
        self.skip_newlines();

        let character = self.expect_ident("character name")?;

        let mut transition = None;

        // Parse optional keywords and transition
        while !self.is_at_end() && !matches!(self.peek().kind, TokenKind::Newline | TokenKind::Eof) {
            if let TokenKind::Ident(s) = &self.peek().kind {
                match s.as_str() {
                    "exits" | "exit" | "leaves" | "leave" => {
                        self.advance(); // optional keyword
                    }
                    "with" => {
                        self.advance();
                        let tname = self.expect_ident("transition name")?;
                        transition = match Transition::from_name(&tname) {
                            Some(t) => Some(t),
                            None => return Err(ParseError {
                                message: format!("unknown transition: '{}'", tname),
                                span: self.current_span(),
                                hint: Some("valid: fade, dissolve, slide_left, etc.".to_string()),
                            }),
                        };
                    }
                    _ => { self.advance(); }
                }
            } else {
                break;
            }
        }

        self.consume_newline();
        Ok(Node::Direction {
            action: DirectionAction {
                kind: DirectionKind::Exit,
                character,
                pose: None,
                position: None,
                transition,
            },
            span,
        })
    }

    fn parse_varop(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        self.advance(); // $
        let name = self.expect_ident("variable name")?;

        let op = match self.peek().kind.clone() {
            TokenKind::Assign => { self.advance(); VarOpKind::Assign }
            TokenKind::PlusEq => { self.advance(); VarOpKind::PlusEq }
            TokenKind::MinusEq => { self.advance(); VarOpKind::MinusEq }
            _ => return Err(ParseError {
                message: format!("expected =, +=, or -=, found {}", self.peek().kind),
                span: self.current_span(),
                hint: Some("use $var = value, $var += value, or $var -= value".to_string()),
            }),
        };

        let expr = self.parse_expr()?;
        self.consume_newline();
        Ok(Node::VarOp { name, op, expr, span })
    }

    fn parse_choice(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        self.advance(); // ?

        // Optional prompt string
        let prompt = if let TokenKind::String(s) = &self.peek().kind.clone() {
            let p = s.clone();
            self.advance();
            Some(p)
        } else {
            None
        };
        self.consume_newline();
        self.skip_newlines();

        let mut options = Vec::new();

        // Parse options: | "text" [if expr] body
        while self.check(&TokenKind::Pipe) {
            let opt_span = self.current_span();
            self.advance(); // |
            let text = self.expect_string("choice text")?;

            // Optional condition: `if $var > 5`
            let condition = if self.check(&TokenKind::If) {
                self.advance();
                Some(self.parse_expr()?)
            } else {
                None
            };

            self.consume_newline();
            self.skip_newlines();

            // Parse option body until next | or closing ?
            let mut body = Vec::new();
            while !self.is_at_end()
                && !self.check(&TokenKind::Pipe)
                && !self.check(&TokenKind::Question)
            {
                match self.parse_node() {
                    Ok(n) => body.push(n),
                    Err(e) => { self.errors.push(e); break; }
                }
                self.skip_newlines();
            }

            options.push(ChoiceOption { text, condition, body, span: opt_span });
        }

        // Closing ?
        self.expect(&TokenKind::Question, "? (to close choice block)")?;
        self.consume_newline();

        Ok(Node::Choice { prompt, options, span })
    }

    fn parse_conditional(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        self.advance(); // if
        let cond = self.parse_expr()?;
        self.consume_newline();
        self.skip_newlines();

        let mut body = Vec::new();
        while !self.is_at_end()
            && !self.check(&TokenKind::Else)
            && !self.check(&TokenKind::End)
        {
            match self.parse_node() {
                Ok(n) => body.push(n),
                Err(e) => { self.errors.push(e); break; }
            }
            self.skip_newlines();
        }

        let mut branches = vec![(cond, body)];
        let mut else_branch = None;

        // else branch
        if self.check(&TokenKind::Else) {
            self.advance();
            self.consume_newline();
            self.skip_newlines();
            let mut else_body = Vec::new();
            while !self.is_at_end() && !self.check(&TokenKind::End) {
                match self.parse_node() {
                    Ok(n) => else_body.push(n),
                    Err(e) => { self.errors.push(e); break; }
                }
                self.skip_newlines();
            }
            else_branch = Some(else_body);
        }

        self.expect(&TokenKind::End, "end")?;
        self.consume_newline();

        Ok(Node::Conditional { branches, else_branch, span })
    }

    fn parse_flow(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        self.advance(); // ->
        let target = self.expect_ident("section name")?;
        self.consume_newline();
        Ok(Node::Flow { target, span })
    }

    fn parse_visit(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        self.advance(); // =>
        let target = self.expect_ident("section name")?;
        self.consume_newline();
        Ok(Node::Visit { target, span })
    }

    fn parse_return(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        self.advance(); // <= (lexed as LtEq, disambiguated by parser at statement level)
        self.consume_newline();
        Ok(Node::Return { span })
    }

    fn parse_story_end(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        self.advance(); // ~~
        self.consume_newline();
        Ok(Node::StoryEnd { span })
    }

    fn parse_wait(&mut self) -> Result<Node, ParseError> {
        let span = self.current_span();
        self.advance(); // wait
        let seconds = match self.peek().kind.clone() {
            TokenKind::Integer(n) => { self.advance(); n as f64 }
            TokenKind::Float(n) => { self.advance(); n }
            _ => return Err(ParseError {
                message: "expected a number after 'wait'".to_string(),
                span: self.current_span(),
                hint: Some("use 'wait 2.0' for 2 seconds".to_string()),
            }),
        };
        self.consume_newline();
        Ok(Node::Wait { seconds, span })
    }

    // --- Expression parsing ---

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_and()?;
        while self.check(&TokenKind::Or) {
            let span = self.current_span();
            self.advance();
            let right = self.parse_and()?;
            left = Expr::Binary { op: BinOp::Or, left: Box::new(left), right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_not()?;
        while self.check(&TokenKind::And) {
            let span = self.current_span();
            self.advance();
            let right = self.parse_not()?;
            left = Expr::Binary { op: BinOp::And, left: Box::new(left), right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, ParseError> {
        if self.check(&TokenKind::Not) {
            let span = self.current_span();
            self.advance();
            let operand = self.parse_not()?;
            return Ok(Expr::Unary { op: UnOp::Not, operand: Box::new(operand), span });
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_additive()?;
        loop {
            let op = match &self.peek().kind {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::NotEq => BinOp::Neq,
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::LtEq => BinOp::LtEq,
                TokenKind::GtEq => BinOp::GtEq,
                _ => break,
            };
            let span = self.current_span();
            self.advance();
            let right = self.parse_additive()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match &self.peek().kind {
                TokenKind::Plus2 => BinOp::Add,
                TokenKind::Minus2 => BinOp::Sub,
                _ => break,
            };
            let span = self.current_span();
            self.advance();
            let right = self.parse_multiplicative()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match &self.peek().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                _ => break,
            };
            let span = self.current_span();
            self.advance();
            let right = self.parse_unary()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        if self.check(&TokenKind::Minus2) {
            let span = self.current_span();
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(Expr::Unary { op: UnOp::Neg, operand: Box::new(operand), span });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let span = self.current_span();
        match self.peek().kind.clone() {
            TokenKind::Integer(n) => { self.advance(); Ok(Expr::Int(n)) }
            TokenKind::Float(n) => { self.advance(); Ok(Expr::Float(n)) }
            TokenKind::String(s) => { self.advance(); Ok(Expr::Str(s)) }
            TokenKind::Dollar => {
                self.advance();
                let name = self.expect_ident("variable name")?;
                Ok(Expr::Var(name))
            }
            TokenKind::LParen => {
                self.advance();
                let e = self.parse_expr()?;
                self.expect(&TokenKind::RParen, ")")?;
                Ok(e)
            }
            _ => Err(ParseError {
                message: format!("expected expression, found {}", self.peek().kind),
                span,
                hint: Some("expressions: numbers, strings, $variables, or (expr)".to_string()),
            }),
        }
    }

    // --- Helpers ---

    fn peek(&self) -> &Token { &self.tokens[self.pos] }
    fn current_span(&self) -> LocSpan { self.peek().span }
    fn is_at_end(&self) -> bool { matches!(self.peek().kind, TokenKind::Eof) }

    fn check(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(kind)
    }

    fn advance(&mut self) -> &Token {
        let t = &self.tokens[self.pos];
        if !self.is_at_end() { self.pos += 1; }
        t
    }

    fn expect(&mut self, kind: &TokenKind, what: &str) -> Result<(), ParseError> {
        if self.check(kind) { self.advance(); Ok(()) }
        else { Err(ParseError {
            message: format!("expected {}, found {}", what, self.peek().kind),
            span: self.current_span(),
            hint: None,
        })}
    }

    fn expect_ident(&mut self, what: &str) -> Result<String, ParseError> {
        match &self.peek().kind.clone() {
            TokenKind::Ident(s) => { self.advance(); Ok(s.clone()) }
            _ => Err(ParseError {
                message: format!("expected {}, found {}", what, self.peek().kind),
                span: self.current_span(),
                hint: None,
            }),
        }
    }

    fn expect_string(&mut self, what: &str) -> Result<String, ParseError> {
        match &self.peek().kind.clone() {
            TokenKind::String(s) => { self.advance(); Ok(s.clone()) }
            _ => Err(ParseError {
                message: format!("expected {} (quoted string), found {}", what, self.peek().kind),
                span: self.current_span(),
                hint: Some("strings must be in double quotes".to_string()),
            }),
        }
    }

    fn consume_newline(&mut self) {
        if self.check(&TokenKind::Newline) { self.advance(); }
    }

    fn skip_newlines(&mut self) {
        while self.check(&TokenKind::Newline) { self.advance(); }
    }

    fn sync_to_section(&mut self) {
        while !self.is_at_end() && !self.check(&TokenKind::SectionMark) { self.advance(); }
    }

    fn sync_in_section(&mut self) {
        while !self.is_at_end()
            && !self.check(&TokenKind::SectionMark)
            && !self.check(&TokenKind::Question)
            && !self.check(&TokenKind::End)
        { self.advance(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse(src: &str) -> Program {
        let tokens = Lexer::new(src).tokenize().expect("lex failed");
        Parser::new(tokens).parse().expect("parse failed")
    }

    #[test]
    fn test_basic_section() {
        let p = parse("# Start\nAki: \"Hello!\"\n\"Narration.\"\n");
        assert_eq!(p.sections.len(), 1);
        assert_eq!(p.sections[0].name, "Start");
        assert_eq!(p.sections[0].nodes.len(), 2);
    }

    #[test]
    fn test_dialogue_with_pose() {
        let p = parse("# S\nAki (happy): \"Hi!\"\n");
        match &p.sections[0].nodes[0] {
            Node::Dialogue { speaker, pose, text, .. } => {
                assert_eq!(speaker, "Aki");
                assert_eq!(pose.as_deref(), Some("happy"));
                assert_eq!(text, "Hi!");
            }
            _ => panic!("expected Dialogue"),
        }
    }

    #[test]
    fn test_command() {
        let p = parse("# S\n@bg school with fade\n");
        match &p.sections[0].nodes[0] {
            Node::Command { cmd, args, transition, .. } => {
                assert_eq!(cmd, "bg");
                assert_eq!(args, &vec!["school"]);
                assert_eq!(*transition, Some(Transition::Fade));
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_direction() {
        let p = parse("# S\n+ Aki enters from left with fade\n");
        match &p.sections[0].nodes[0] {
            Node::Direction { action, .. } => {
                assert_eq!(action.character, "Aki");
                assert_eq!(action.kind, DirectionKind::Enter);
                assert_eq!(action.position, Some(Position::Left));
                assert_eq!(action.transition, Some(Transition::Fade));
            }
            _ => panic!("expected Direction"),
        }
    }

    #[test]
    fn test_exit_direction() {
        let p = parse("# S\n- Aki\n");
        match &p.sections[0].nodes[0] {
            Node::Direction { action, .. } => {
                assert_eq!(action.character, "Aki");
                assert_eq!(action.kind, DirectionKind::Exit);
                assert_eq!(action.transition, None);
            }
            _ => panic!("expected Direction (exit)"),
        }
    }

    #[test]
    fn test_exit_direction_with_transition() {
        let p = parse("# S\n- Aki with dissolve\n");
        match &p.sections[0].nodes[0] {
            Node::Direction { action, .. } => {
                assert_eq!(action.character, "Aki");
                assert_eq!(action.kind, DirectionKind::Exit);
                assert_eq!(action.transition, Some(Transition::Dissolve));
            }
            _ => panic!("expected Direction (exit with transition)"),
        }
    }

    #[test]
    fn test_choice() {
        let src = "# S\n? \"Pick one:\"\n| \"Option A\"\nAki: \"A\"\n| \"Option B\"\nAki: \"B\"\n?\n";
        let p = parse(src);
        match &p.sections[0].nodes[0] {
            Node::Choice { prompt, options, .. } => {
                assert_eq!(prompt.as_deref(), Some("Pick one:"));
                assert_eq!(options.len(), 2);
                assert_eq!(options[0].text, "Option A");
                assert_eq!(options[1].text, "Option B");
            }
            _ => panic!("expected Choice"),
        }
    }

    #[test]
    fn test_conditional() {
        let src = "# S\nif $affection > 5\nAki: \"High!\"\nelse\nAki: \"Low...\"\nend\n";
        let p = parse(src);
        match &p.sections[0].nodes[0] {
            Node::Conditional { branches, else_branch, .. } => {
                assert_eq!(branches.len(), 1);
                assert!(else_branch.is_some());
            }
            _ => panic!("expected Conditional"),
        }
    }

    #[test]
    fn test_flow_and_visit() {
        let p = parse("# S\n-> Next\n# T\n=> Sub\n<=\n");
        assert!(matches!(p.sections[0].nodes[0], Node::Flow { .. }));
        assert!(matches!(p.sections[1].nodes[0], Node::Visit { .. }));
        assert!(matches!(p.sections[1].nodes[1], Node::Return { .. }));
    }

    #[test]
    fn test_varop() {
        let p = parse("# S\n$affection = 10\n$score += 5\n");
        assert!(matches!(p.sections[0].nodes[0], Node::VarOp { op: VarOpKind::Assign, .. }));
        assert!(matches!(p.sections[0].nodes[1], Node::VarOp { op: VarOpKind::PlusEq, .. }));
    }

    #[test]
    fn test_direction_pose_and_chinese_position() {
        let p = parse("# S\n+ 心夏 (kokonabody1) 居中\n");
        match &p.sections[0].nodes[0] {
            Node::Direction { action, .. } => {
                assert_eq!(action.character, "心夏");
                assert_eq!(action.kind, DirectionKind::Enter);
                assert_eq!(action.pose.as_deref(), Some("kokonabody1"));
                assert_eq!(action.position, Some(Position::Center));
                assert_eq!(action.transition, None);
            }
            _ => panic!("expected Direction"),
        }
    }

    #[test]
    fn test_direction_chinese_position_only() {
        let p = parse("# S\n+ 心夏 居左\n");
        match &p.sections[0].nodes[0] {
            Node::Direction { action, .. } => {
                assert_eq!(action.character, "心夏");
                assert_eq!(action.pose, None);
                assert_eq!(action.position, Some(Position::Left));
            }
            _ => panic!("expected Direction"),
        }
    }

    #[test]
    fn test_direction_default_center_no_position() {
        let p = parse("# S\n+ 心夏\n");
        match &p.sections[0].nodes[0] {
            Node::Direction { action, .. } => {
                assert_eq!(action.character, "心夏");
                assert_eq!(action.position, None);
                assert_eq!(action.transition, None);
            }
            _ => panic!("expected Direction"),
        }
    }

    #[test]
    fn test_direction_pose_english_position_with_transition() {
        let p = parse("# S\n+ Aki (happy) left with fade\n");
        match &p.sections[0].nodes[0] {
            Node::Direction { action, .. } => {
                assert_eq!(action.character, "Aki");
                assert_eq!(action.pose.as_deref(), Some("happy"));
                assert_eq!(action.position, Some(Position::Left));
                assert_eq!(action.transition, Some(Transition::Fade));
            }
            _ => panic!("expected Direction"),
        }
    }
}
