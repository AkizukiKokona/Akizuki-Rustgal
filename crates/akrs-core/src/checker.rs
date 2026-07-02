//! Semantic checker: validates section references, variable types, and resource references.
//!
//! This runs at both compile-time (via proc macro) and runtime (hot reload),
//! using the exact same code for consistent behavior.

use crate::ast::*;
use crate::token::LocSpan;
use crate::value::Type;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct CheckError {
    pub message: String,
    pub span: LocSpan,
    pub hint: Option<String>,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

pub struct Checker {
    errors: Vec<CheckError>,
    sections: HashMap<String, LocSpan>,
    variables: HashMap<String, Type>,
    /// Declared resource paths for reference checking
    resources: Vec<String>,
}

impl Checker {
    pub fn new() -> Self {
        Self { errors: Vec::new(), sections: HashMap::new(), variables: HashMap::new(), resources: Vec::new() }
    }

    /// Set known resources (for compile-time resource reference checking).
    pub fn with_resources(mut self, resources: Vec<String>) -> Self {
        self.resources = resources;
        self
    }

    pub fn check(mut self, program: &Program) -> Vec<CheckError> {
        // Pass 1: collect section names
        for section in &program.sections {
            if let Some(prev) = self.sections.get(&section.name) {
                self.errors.push(CheckError {
                    message: format!("duplicate section name: '{}'", section.name),
                    span: section.span,
                    hint: Some("each section must have a unique name".to_string()),
                    severity: Severity::Error,
                });
                self.errors.push(CheckError {
                    message: format!("'{}' was first defined here", section.name),
                    span: *prev,
                    hint: None,
                    severity: Severity::Note,
                });
            } else {
                self.sections.insert(section.name.clone(), section.span);
            }
        }

        // Check entry exists
        if let Some(entry) = &program.entry {
            if !self.sections.contains_key(entry) {
                self.errors.push(CheckError {
                    message: format!("entry section '{}' not found", entry),
                    span: LocSpan::dummy(),
                    hint: None,
                    severity: Severity::Error,
                });
            }
        } else if self.sections.is_empty() {
            self.errors.push(CheckError {
                    message: "no sections defined".to_string(),
                    span: LocSpan::dummy(),
                    hint: Some("add at least one section: # Start".to_string()),
                    severity: Severity::Warning,
                });
        }

        // Pass 2: check each section (含同场角色数检查)
        for section in &program.sections {
            let mut on_stage: Vec<String> = Vec::new();
            self.check_nodes(&section.nodes, &mut on_stage);
        }

        self.errors
    }

    fn check_nodes(&mut self, nodes: &[Node], on_stage: &mut Vec<String>) {
        for node in nodes {
            self.check_node(node, on_stage);
        }
    }

    fn check_node(&mut self, node: &Node, on_stage: &mut Vec<String>) {
        match node {
            Node::Dialogue { .. } | Node::Narration { .. } => {}
            Node::Command { cmd, args, .. } => {
                // Check resource references if resources are declared
                if !self.resources.is_empty()
                    && matches!(cmd.as_str(), "bg" | "music" | "sound")
                    && let Some(res) = args.first()
                    && !self.resources.iter().any(|r| r.contains(res) || r == res)
                {
                    self.errors.push(CheckError {
                        message: format!("resource '{}' not found", res),
                        span: node_span(node),
                        hint: Some(format!("add '{}' to your assets directory", res)),
                        severity: Severity::Warning,
                    });
                }
            }
            Node::Direction { action, span } => {
                match action.kind {
                    DirectionKind::Enter => {
                        // 重入同名角色不新增
                        if !on_stage.iter().any(|n| n == &action.character) {
                            on_stage.push(action.character.clone());
                            if on_stage.len() > 2 {
                                self.errors.push(CheckError {
                                    message: format!(
                                        "too many characters on stage: '{}' would be the 3rd (limit is 2)",
                                        action.character
                                    ),
                                    span: *span,
                                    hint: Some("exit a character with `- Name` before entering a new one".to_string()),
                                    severity: Severity::Error,
                                });
                            }
                        }
                    }
                    DirectionKind::Exit => {
                        on_stage.retain(|n| n != &action.character);
                    }
                }
            }
            Node::VarOp { name, op, expr, span } => {
                let expr_ty = self.infer_expr(expr);
                match op {
                    VarOpKind::Assign => {
                        self.variables.insert(name.clone(), expr_ty);
                    }
                    VarOpKind::PlusEq | VarOpKind::MinusEq => {
                        if let Some(existing) = self.variables.get(name) {
                            if !existing.compatible(&expr_ty) {
                                self.errors.push(CheckError {
                                    message: format!("type mismatch: cannot apply {} to {} ({}) and {}", op_name(op), name, existing, expr_ty),
                                    span: *span,
                                    hint: None,
                                    severity: Severity::Error,
                                });
                            }
                        } else {
                            self.errors.push(CheckError {
                                message: format!("undefined variable: '{}'", name),
                                span: *span,
                                hint: Some(format!("declare with ${} = ... first", name)),
                                severity: Severity::Error,
                            });
                        }
                    }
                }
            }
            Node::Choice { options, .. } => {
                for opt in options {
                    if let Some(cond) = &opt.condition {
                        let cond_ty = self.infer_expr(cond);
                        if cond_ty != Type::Bool && cond_ty != Type::Unknown {
                            self.errors.push(CheckError {
                                message: format!("choice condition must be bool, found {}", cond_ty),
                                span: opt.span,
                                hint: None,
                                severity: Severity::Error,
                            });
                        }
                    }
                    // 每个选项分支独立模拟同场角色（互斥分支不累加）
                    let mut branch_stage = on_stage.clone();
                    self.check_nodes(&opt.body, &mut branch_stage);
                }
            }
            Node::Conditional { branches, else_branch, .. } => {
                for (cond, body) in branches {
                    let cond_ty = self.infer_expr(cond);
                    if cond_ty != Type::Bool && cond_ty != Type::Unknown {
                        self.errors.push(CheckError {
                            message: format!("if condition must be bool, found {}", cond_ty),
                            span: cond.span(),
                            hint: None,
                            severity: Severity::Error,
                        });
                    }
                    // 每个条件分支独立模拟同场角色
                    let mut branch_stage = on_stage.clone();
                    self.check_nodes(body, &mut branch_stage);
                }
                if let Some(body) = else_branch {
                    let mut branch_stage = on_stage.clone();
                    self.check_nodes(body, &mut branch_stage);
                }
            }
            Node::Flow { target, span } => {
                if !self.sections.contains_key(target) {
                    self.errors.push(CheckError {
                        message: format!("undefined section: '{}'", target),
                        span: *span,
                        hint: Some(self.suggest_section(target)),
                        severity: Severity::Error,
                    });
                }
            }
            Node::Visit { target, span } => {
                if !self.sections.contains_key(target) {
                    self.errors.push(CheckError {
                        message: format!("undefined section: '{}'", target),
                        span: *span,
                        hint: Some(self.suggest_section(target)),
                        severity: Severity::Error,
                    });
                }
            }
            Node::Return { .. } => {}
            Node::Wait { .. } => {}
            Node::StoryEnd { .. } => {}
        }
    }

    fn infer_expr(&mut self, expr: &Expr) -> Type {
        match expr {
            Expr::Int(_) => Type::Int,
            Expr::Float(_) => Type::Float,
            Expr::Str(_) => Type::Str,
            Expr::Bool(_) => Type::Bool,
            Expr::Var(name) => {
                self.variables.get(name).copied().unwrap_or_else(|| {
                    self.errors.push(CheckError {
                        message: format!("undefined variable: '{}'", name),
                        span: expr.span(),
                        hint: Some(format!("declare with ${} = ... first", name)),
                        severity: Severity::Error,
                    });
                    Type::Unknown
                })
            }
            Expr::Binary { op, left, right, span } => {
                let lt = self.infer_expr(left);
                let rt = self.infer_expr(right);
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                        if lt == Type::Str && rt == Type::Str && *op == BinOp::Add { Type::Str }
                        else if (lt == Type::Int || lt == Type::Float) && (rt == Type::Int || rt == Type::Float) {
                            if lt == Type::Float || rt == Type::Float { Type::Float } else { Type::Int }
                        } else if lt == Type::Unknown || rt == Type::Unknown { Type::Unknown }
                        else {
                            self.errors.push(CheckError {
                                message: format!("cannot apply {:?} to {} and {}", op, lt, rt),
                                span: *span, hint: None, severity: Severity::Error,
                            });
                            Type::Unknown
                        }
                    }
                    BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => Type::Bool,
                    BinOp::And | BinOp::Or => {
                        if lt != Type::Bool && lt != Type::Unknown {
                            self.errors.push(CheckError {
                                message: format!("expected bool, found {}", lt),
                                span: left.span(), hint: None, severity: Severity::Error,
                            });
                        }
                        Type::Bool
                    }
                }
            }
            Expr::Unary { op, operand, span } => {
                let ot = self.infer_expr(operand);
                match op {
                    UnOp::Neg => {
                        if ot != Type::Int && ot != Type::Float && ot != Type::Unknown {
                            self.errors.push(CheckError {
                                message: format!("cannot negate {}", ot),
                                span: *span, hint: None, severity: Severity::Error,
                            });
                        }
                        ot
                    }
                    UnOp::Not => Type::Bool,
                }
            }
        }
    }

    fn suggest_section(&self, target: &str) -> String {
        let mut best: Option<(usize, &String)> = None;
        for name in self.sections.keys() {
            let dist = levenshtein(target, name);
            if dist <= 3 && best.is_none_or(|(d, _)| dist < d) {
                best = Some((dist, name));
            }
        }
        match best {
            Some((_, name)) => format!("did you mean '{}'?", name),
            None => "define this section with # Name".to_string(),
        }
    }
}

impl Default for Checker {
    fn default() -> Self { Self::new() }
}

fn node_span(node: &Node) -> LocSpan {
    match node {
        Node::Dialogue { span, .. } | Node::Narration { span, .. } | Node::Command { span, .. }
        | Node::Direction { span, .. } | Node::VarOp { span, .. } | Node::Choice { span, .. }
        | Node::Conditional { span, .. } | Node::Flow { span, .. } | Node::Visit { span, .. }
        | Node::Return { span, .. } | Node::Wait { span, .. } | Node::StoryEnd { span, .. } => *span,
    }
}

fn op_name(op: &VarOpKind) -> &'static str {
    match op { VarOpKind::Assign => "=", VarOpKind::PlusEq => "+=", VarOpKind::MinusEq => "-=" }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut dp = vec![vec![0; b.len() + 1]; a.len() + 1];
    for i in 0..=a.len() { dp[i][0] = i; }
    for j in 0..=b.len() { dp[0][j] = j; }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = if a[i-1] == b[j-1] { 0 } else { 1 };
            dp[i][j] = (dp[i-1][j] + 1).min(dp[i][j-1] + 1).min(dp[i-1][j-1] + cost);
        }
    }
    dp[a.len()][b.len()]
}
