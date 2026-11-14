//! `=` calculator on fend-core: live result, `0x…` hex, `86% of 5000`, units.

use std::time::{Duration, Instant};

struct Deadline(Instant);

impl fend_core::Interrupt for Deadline {
    fn should_interrupt(&self) -> bool {
        Instant::now() > self.0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CalcOut {
    /// Nothing typed yet.
    Empty,
    Value {
        text: String,
        /// The result rounded to an integer when it is a plain non-integer number.
        int: Option<String>,
    },
    /// Incomplete or invalid input (shown muted while typing).
    Error(String),
}

pub struct Calc {
    ctx: fend_core::Context,
    /// Session history, newest first: (expression, result).
    pub history: Vec<(String, String)>,
}

impl Calc {
    pub fn new() -> Self {
        Self { ctx: fend_core::Context::new(), history: Vec::new() }
    }

    fn run(ctx: &mut fend_core::Context, expr: &str) -> Result<String, String> {
        let deadline = Deadline(Instant::now() + Duration::from_millis(150));
        fend_core::evaluate_with_interrupt(expr, ctx, &deadline)
            .map(|r| r.get_main_result().to_string())
            .map(|t| decimal_of(&t).unwrap_or(t))
    }

    /// Live evaluation; session variables are visible but never modified.
    pub fn eval(&self, expr: &str) -> CalcOut {
        let expr = expr.trim();
        if expr.is_empty() {
            return CalcOut::Empty;
        }
        let mut ctx = self.ctx.clone();
        match Self::run(&mut ctx, expr) {
            Ok(text) if text.trim().is_empty() => CalcOut::Error("no result".into()),
            Ok(text) => {
                let int = int_of(&text);
                CalcOut::Value { text, int }
            }
            Err(e) => CalcOut::Error(e),
        }
    }

    /// Evaluate for real (variables stick) and record it in the history.
    pub fn commit(&mut self, expr: &str) -> Option<String> {
        let expr = expr.trim();
        let text = Self::run(&mut self.ctx, expr).ok().filter(|t| !t.trim().is_empty())?;
        if self.history.first().is_none_or(|(e, r)| e != expr || *r != text) {
            self.history.insert(0, (expr.to_string(), text.clone()));
            self.history.truncate(20);
        }
        Some(text)
    }
}

/// fend keeps the base of hex/binary/octal input (`0x2a2a27`); show the decimal value.
fn decimal_of(text: &str) -> Option<String> {
    let t = text.trim();
    let (radix, digits) = match t.get(..2)? {
        "0x" => (16, &t[2..]),
        "0b" => (2, &t[2..]),
        "0o" => (8, &t[2..]),
        _ => return None,
    };
    u128::from_str_radix(&digits.replace('_', ""), radix).ok().map(|v| v.to_string())
}

/// `966.68` → `967`; `None` for integers, units and anything that is not a plain number.
fn int_of(text: &str) -> Option<String> {
    let t = text.trim().replace([',', '\u{202f}', ' '], "");
    if t.is_empty() || t.chars().any(|c| !(c.is_ascii_digit() || matches!(c, '.' | '-' | 'e' | 'E' | '+'))) {
        return None;
    }
    let v: f64 = t.parse().ok()?;
    if !v.is_finite() || v.fract() == 0.0 || v.abs() >= 1e15 {
        return None;
    }
    Some(format!("{}", v.round() as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(c: &Calc, e: &str) -> String {
        match c.eval(e) {
            CalcOut::Value { text, .. } => text,
            other => panic!("{e}: {other:?}"),
        }
    }

    #[test]
    fn arithmetic_hex_and_percent() {
        let c = Calc::new();
        assert_eq!(text(&c, "1920 / 2"), "960");
        assert_eq!(text(&c, "0x2A2A27"), "2763303");
        assert_eq!(text(&c, "86% of 5000"), "4300");
        assert_eq!(text(&c, "0.66667 * 1468 - 12"), "966.67156");
        assert_eq!(text(&c, "5 km to m"), "5000 m");
    }

    #[test]
    fn int_variant_only_for_fractions() {
        let c = Calc::new();
        assert_eq!(c.eval("0.66667 * 1468 - 12"), CalcOut::Value { text: "966.67156".into(), int: Some("967".into()) });
        assert_eq!(c.eval("2+2"), CalcOut::Value { text: "4".into(), int: None });
    }

    #[test]
    fn empty_and_errors() {
        let c = Calc::new();
        assert_eq!(c.eval("  "), CalcOut::Empty);
        assert!(matches!(c.eval("1 +"), CalcOut::Error(_)));
    }

    #[test]
    fn commit_keeps_variables_and_history_live_does_not() {
        let mut c = Calc::new();
        assert!(matches!(c.eval("x = 5"), CalcOut::Value { .. }));
        assert!(matches!(c.eval("x * 2"), CalcOut::Error(_)), "live eval must not define x");
        assert_eq!(c.commit("x = 5").as_deref(), Some("5"));
        assert_eq!(c.commit("x * 2").as_deref(), Some("10"));
        assert_eq!(c.history[0], ("x * 2".to_string(), "10".to_string()));
        assert_eq!(c.history.len(), 2);
        c.commit("x * 2");
        assert_eq!(c.history.len(), 2, "same entry twice in a row is not repeated");
    }
}
