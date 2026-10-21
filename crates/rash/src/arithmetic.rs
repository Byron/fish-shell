// Copyright (C) 1987-2005 Free Software Foundation, Inc.
// Rust adaptation: Copyright (C) 2026 fish-shell contributors.
// Derived from GNU Bash 3.2 expr.c; GPL-2.0-or-later. See ../COPYING.

//! Integer arithmetic with Bash 3.2's precedence and recursive variables.
//! Skipped branches suppress assignments but retain arithmetic errors, as in 3.2.

use crate::{Error, Result};

pub trait Variables {
    fn get(&mut self, name: &[u8]) -> Result<Option<Vec<u8>>>;
    fn set(&mut self, name: &[u8], value: Vec<u8>) -> Result<()>;
}

#[derive(Debug)]
enum Expr {
    Number(i64),
    Variable(Vec<u8>),
    Group(Box<Expr>),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    Assign(&'static str, Box<Expr>, Box<Expr>),
    Increment(Box<Expr>, i64, bool),
}

struct Parser<'a> {
    input: &'a [u8],
    at: usize,
    depth: usize,
    nodes: usize,
}

impl Parser<'_> {
    fn prefix_increment(&mut self, operator: &[u8]) -> bool {
        self.space();
        if !self.input[self.at..].starts_with(operator) {
            return false;
        }
        let name = self.input[self.at + operator.len()..]
            .iter()
            .find(|c| !c.is_ascii_whitespace());
        if name.is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_') {
            self.at += operator.len();
            true
        } else {
            false
        }
    }
    fn space(&mut self) {
        while self.input.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn consume(&mut self, value: &[u8]) -> bool {
        self.space();
        if self.input[self.at..].starts_with(value) {
            self.at += value.len();
            true
        } else {
            false
        }
    }

    fn expression(&mut self, minimum: u8) -> Result<Expr> {
        // shortcut: bound the expression tree until arithmetic evaluation and drop are iterative.
        if self.nodes >= 1024 {
            return Err(Error::new("arithmetic: expression size limit exceeded"));
        }
        self.nodes += 1;
        if self.depth >= 128 {
            return Err(Error::new("arithmetic: expression nesting limit exceeded"));
        }
        self.depth += 1;
        let result = self.parse_expression(minimum);
        self.depth -= 1;
        result
    }

    fn parse_expression(&mut self, minimum: u8) -> Result<Expr> {
        self.space();
        let mut left = if self.prefix_increment(b"++") {
            Expr::Increment(Box::new(self.expression(15)?), 1, true)
        } else if self.prefix_increment(b"--") {
            Expr::Increment(Box::new(self.expression(15)?), -1, true)
        } else if self.consume(b"+") {
            Expr::Unary("+", Box::new(self.expression(15)?))
        } else if self.consume(b"-") {
            Expr::Unary("-", Box::new(self.expression(15)?))
        } else if self.consume(b"!") {
            Expr::Unary("!", Box::new(self.expression(15)?))
        } else if self.consume(b"~") {
            Expr::Unary("~", Box::new(self.expression(15)?))
        } else if self.consume(b"(") {
            let value = self.expression(1)?;
            if !self.consume(b")") {
                return Err(Error::new("arithmetic: missing closing parenthesis"));
            }
            Expr::Group(Box::new(value))
        } else {
            self.atom()?
        };
        loop {
            self.space();
            if minimum <= 16 && self.consume(b"++") {
                left = Expr::Increment(Box::new(left), 1, false);
                continue;
            }
            if minimum <= 16 && self.consume(b"--") {
                left = Expr::Increment(Box::new(left), -1, false);
                continue;
            }
            if minimum <= 3 && self.consume(b"?") {
                let yes = self.expression(1)?;
                if !self.consume(b":") {
                    return Err(Error::new("arithmetic: missing ':' in conditional"));
                }
                let no = self.expression(3)?;
                left = Expr::Conditional(Box::new(left), Box::new(yes), Box::new(no));
                continue;
            }
            let Some((operator, precedence, right_associative)) = self.operator() else {
                break;
            };
            if precedence < minimum {
                break;
            }
            self.at += operator.len();
            let right = self.expression(precedence + u8::from(!right_associative))?;
            left = if precedence == 2 {
                Expr::Assign(operator, Box::new(left), Box::new(right))
            } else {
                Expr::Binary(operator, Box::new(left), Box::new(right))
            };
        }
        Ok(left)
    }

    fn operator(&self) -> Option<(&'static str, u8, bool)> {
        [
            ("<<=", 2, true),
            (">>=", 2, true),
            ("+=", 2, true),
            ("-=", 2, true),
            ("*=", 2, true),
            ("/=", 2, true),
            ("%=", 2, true),
            ("&=", 2, true),
            ("^=", 2, true),
            ("|=", 2, true),
            ("||", 4, false),
            ("&&", 5, false),
            ("==", 9, false),
            ("!=", 9, false),
            ("<=", 10, false),
            (">=", 10, false),
            ("<<", 11, false),
            (">>", 11, false),
            ("**", 14, true),
            (",", 1, false),
            ("=", 2, true),
            ("|", 6, false),
            ("^", 7, false),
            ("&", 8, false),
            ("<", 10, false),
            (">", 10, false),
            ("+", 12, false),
            ("-", 12, false),
            ("*", 13, false),
            ("/", 13, false),
            ("%", 13, false),
        ]
        .into_iter()
        .find(|(op, _, _)| self.input[self.at..].starts_with(op.as_bytes()))
    }

    fn atom(&mut self) -> Result<Expr> {
        self.space();
        self.consume(b"$");
        let start = self.at;
        if self.input.get(self.at).is_some_and(u8::is_ascii_digit) {
            while self
                .input
                .get(self.at)
                .is_some_and(|c| c.is_ascii_alphanumeric() || b"#@_".contains(c))
            {
                self.at += 1;
            }
            return Ok(Expr::Number(number(&self.input[start..self.at])?));
        }
        if self
            .input
            .get(self.at)
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
        {
            self.at += 1;
            while self
                .input
                .get(self.at)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            {
                self.at += 1;
            }
            if self.input.get(self.at) == Some(&b'[') {
                let mut depth = 1;
                self.at += 1;
                while self.at < self.input.len() && depth > 0 {
                    match self.input[self.at] {
                        b'[' => depth += 1,
                        b']' => depth -= 1,
                        _ => {}
                    }
                    self.at += 1;
                }
                if depth != 0 {
                    return Err(Error::new("arithmetic: missing array subscript bracket"));
                }
            }
            return Ok(Expr::Variable(self.input[start..self.at].to_vec()));
        }
        Err(Error::new("arithmetic: expected an operand"))
    }
}

fn number(input: &[u8]) -> Result<i64> {
    let (base, digits) = if let Some(hash) = input.iter().position(|&c| c == b'#') {
        let base = std::str::from_utf8(&input[..hash])
            .ok()
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|&n| (2..=64).contains(&n))
            .ok_or_else(|| Error::new("arithmetic: invalid base"))?;
        (base, &input[hash + 1..])
    } else if input.starts_with(b"0x") || input.starts_with(b"0X") {
        (16, &input[2..])
    } else if input.len() > 1 && input[0] == b'0' {
        (8, &input[1..])
    } else {
        (10, input)
    };
    if digits.is_empty() {
        return Err(Error::new("arithmetic: invalid number"));
    }
    let mut value = 0i64;
    for &c in digits {
        let digit = match c {
            b'0'..=b'9' => u32::from(c - b'0'),
            b'a'..=b'z' => u32::from(c - b'a') + 10,
            b'A'..=b'Z' => u32::from(c - b'A') + if base <= 36 { 10 } else { 36 },
            b'@' => 62,
            b'_' => 63,
            _ => 64,
        };
        if digit >= base {
            return Err(Error::new("arithmetic: value too great for base"));
        }
        value = value
            .wrapping_mul(i64::from(base))
            .wrapping_add(i64::from(digit));
    }
    Ok(value)
}

fn binary(operator: &str, a: i64, b: i64) -> Result<i64> {
    Ok(match operator {
        "+" => a.wrapping_add(b),
        "-" => a.wrapping_sub(b),
        "*" => a.wrapping_mul(b),
        "/" | "%" if b == 0 => return Err(Error::new("arithmetic: division by 0")),
        "/" => a.wrapping_div(b),
        "%" => a.wrapping_rem(b),
        "**" if b < 0 => return Err(Error::new("arithmetic: exponent less than 0")),
        "**" => {
            let (mut base, mut exponent, mut value) = (a, b as u64, 1i64);
            while exponent != 0 {
                if exponent & 1 != 0 {
                    value = value.wrapping_mul(base);
                }
                exponent >>= 1;
                base = base.wrapping_mul(base);
            }
            value
        }
        "<<" => a.wrapping_shl(b as u32),
        ">>" => a.wrapping_shr(b as u32),
        "&" => a & b,
        "|" => a | b,
        "^" => a ^ b,
        "==" => i64::from(a == b),
        "!=" => i64::from(a != b),
        "<" => i64::from(a < b),
        ">" => i64::from(a > b),
        "<=" => i64::from(a <= b),
        ">=" => i64::from(a >= b),
        "," => b,
        _ => return Err(Error::new("arithmetic: invalid operator")),
    })
}

impl Expr {
    fn variable(&self) -> Result<&[u8]> {
        match self {
            Self::Variable(name) => Ok(name),
            _ => Err(Error::new(
                "arithmetic: attempted assignment to non-variable",
            )),
        }
    }

    fn evaluate(
        &self,
        vars: &mut (impl Variables + ?Sized),
        depth: usize,
        assign: bool,
    ) -> Result<i64> {
        if depth >= 128 {
            return Err(Error::new(
                "arithmetic: expression recursion level exceeded",
            ));
        }
        let depth = depth + 1;
        match self {
            Self::Number(value) => Ok(*value),
            Self::Group(value) => value.evaluate(vars, depth, assign),
            Self::Variable(name) => match vars.get(name)? {
                Some(value) => evaluate(&value, vars, depth + 1, assign),
                None => Ok(0),
            },
            Self::Unary(op, value) => {
                let value = value.evaluate(vars, depth, assign)?;
                Ok(match *op {
                    "-" => value.wrapping_neg(),
                    "!" => i64::from(value == 0),
                    "~" => !value,
                    _ => value,
                })
            }
            Self::Binary("&&", a, b) => {
                let a = a.evaluate(vars, depth, assign)?;
                let b = b.evaluate(vars, depth, assign && a != 0)?;
                Ok(i64::from(a != 0 && b != 0))
            }
            Self::Binary("||", a, b) => {
                let a = a.evaluate(vars, depth, assign)?;
                let b = b.evaluate(vars, depth, assign && a == 0)?;
                Ok(i64::from(a != 0 || b != 0))
            }
            Self::Binary(op, a, b) => {
                let a = a.evaluate(vars, depth, assign)?;
                binary(op, a, b.evaluate(vars, depth, assign)?)
            }
            Self::Conditional(test, yes, no) => {
                let test = test.evaluate(vars, depth, assign)?;
                let yes = yes.evaluate(vars, depth, assign && test != 0)?;
                let no = no.evaluate(vars, depth, assign && test == 0)?;
                Ok(if test != 0 { yes } else { no })
            }
            Self::Assign(op, left, right) => {
                let name = left.variable()?;
                let previous = if *op == "=" {
                    0
                } else {
                    left.evaluate(vars, depth, assign)?
                };
                let mut value = right.evaluate(vars, depth, assign)?;
                if *op != "=" {
                    value = binary(&op[..op.len() - 1], previous, value)?;
                }
                if assign {
                    vars.set(name, value.to_string().into_bytes())?;
                }
                Ok(value)
            }
            Self::Increment(value, amount, prefix) => {
                let name = value.variable()?;
                let previous = value.evaluate(vars, depth, assign)?;
                let next = previous.wrapping_add(*amount);
                if assign {
                    vars.set(name, next.to_string().into_bytes())?;
                }
                Ok(if *prefix { next } else { previous })
            }
        }
    }
}

fn evaluate(
    input: &[u8],
    vars: &mut (impl Variables + ?Sized),
    depth: usize,
    assign: bool,
) -> Result<i64> {
    if depth >= 128 {
        return Err(Error::new(
            "arithmetic: expression recursion level exceeded",
        ));
    }
    if input.iter().all(u8::is_ascii_whitespace) {
        return Ok(0);
    }
    let mut parser = Parser {
        input,
        at: 0,
        depth: 0,
        nodes: 0,
    };
    let expression = parser.expression(1)?;
    parser.space();
    if parser.at != input.len() {
        return Err(Error::new("arithmetic: syntax error in expression"));
    }
    expression.evaluate(vars, depth, assign)
}

pub fn eval(input: &[u8], vars: &mut (impl Variables + ?Sized)) -> Result<i64> {
    evaluate(input, vars, 0, true).map_err(|mut error| {
        error.status = 1;
        error
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Vars(BTreeMap<Vec<u8>, Vec<u8>>);
    impl Variables for Vars {
        fn get(&mut self, name: &[u8]) -> Result<Option<Vec<u8>>> {
            Ok(self.0.get(name).cloned())
        }
        fn set(&mut self, name: &[u8], value: Vec<u8>) -> Result<()> {
            self.0.insert(name.to_vec(), value);
            Ok(())
        }
    }
    #[test]
    fn arithmetic_precedence_and_side_effects() {
        let mut vars = Vars::default();
        assert_eq!(eval(b"n=2, n++ + ++n * 3", &mut vars).unwrap(), 14);
        assert_eq!(eval(b"n", &mut vars).unwrap(), 4);
        assert_eq!(eval(b"0 && (n=8)", &mut vars).unwrap(), 0);
        assert!(eval(b"1 ? 7 : 1/0", &mut vars).is_err());
        assert_eq!(eval(b"2**3**2", &mut vars).unwrap(), 512);
        assert_eq!(eval(b"64#_ + 16#ff + 010", &mut vars).unwrap(), 326);
        assert!(eval(b"1/0", &mut vars).is_err());
        vars.set(b"x", b"x+1".to_vec()).unwrap();
        assert!(eval(b"x", &mut vars).is_err());
        assert!(eval(&b"1+".repeat(5000), &mut vars).is_err());
    }
}
