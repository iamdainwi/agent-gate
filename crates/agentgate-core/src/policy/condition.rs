use anyhow::{bail, Result};
use chrono::{Timelike, Utc};
use regex::Regex;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
}

#[derive(Debug, Clone)]
pub enum Expr {
    ArgumentFieldMatches { field: String, re: Regex },
    ArgumentsContainPattern { re: Regex },
    TimeHour { op: CmpOp, value: u32 },
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
}

pub struct EvalCtx<'a> {
    pub arguments: Option<&'a Value>,
    pub now: chrono::DateTime<Utc>,
}

impl Expr {
    pub fn parse(input: &str) -> Result<Self> {
        let tokens = tokenize(input)?;
        let mut parser = Parser { tokens, pos: 0 };
        let expr = parser.parse_or()?;
        if parser.peek() != Tok::Eof {
            bail!("Unexpected trailing tokens in condition");
        }
        Ok(expr)
    }

    pub fn evaluate(&self, ctx: &EvalCtx) -> bool {
        match self {
            Expr::ArgumentFieldMatches { field, re } => {
                let s = ctx
                    .arguments
                    .and_then(|a| a.get(field))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                re.is_match(s)
            }
            Expr::ArgumentsContainPattern { re } => {
                let s = ctx.arguments.map(|v| v.to_string()).unwrap_or_default();
                re.is_match(&s)
            }
            Expr::TimeHour { op, value } => {
                let h = ctx.now.hour();
                match op {
                    CmpOp::Lt => h < *value,
                    CmpOp::Gt => h > *value,
                    CmpOp::Le => h <= *value,
                    CmpOp::Ge => h >= *value,
                    CmpOp::Eq => h == *value,
                }
            }
            Expr::And(a, b) => a.evaluate(ctx) && b.evaluate(ctx),
            Expr::Or(a, b) => a.evaluate(ctx) || b.evaluate(ctx),
            Expr::Not(e) => !e.evaluate(ctx),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Kw {
    Arguments,
    Time,
    Hour,
    Matches,
    ContainsPattern,
    Or,
    And,
    Not,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Kw(Kw),
    Ident(String),
    Str(String),
    Num(u32),
    Dot,
    Cmp(CmpOp),
    LParen,
    RParen,
    Eof,
}

fn tokenize(input: &str) -> Result<Vec<Tok>> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            ' ' | '\t' | '\n' | '\r' => {
                i += 1;
            }
            '.' => {
                tokens.push(Tok::Dot);
                i += 1;
            }
            '(' => {
                tokens.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Tok::RParen);
                i += 1;
            }
            '<' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push(Tok::Cmp(CmpOp::Le));
                    i += 2;
                } else {
                    tokens.push(Tok::Cmp(CmpOp::Lt));
                    i += 1;
                }
            }
            '>' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push(Tok::Cmp(CmpOp::Ge));
                    i += 2;
                } else {
                    tokens.push(Tok::Cmp(CmpOp::Gt));
                    i += 1;
                }
            }
            '=' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push(Tok::Cmp(CmpOp::Eq));
                    i += 2;
                } else {
                    bail!("Expected '==' at position {i}");
                }
            }
            '\'' => {
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                if i >= chars.len() {
                    bail!("Unterminated string literal in condition");
                }
                let s: String = chars[start..i].iter().collect();
                tokens.push(Tok::Str(s));
                i += 1;
            }
            c if c.is_ascii_digit() => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let n: u32 = chars[start..i].iter().collect::<String>().parse()?;
                tokens.push(Tok::Num(n));
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let tok = match word.as_str() {
                    "arguments" => Tok::Kw(Kw::Arguments),
                    "time" => Tok::Kw(Kw::Time),
                    "hour" => Tok::Kw(Kw::Hour),
                    "matches" => Tok::Kw(Kw::Matches),
                    "contains_pattern" => Tok::Kw(Kw::ContainsPattern),
                    "or" => Tok::Kw(Kw::Or),
                    "and" => Tok::Kw(Kw::And),
                    "not" => Tok::Kw(Kw::Not),
                    _ => Tok::Ident(word),
                };
                tokens.push(tok);
            }
            c => bail!("Unexpected character '{c}' at position {i}"),
        }
    }

    tokens.push(Tok::Eof);
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Tok {
        self.tokens.get(self.pos).cloned().unwrap_or(Tok::Eof)
    }

    fn advance(&mut self) -> Tok {
        let tok = self.tokens.get(self.pos).cloned().unwrap_or(Tok::Eof);
        self.pos += 1;
        tok
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        while self.peek() == Tok::Kw(Kw::Or) {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_not()?;
        while self.peek() == Tok::Kw(Kw::And) {
            self.advance();
            let right = self.parse_not()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr> {
        if self.peek() == Tok::Kw(Kw::Not) {
            self.advance();
            let inner = self.parse_not()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        if self.peek() == Tok::LParen {
            self.advance();
            let expr = self.parse_or()?;
            if self.advance() != Tok::RParen {
                bail!("Expected closing ')'");
            }
            return Ok(expr);
        }
        self.parse_atom()
    }

    fn parse_atom(&mut self) -> Result<Expr> {
        match self.peek() {
            Tok::Kw(Kw::Arguments) => {
                self.advance();
                match self.peek() {
                    Tok::Dot => {
                        self.advance();
                        let field = match self.advance() {
                            Tok::Ident(s) => s,
                            t => bail!("Expected field name after 'arguments.', got {t:?}"),
                        };
                        if self.advance() != Tok::Kw(Kw::Matches) {
                            bail!("Expected 'matches' after field path");
                        }
                        let pattern = match self.advance() {
                            Tok::Str(s) => s,
                            t => bail!("Expected quoted pattern after 'matches', got {t:?}"),
                        };
                        let re = Regex::new(&pattern)
                            .map_err(|e| anyhow::anyhow!("Invalid regex '{pattern}': {e}"))?;
                        Ok(Expr::ArgumentFieldMatches { field, re })
                    }
                    Tok::Kw(Kw::ContainsPattern) => {
                        self.advance();
                        let pattern = match self.advance() {
                            Tok::Str(s) => s,
                            t => {
                                bail!("Expected quoted pattern after 'contains_pattern', got {t:?}")
                            }
                        };
                        let re = Regex::new(&pattern)
                            .map_err(|e| anyhow::anyhow!("Invalid regex '{pattern}': {e}"))?;
                        Ok(Expr::ArgumentsContainPattern { re })
                    }
                    t => bail!("Expected '.' or 'contains_pattern' after 'arguments', got {t:?}"),
                }
            }
            Tok::Kw(Kw::Time) => {
                self.advance();
                if self.advance() != Tok::Dot {
                    bail!("Expected '.' after 'time'");
                }
                if self.advance() != Tok::Kw(Kw::Hour) {
                    bail!("Expected 'hour' after 'time.'");
                }
                let op = match self.advance() {
                    Tok::Cmp(op) => op,
                    t => bail!("Expected comparison operator after 'time.hour', got {t:?}"),
                };
                let value = match self.advance() {
                    Tok::Num(n) => n,
                    t => bail!("Expected integer after operator, got {t:?}"),
                };
                Ok(Expr::TimeHour { op, value })
            }
            t => bail!("Unexpected token in condition expression: {t:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx_with_args(args: Value) -> EvalCtx<'static> {
        // SAFETY: the Value is owned by the test and lives for the duration
        // of the expression evaluation within the test.
        let args = Box::leak(Box::new(args));
        EvalCtx {
            arguments: Some(args),
            now: Utc::now(),
        }
    }

    #[test]
    fn field_matches_regex() {
        let expr = Expr::parse("arguments.command matches '(rm -rf)'").unwrap();
        let ctx = ctx_with_args(json!({ "command": "rm -rf /" }));
        assert!(expr.evaluate(&ctx));

        let ctx2 = ctx_with_args(json!({ "command": "ls -la" }));
        assert!(!expr.evaluate(&ctx2));
    }

    #[test]
    fn args_contains_pattern() {
        let expr = Expr::parse("arguments contains_pattern '(sk-[a-zA-Z0-9]+)'").unwrap();
        let ctx = ctx_with_args(json!({ "key": "sk-abc123" }));
        assert!(expr.evaluate(&ctx));
    }

    #[test]
    fn or_combinator() {
        let expr =
            Expr::parse("arguments.cmd matches '(rm)' or arguments.cmd matches '(DROP)'").unwrap();
        let ctx = ctx_with_args(json!({ "cmd": "DROP TABLE users" }));
        assert!(expr.evaluate(&ctx));
    }

    #[test]
    fn not_combinator() {
        let expr = Expr::parse("not arguments.cmd matches '(safe)'").unwrap();
        let ctx = ctx_with_args(json!({ "cmd": "safe_command" }));
        assert!(!expr.evaluate(&ctx));
    }
}
