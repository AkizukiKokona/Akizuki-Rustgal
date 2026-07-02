//! Runtime value types for the .akrs VM.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::Bool(_) => "bool",
            Value::Null => "null",
        }
    }
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Int(n) => *n != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Bool(b) => *b,
            Value::Null => false,
        }
    }
    pub fn as_int(&self) -> Result<i64, String> {
        match self {
            Value::Int(n) => Ok(*n),
            Value::Float(f) => Ok(*f as i64),
            Value::Bool(b) => Ok(*b as i64),
            _ => Err(format!("cannot convert {} to int", self.type_name())),
        }
    }
    pub fn as_float(&self) -> Result<f64, String> {
        match self {
            Value::Int(n) => Ok(*n as f64),
            Value::Float(f) => Ok(*f),
            _ => Err(format!("cannot convert {} to float", self.type_name())),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{}", n),
            Value::Float(n) => write!(f, "{:.1}", n),
            Value::Str(s) => write!(f, "{}", s),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Null => write!(f, "null"),
        }
    }
}

/// Compile-time type representation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Type {
    Int,
    Float,
    Str,
    Bool,
    Unknown,
}

impl Type {
    pub fn name(&self) -> &'static str {
        match self {
            Type::Int => "int", Type::Float => "float", Type::Str => "string",
            Type::Bool => "bool", Type::Unknown => "unknown",
        }
    }
    pub fn compatible(&self, other: &Type) -> bool {
        if *self == Type::Unknown || *other == Type::Unknown { return true; }
        if (*self == Type::Int && *other == Type::Float) || (*self == Type::Float && *other == Type::Int) { return true; }
        self == other
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}", self.name()) }
}
