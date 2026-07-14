//! Ledger-scoped JSON value/parser.
//!
//! The durable run ledger must preserve Python-canonical numeric spellings in
//! payloads for byte-level interop. This crate keeps that behavior local by
//! storing JSON numbers as their source lexemes instead of enabling
//! `serde_json/arbitrary_precision`, which would alter normal
//! `serde_json::Value` behavior across config, policy, and CLI JSON paths.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Index;

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::value::RawValue;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Number(String);

impl Number {
    pub fn as_f64(&self) -> Option<f64> {
        self.0.parse().ok()
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.0.parse().ok()
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(map) => map.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(n) => n.as_i64(),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut BTreeMap<String, Value>> {
        match self {
            Value::Object(map) => Some(map),
            _ => None,
        }
    }
}

impl From<serde_json::Value> for Value {
    fn from(value: serde_json::Value) -> Value {
        match value {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => Value::Number(Number(n.to_string())),
            serde_json::Value::String(s) => Value::String(s),
            serde_json::Value::Array(items) => {
                Value::Array(items.into_iter().map(Value::from).collect())
            }
            serde_json::Value::Object(map) => {
                Value::Object(map.into_iter().map(|(k, v)| (k, Value::from(v))).collect())
            }
        }
    }
}

impl From<String> for Value {
    fn from(value: String) -> Value {
        Value::String(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Value {
        Value::String(value.to_owned())
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Value {
        Value::Bool(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Value {
        Value::Number(Number(value.to_string()))
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Value {
        Value::Number(Number(value.to_string()))
    }
}

impl From<usize> for Value {
    fn from(value: usize) -> Value {
        Value::Number(Number(value.to_string()))
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Value {
        Value::Number(Number(value.to_string()))
    }
}

impl PartialEq<&str> for Value {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == Some(*other)
    }
}

impl PartialEq<i32> for Value {
    fn eq(&self, other: &i32) -> bool {
        self.as_i64() == Some(i64::from(*other))
    }
}

impl PartialEq<i64> for Value {
    fn eq(&self, other: &i64) -> bool {
        self.as_i64() == Some(*other)
    }
}

impl PartialEq<usize> for Value {
    fn eq(&self, other: &usize) -> bool {
        self.as_i64().and_then(|v| usize::try_from(v).ok()) == Some(*other)
    }
}

static NULL: Value = Value::Null;

impl Index<&str> for Value {
    type Output = Value;

    fn index(&self, index: &str) -> &Self::Output {
        self.get(index).unwrap_or(&NULL)
    }
}

impl Index<usize> for Value {
    type Output = Value;

    fn index(&self, index: usize) -> &Self::Output {
        match self {
            Value::Array(items) => items.get(index).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match serde_json::to_string(self) {
            Ok(text) => f.write_str(&text),
            Err(_) => Err(fmt::Error),
        }
    }
}

impl Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Value::Null => serializer.serialize_none(),
            Value::Bool(b) => serializer.serialize_bool(*b),
            Value::Number(n) => RawValue::from_string(n.to_string())
                .map_err(serde::ser::Error::custom)?
                .serialize(serializer),
            Value::String(s) => serializer.serialize_str(s),
            Value::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Value::Object(map) => {
                let mut obj = serializer.serialize_map(Some(map.len()))?;
                for (key, value) in map {
                    obj.serialize_entry(key, value)?;
                }
                obj.end()
            }
        }
    }
}

pub fn from_str(text: &str) -> Result<Value, serde_json::Error> {
    Parser::new(text).parse()
}

#[macro_export]
macro_rules! json {
    ($($json:tt)+) => {{
        $crate::Value::from(::serde_json::json!($($json)+))
    }};
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Parser<'a> {
        Parser { text, pos: 0 }
    }

    fn parse(mut self) -> Result<Value, serde_json::Error> {
        let value = self.value()?;
        self.ws();
        if self.pos == self.text.len() {
            Ok(value)
        } else {
            self.error("trailing characters after JSON value")
        }
    }

    fn error<T>(&self, message: &str) -> Result<T, serde_json::Error> {
        Err(<serde_json::Error as serde::de::Error>::custom(message))
    }

    fn value(&mut self) -> Result<Value, serde_json::Error> {
        self.ws();
        match self.peek() {
            Some(b'n') => self.literal(b"null", Value::Null),
            Some(b't') => self.literal(b"true", Value::Bool(true)),
            Some(b'f') => self.literal(b"false", Value::Bool(false)),
            Some(b'\"') => self.string().map(Value::String),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => self.error("invalid JSON value"),
        }
    }

    fn literal(&mut self, literal: &[u8], value: Value) -> Result<Value, serde_json::Error> {
        if self.text.as_bytes()[self.pos..].starts_with(literal) {
            self.pos += literal.len();
            Ok(value)
        } else {
            self.error("invalid JSON literal")
        }
    }

    fn array(&mut self) -> Result<Value, serde_json::Error> {
        self.bump();
        let mut items = Vec::new();
        self.ws();
        if self.eat(b']') {
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            if self.eat(b']') {
                return Ok(Value::Array(items));
            }
            if !self.eat(b',') {
                return self.error("invalid JSON");
            }
        }
    }

    fn object(&mut self) -> Result<Value, serde_json::Error> {
        self.bump();
        let mut map = BTreeMap::new();
        self.ws();
        if self.eat(b'}') {
            return Ok(Value::Object(map));
        }
        loop {
            self.ws();
            if self.peek() != Some(b'\"') {
                return self.error("invalid JSON");
            }
            let key = self.string()?;
            self.ws();
            if !self.eat(b':') {
                return self.error("invalid JSON");
            }
            let value = self.value()?;
            map.insert(key, value);
            self.ws();
            if self.eat(b'}') {
                return Ok(Value::Object(map));
            }
            if !self.eat(b',') {
                return self.error("invalid JSON");
            }
        }
    }

    fn string(&mut self) -> Result<String, serde_json::Error> {
        let start = self.pos;
        self.bump();
        while let Some(byte) = self.peek() {
            match byte {
                b'\\' => {
                    self.bump();
                    if self.peek().is_some() {
                        self.bump();
                    }
                }
                b'\"' => {
                    self.bump();
                    return serde_json::from_str(&self.text[start..self.pos]);
                }
                _ => self.bump(),
            }
        }
        self.error("unterminated JSON string")
    }

    fn number(&mut self) -> Result<Value, serde_json::Error> {
        let start = self.pos;
        self.eat(b'-');
        match self.peek() {
            Some(b'0') => {
                self.bump();
            }
            Some(b'1'..=b'9') => {
                self.bump();
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.bump();
                }
            }
            _ => return self.error("invalid JSON number"),
        }
        if self.eat(b'.') {
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return self.error("invalid JSON");
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.bump();
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.bump();
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.bump();
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return self.error("invalid JSON");
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.bump();
            }
        }
        Ok(Value::Number(Number(self.text[start..self.pos].to_owned())))
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.bump();
        }
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn bump(&mut self) {
        self.pos += 1;
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.bump();
            true
        } else {
            false
        }
    }
}
