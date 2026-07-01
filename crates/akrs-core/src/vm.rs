//! Virtual machine for executing .akrs scripts.
//!
//! Compiles AST to flat instructions, executes step-by-step,
//! emitting events for the renderer. Supports save/load via serialization.

use crate::ast::*;
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Events emitted by the VM for the engine/renderer to handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VmEvent {
    Dialogue { speaker: String, pose: Option<String>, text: String },
    Narration { text: String },
    Command { cmd: String, args: Vec<String>, transition: Option<Transition> },
    Direction { action: DirectionAction },
    Choice { prompt: Option<String>, options: Vec<ChoiceInfo> },
    Flow { target: String },
    Visit { target: String },
    Return,
    Wait { seconds: f64 },
    StoryEnd,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceInfo {
    pub text: String,
    pub available: bool,
}

#[derive(Debug, Clone)]
pub struct VmError {
    pub message: String,
}

/// Serializable VM state for save/load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmState {
    pub ip: usize,
    pub section: usize,
    pub variables: HashMap<String, Value>,
    pub call_stack: Vec<(usize, usize)>, // (section_idx, instr_idx)
}

/// Compiled instruction.
#[derive(Debug, Clone)]
enum Instr {
    Dialogue { speaker: String, pose: Option<String>, text: String },
    Narration { text: String },
    Command { cmd: String, args: Vec<String>, transition: Option<Transition> },
    Direction { action: DirectionAction },
    Choice { prompt: Option<String>, options: Vec<CompiledOption> },
    VarOp { name: String, op: VarOpKind, expr: Expr },
    JumpIfFalse { cond: Expr, target: usize },
    Jump { target: usize },
    /// One-way section jump (flow ->), no return
    SectionJump { target: usize },
    /// Visit with return (=>)
    VisitCall { target: usize },
    Return,
    Wait { seconds: f64 },
    StoryEnd,
}

#[derive(Debug, Clone)]
struct CompiledOption {
    text: String,
    condition: Option<Expr>,
    target: usize,
}

/// A compiled section.
#[derive(Debug, Clone)]
struct CompiledSection {
    name: String,
    instrs: Vec<Instr>,
}

/// The virtual machine.
pub struct Vm {
    sections: Vec<CompiledSection>,
    #[allow(dead_code)]
    section_map: HashMap<String, usize>,
    current_section: usize,
    ip: usize,
    variables: HashMap<String, Value>,
    call_stack: Vec<(usize, usize)>,
    pending_choice: Option<Vec<CompiledOption>>,
}

impl Vm {
    pub fn new(program: &Program) -> Self {
        let mut compiler = Compiler::new();
        let mut sections = Vec::new();
        let mut section_map = HashMap::new();

        // Build complete section map FIRST (for forward references)
        for (i, sec) in program.sections.iter().enumerate() {
            section_map.insert(sec.name.clone(), i);
        }

        // Now compile each section with the complete map available
        for sec in &program.sections {
            let instrs = compiler.compile_section(sec, &section_map);
            sections.push(CompiledSection { name: sec.name.clone(), instrs });
        }

        let entry = program.entry.as_deref().or_else(|| sections.first().map(|s| s.name.as_str()));
        let current_section = entry.and_then(|n| section_map.get(n)).copied().unwrap_or(0);

        Self {
            sections,
            section_map,
            current_section,
            ip: 0,
            variables: HashMap::new(),
            call_stack: Vec::new(),
            pending_choice: None,
        }
    }

    pub fn start(&mut self) -> Result<(), VmError> {
        if self.sections.is_empty() {
            return Err(VmError { message: "no sections to run".to_string() });
        }
        self.ip = 0;
        Ok(())
    }

    /// Advance past the current dialogue/narration (player clicked to continue).
    pub fn advance(&mut self) {
        self.ip += 1;
    }

    /// Execute one step, returning an event for the renderer.
    pub fn step(&mut self) -> Result<VmEvent, VmError> {
        loop {
            if self.current_section >= self.sections.len() {
                return Ok(VmEvent::StoryEnd);
            }
            let section = &self.sections[self.current_section];
            if self.ip >= section.instrs.len() {
                // End of section = story end (or return to caller)
                if let Some((sec, instr)) = self.call_stack.pop() {
                    self.current_section = sec;
                    self.ip = instr;
                    continue;
                }
                return Ok(VmEvent::StoryEnd);
            }

            let instr = section.instrs[self.ip].clone();
            match instr {
                Instr::Dialogue { speaker, pose, text } => {
                    return Ok(VmEvent::Dialogue { speaker, pose, text });
                }
                Instr::Narration { text } => {
                    return Ok(VmEvent::Narration { text });
                }
                Instr::Command { cmd, args, transition } => {
                    self.ip += 1;
                    return Ok(VmEvent::Command { cmd, args, transition });
                }
                Instr::Direction { action } => {
                    self.ip += 1;
                    return Ok(VmEvent::Direction { action });
                }
                Instr::Choice { prompt, options } => {
                    let infos: Vec<ChoiceInfo> = options.iter().map(|o| {
                        let available = o.condition.as_ref().is_none_or(|c| {
                            self.eval(c).map(|v| v.is_truthy()).unwrap_or(false)
                        });
                        ChoiceInfo { text: o.text.clone(), available }
                    }).collect();
                    self.pending_choice = Some(options);
                    return Ok(VmEvent::Choice { prompt, options: infos });
                }
                Instr::VarOp { name, op, expr } => {
                    let val = self.eval(&expr)?;
                    match op {
                        VarOpKind::Assign => { self.variables.insert(name, val); }
                        VarOpKind::PlusEq => {
                            let cur = self.variables.get(&name).cloned().unwrap_or(Value::Int(0));
                            let result = self.arith(&cur, &val, BinOp::Add)?;
                            self.variables.insert(name, result);
                        }
                        VarOpKind::MinusEq => {
                            let cur = self.variables.get(&name).cloned().unwrap_or(Value::Int(0));
                            let result = self.arith(&cur, &val, BinOp::Sub)?;
                            self.variables.insert(name, result);
                        }
                    }
                    self.ip += 1;
                }
                Instr::JumpIfFalse { cond, target } => {
                    let v = self.eval(&cond)?;
                    if v.is_truthy() { self.ip += 1; } else { self.ip = target; }
                }
                Instr::Jump { target } => { self.ip = target; }
                Instr::SectionJump { target } => {
                    self.current_section = target;
                    self.ip = 0;
                    // Clear call stack - flow is one-way
                    self.call_stack.clear();
                }
                Instr::VisitCall { target } => {
                    self.call_stack.push((self.current_section, self.ip + 1));
                    self.current_section = target;
                    self.ip = 0;
                }
                Instr::Return => {
                    if let Some((sec, instr)) = self.call_stack.pop() {
                        self.current_section = sec;
                        self.ip = instr;
                    } else {
                        return Err(VmError { message: "return without visit".to_string() });
                    }
                }
                Instr::Wait { seconds } => {
                    self.ip += 1;
                    return Ok(VmEvent::Wait { seconds });
                }
                Instr::StoryEnd => { return Ok(VmEvent::StoryEnd); }
            }
        }
    }

    /// Respond to a choice event.
    pub fn choose(&mut self, index: usize) -> Result<(), VmError> {
        let options = self.pending_choice.take()
            .ok_or_else(|| VmError { message: "no pending choice".to_string() })?;
        if index >= options.len() {
            return Err(VmError { message: format!("invalid choice index: {}", index) });
        }
        if let Some(cond) = &options[index].condition {
            if !self.eval(cond)?.is_truthy() {
                return Err(VmError { message: format!("choice {} not available", index) });
            }
        }
        self.ip = options[index].target;
        Ok(())
    }

    pub fn save_state(&self) -> VmState {
        VmState { ip: self.ip, section: self.current_section, variables: self.variables.clone(), call_stack: self.call_stack.clone() }
    }

    pub fn load_state(&mut self, state: VmState) {
        self.ip = state.ip;
        self.current_section = state.section;
        self.variables = state.variables;
        self.call_stack = state.call_stack;
        self.pending_choice = None;
    }

    pub fn get_variable(&self, name: &str) -> Option<&Value> { self.variables.get(name) }

    // --- Expression evaluation ---

    fn eval(&self, expr: &Expr) -> Result<Value, VmError> {
        match expr {
            Expr::Int(n) => Ok(Value::Int(*n)),
            Expr::Float(n) => Ok(Value::Float(*n)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Var(name) => self.variables.get(name).cloned().ok_or_else(|| VmError { message: format!("undefined variable: '{}'", name) }),
            Expr::Binary { op, left, right, .. } => {
                let lv = self.eval(left)?;
                let rv = self.eval(right)?;
                self.arith2(op, &lv, &rv)
            }
            Expr::Unary { op, operand, .. } => {
                let v = self.eval(operand)?;
                match op {
                    UnOp::Neg => match v {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        Value::Float(f) => Ok(Value::Float(-f)),
                        _ => Err(VmError { message: format!("cannot negate {}", v.type_name()) }),
                    },
                    UnOp::Not => Ok(Value::Bool(!v.is_truthy())),
                }
            }
        }
    }

    fn arith(&self, a: &Value, b: &Value, op: BinOp) -> Result<Value, VmError> { self.arith2(&op, a, b) }

    fn arith2(&self, op: &BinOp, a: &Value, b: &Value) -> Result<Value, VmError> {
        match op {
            BinOp::Add => match (a, b) {
                (Value::Int(x), Value::Int(y)) => Ok(Value::Int(x + y)),
                (Value::Float(x), Value::Float(y)) => Ok(Value::Float(x + y)),
                (Value::Str(x), Value::Str(y)) => Ok(Value::Str(format!("{}{}", x, y))),
                _ => self.numeric_op(a, b, |x, y| x + y),
            },
            BinOp::Sub => self.numeric_op(a, b, |x, y| x - y),
            BinOp::Mul => self.numeric_op(a, b, |x, y| x * y),
            BinOp::Div => match (a, b) {
                (Value::Int(x), Value::Int(y)) => if *y == 0 { Err(VmError { message: "division by zero".to_string() }) } else { Ok(Value::Int(x / y)) },
                _ => self.numeric_op(a, b, |x, y| x / y),
            },
            BinOp::Eq => Ok(Value::Bool(self.eq(a, b))),
            BinOp::Neq => Ok(Value::Bool(!self.eq(a, b))),
            BinOp::Lt => self.cmp(a, b).map(|c| Value::Bool(c < 0)),
            BinOp::Gt => self.cmp(a, b).map(|c| Value::Bool(c > 0)),
            BinOp::LtEq => self.cmp(a, b).map(|c| Value::Bool(c <= 0)),
            BinOp::GtEq => self.cmp(a, b).map(|c| Value::Bool(c >= 0)),
            BinOp::And => Ok(Value::Bool(a.is_truthy() && b.is_truthy())),
            BinOp::Or => Ok(Value::Bool(a.is_truthy() || b.is_truthy())),
        }
    }

    fn numeric_op(&self, a: &Value, b: &Value, f: impl Fn(f64, f64) -> f64) -> Result<Value, VmError> {
        let av = a.as_float().map_err(|e| VmError { message: e })?;
        let bv = b.as_float().map_err(|e| VmError { message: e })?;
        if matches!(a, Value::Int(_)) && matches!(b, Value::Int(_)) {
            Ok(Value::Int(f(av, bv) as i64))
        } else {
            Ok(Value::Float(f(av, bv)))
        }
    }

    fn eq(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Float(x), Value::Float(y)) => (x - y).abs() < f64::EPSILON,
            (Value::Int(x), Value::Float(y)) => (*x as f64 - y).abs() < f64::EPSILON,
            (Value::Float(x), Value::Int(y)) => (x - *y as f64).abs() < f64::EPSILON,
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            _ => false,
        }
    }

    fn cmp(&self, a: &Value, b: &Value) -> Result<i32, VmError> {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y) as i32),
            (Value::Float(x), Value::Float(y)) => Ok(x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal) as i32),
            (Value::Str(x), Value::Str(y)) => Ok(x.cmp(y) as i32),
            _ => Err(VmError { message: format!("cannot compare {} and {}", a.type_name(), b.type_name()) }),
        }
    }
}

/// Compiler: AST → flat instructions.
struct Compiler;

impl Compiler {
    fn new() -> Self { Self }

    fn compile_section(&mut self, section: &Section, section_map: &HashMap<String, usize>) -> Vec<Instr> {
        let mut instrs = Vec::new();
        self.compile_nodes(&section.nodes, &mut instrs, section_map);
        instrs.push(Instr::StoryEnd);
        instrs
    }

    fn compile_nodes(&mut self, nodes: &[Node], instrs: &mut Vec<Instr>, section_map: &HashMap<String, usize>) {
        for node in nodes {
            self.compile_node(node, instrs, section_map);
        }
    }

    fn compile_node(&mut self, node: &Node, instrs: &mut Vec<Instr>, section_map: &HashMap<String, usize>) {
        match node {
            Node::Dialogue { speaker, pose, text, .. } => {
                instrs.push(Instr::Dialogue { speaker: speaker.clone(), pose: pose.clone(), text: text.clone() });
            }
            Node::Narration { text, .. } => {
                instrs.push(Instr::Narration { text: text.clone() });
            }
            Node::Command { cmd, args, transition, .. } => {
                instrs.push(Instr::Command { cmd: cmd.clone(), args: args.clone(), transition: *transition });
            }
            Node::Direction { action, .. } => {
                instrs.push(Instr::Direction { action: action.clone() });
            }
            Node::VarOp { name, op, expr, .. } => {
                instrs.push(Instr::VarOp { name: name.clone(), op: *op, expr: expr.clone() });
            }
            Node::Choice { prompt, options, .. } => {
                let menu_idx = instrs.len();
                instrs.push(Instr::Choice { prompt: prompt.clone(), options: Vec::new() });

                let jump_after = instrs.len();
                instrs.push(Instr::Jump { target: 0 });

                let mut compiled_opts = Vec::new();
                let mut end_jumps = Vec::new();

                for opt in options {
                    let body_start = instrs.len();
                    compiled_opts.push(CompiledOption {
                        text: opt.text.clone(),
                        condition: opt.condition.clone(),
                        target: body_start,
                    });
                    self.compile_nodes(&opt.body, instrs, section_map);
                    let jmp = instrs.len();
                    instrs.push(Instr::Jump { target: 0 });
                    end_jumps.push(jmp);
                }

                let after = instrs.len();
                instrs[jump_after] = Instr::Jump { target: after };
                instrs[menu_idx] = Instr::Choice { prompt: prompt.clone(), options: compiled_opts };
                for j in end_jumps { instrs[j] = Instr::Jump { target: after }; }
            }
            Node::Conditional { branches, else_branch, .. } => {
                let mut end_jumps = Vec::new();
                for (cond, body) in branches {
                    let jif = instrs.len();
                    instrs.push(Instr::JumpIfFalse { cond: cond.clone(), target: 0 });
                    self.compile_nodes(body, instrs, section_map);
                    let jmp = instrs.len();
                    instrs.push(Instr::Jump { target: 0 });
                    end_jumps.push(jmp);
                    instrs[jif] = Instr::JumpIfFalse { cond: cond.clone(), target: instrs.len() };
                }
                if let Some(body) = else_branch {
                    self.compile_nodes(body, instrs, section_map);
                }
                let end = instrs.len();
                for j in end_jumps { instrs[j] = Instr::Jump { target: end }; }
            }
            Node::Flow { target, .. } => {
                if let Some(&idx) = section_map.get(target) {
                    instrs.push(Instr::SectionJump { target: idx });
                } else {
                    instrs.push(Instr::StoryEnd);
                }
            }
            Node::Visit { target, .. } => {
                if let Some(&idx) = section_map.get(target) {
                    instrs.push(Instr::VisitCall { target: idx });
                } else {
                    instrs.push(Instr::StoryEnd);
                }
            }
            Node::Return { .. } => { instrs.push(Instr::Return); }
            Node::Wait { seconds, .. } => { instrs.push(Instr::Wait { seconds: *seconds }); }
            Node::StoryEnd { .. } => { instrs.push(Instr::StoryEnd); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn run(src: &str) -> Vec<VmEvent> {
        let tokens = Lexer::new(src).tokenize().expect("lex");
        let prog = Parser::new(tokens).parse().expect("parse");
        let mut vm = Vm::new(&prog);
        vm.start().unwrap();
        let mut events = Vec::new();
        loop {
            match vm.step() {
                Ok(VmEvent::StoryEnd) => { events.push(VmEvent::StoryEnd); break; }
                Ok(VmEvent::Dialogue { .. }) | Ok(VmEvent::Narration { .. }) => {
                    let e = vm.step().unwrap();
                    events.push(e);
                    vm.advance();
                }
                Ok(VmEvent::Choice { .. }) => {
                    events.push(VmEvent::Choice { prompt: None, options: vec![] });
                    let _ = vm.choose(0);
                }
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }
        events
    }

    #[test]
    fn test_dialogue_execution() {
        let src = "# Start\nAki: \"Hello!\"\n\"Narration.\"\n";
        let events = run(src);
        assert!(events.iter().any(|e| matches!(e, VmEvent::Dialogue { text, .. } if text == "Hello!")));
        assert!(events.iter().any(|e| matches!(e, VmEvent::Narration { text } if text == "Narration.")));
    }

    #[test]
    fn test_flow_between_sections() {
        let src = "# Start\nAki: \"First.\"\n-> Second\n# Second\nAki: \"Second.\"\n";
        let events = run(src);
        assert!(events.iter().any(|e| matches!(e, VmEvent::Dialogue { text, .. } if text == "First.")));
        assert!(events.iter().any(|e| matches!(e, VmEvent::Dialogue { text, .. } if text == "Second.")));
    }

    #[test]
    fn test_choice_execution() {
        let src = "# S\n? \"Pick:\"\n| \"A\"\nAki: \"A!\"\n| \"B\"\nAki: \"B!\"\n?\n";
        let events = run(src);
        assert!(events.iter().any(|e| matches!(e, VmEvent::Dialogue { text, .. } if text == "A!")));
    }

    #[test]
    fn test_variables_and_conditionals() {
        let src = "# S\n$affection = 10\nif $affection > 5\nAki: \"High!\"\nelse\nAki: \"Low.\"\nend\n";
        let events = run(src);
        assert!(events.iter().any(|e| matches!(e, VmEvent::Dialogue { text, .. } if text == "High!")));
    }

    #[test]
    fn test_visit_and_return() {
        let src = "# Main\nAki: \"Before.\"\n=> Sub\nAki: \"After.\"\n# Sub\nAki: \"In sub.\"\n<=\n";
        let events = run(src);
        assert!(events.iter().any(|e| matches!(e, VmEvent::Dialogue { text, .. } if text == "Before.")));
        assert!(events.iter().any(|e| matches!(e, VmEvent::Dialogue { text, .. } if text == "In sub.")));
        assert!(events.iter().any(|e| matches!(e, VmEvent::Dialogue { text, .. } if text == "After.")));
    }

    #[test]
    fn test_save_load() {
        let src = "# S\n$x = 42\nAki: \"First.\"\nAki: \"Second.\"\n";
        let tokens = Lexer::new(src).tokenize().unwrap();
        let prog = Parser::new(tokens).parse().unwrap();
        let mut vm = Vm::new(&prog);
        vm.start().unwrap();
        // Step to first dialogue
        let _ = vm.step(); // VarOp (no event, continues)
        let _ = vm.step(); // Dialogue "First."
        let state = vm.save_state();
        vm.advance();
        let _ = vm.step(); // Dialogue "Second."
        // Load back
        vm.load_state(state);
        let e = vm.step().unwrap();
        assert!(matches!(e, VmEvent::Dialogue { text, .. } if text == "First."));
    }
}
