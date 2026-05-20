//! Phase 99.2 — `visible_when` mini-DSL parser + evaluator.
//!
//! A `visible_when` expression decides whether an admin-UI
//! contribution / field is shown. It is declared in
//! `[plugin.admin_ui]` (validated at parse time, Phase 99.1) and
//! evaluated both server-side (drop hidden contributions in the
//! `plugin_ui/list` aggregator, Phase 99.5) and client-side (live
//! field visibility as form values change, ported to TS in 99.7).
//!
//! Pure module — no runtime deps. The evaluator takes a
//! `serde_json::Value` object as context:
//!
//! ```text
//! { "plugin": {"installed": true, "enabled": true, "healthy": false},
//!   "config": {"use_tls": true, "port": 587},
//!   "tenant": {"id": "acme"},
//!   "user":   {"role": "admin"} }
//! ```
//!
//! ## Grammar (recursive descent)
//!
//! ```text
//! expr    = or
//! or      = and ('||' and)*
//! and     = cmp ('&&' cmp)*
//! cmp     = unary (('==' | '!=' | '<' | '>' | '<=' | '>=') unary)?
//! unary   = '!'? primary
//! primary = var | literal | '(' expr ')'
//! var     = ident ('.' ident)*        e.g. plugin.enabled
//! literal = string | number | bool    "x" | 'x' | 12.5 | -3 | true
//! ```
//!
//! Bounds: source ≤ [`MAX_LEN`] chars, AST depth ≤ [`MAX_DEPTH`].
//! Evaluation never panics — a missing variable resolves to
//! `null` (falsy) and a type-mismatched comparison yields `false`.

use serde_json::Value;

/// Max characters in a `visible_when` source expression.
pub const MAX_LEN: usize = 200;

/// Max AST depth. Leaves (var / literal) are depth 1.
pub const MAX_DEPTH: usize = 5;

/// Parse / bound failure. Evaluation itself is infallible (errors
/// degrade to `false`); only `parse` can fail.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VisibleWhenError {
    #[error("visible_when expression exceeds {MAX_LEN} chars (got {0})")]
    TooLong(usize),
    #[error("visible_when AST depth exceeds {MAX_DEPTH}")]
    TooDeep,
    #[error("visible_when syntax error: {0}")]
    Syntax(String),
}

fn syntax(msg: impl Into<String>) -> VisibleWhenError {
    VisibleWhenError::Syntax(msg.into())
}

// ── AST ──────────────────────────────────────────────────────────

/// Parsed expression tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Cmp {
        op: CmpOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// Dotted variable path (`plugin.enabled` → `["plugin","enabled"]`).
    Var(Vec<String>),
    Lit(Literal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Str(String),
    Num(f64),
    Bool(bool),
}

impl Literal {
    fn to_value(&self) -> Value {
        match self {
            Literal::Str(s) => Value::String(s.clone()),
            Literal::Num(n) => serde_json::Number::from_f64(*n)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            Literal::Bool(b) => Value::Bool(*b),
        }
    }
}

// ── Public API ───────────────────────────────────────────────────

/// Parse + bound-check an expression.
pub fn parse(src: &str) -> Result<Expr, VisibleWhenError> {
    if src.len() > MAX_LEN {
        return Err(VisibleWhenError::TooLong(src.len()));
    }
    let tokens = lex(src)?;
    let mut p = Parser { tokens, pos: 0 };
    let expr = p.parse_or()?;
    if p.pos != p.tokens.len() {
        return Err(syntax(format!("unexpected trailing token at {}", p.pos)));
    }
    if depth(&expr) > MAX_DEPTH {
        return Err(VisibleWhenError::TooDeep);
    }
    Ok(expr)
}

/// Evaluate a parsed expression against `ctx` (a JSON object).
/// Infallible — degrades to `false` on missing vars / type
/// mismatches.
pub fn eval(expr: &Expr, ctx: &Value) -> bool {
    truthy(&eval_value(expr, ctx))
}

/// Convenience: parse then eval. Parse errors propagate; a caller
/// that wants "invalid ⇒ hidden" maps `Err(_) => false`.
pub fn parse_and_eval(src: &str, ctx: &Value) -> Result<bool, VisibleWhenError> {
    Ok(eval(&parse(src)?, ctx))
}

// ── Evaluation ───────────────────────────────────────────────────

fn eval_value(expr: &Expr, ctx: &Value) -> Value {
    match expr {
        Expr::Lit(l) => l.to_value(),
        Expr::Var(path) => resolve(path, ctx).cloned().unwrap_or(Value::Null),
        Expr::Not(e) => Value::Bool(!truthy(&eval_value(e, ctx))),
        Expr::And(a, b) => Value::Bool(truthy(&eval_value(a, ctx)) && truthy(&eval_value(b, ctx))),
        Expr::Or(a, b) => Value::Bool(truthy(&eval_value(a, ctx)) || truthy(&eval_value(b, ctx))),
        Expr::Cmp { op, lhs, rhs } => {
            Value::Bool(compare(*op, &eval_value(lhs, ctx), &eval_value(rhs, ctx)))
        }
    }
}

/// JSON truthiness: bool→itself; null→false; number→non-zero;
/// string/array/object→non-empty.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn compare(op: CmpOp, l: &Value, r: &Value) -> bool {
    match op {
        CmpOp::Eq => values_equal(l, r),
        CmpOp::Ne => !values_equal(l, r),
        CmpOp::Lt | CmpOp::Gt | CmpOp::Le | CmpOp::Ge => {
            if let (Some(a), Some(b)) = (l.as_f64(), r.as_f64()) {
                match op {
                    CmpOp::Lt => a < b,
                    CmpOp::Gt => a > b,
                    CmpOp::Le => a <= b,
                    CmpOp::Ge => a >= b,
                    _ => unreachable!(),
                }
            } else if let (Value::String(a), Value::String(b)) = (l, r) {
                match op {
                    CmpOp::Lt => a < b,
                    CmpOp::Gt => a > b,
                    CmpOp::Le => a <= b,
                    CmpOp::Ge => a >= b,
                    _ => unreachable!(),
                }
            } else {
                // Incomparable types → false (never panic).
                false
            }
        }
    }
}

/// Equality that treats integer / float JSON numbers as equal by
/// value (`25 == 25.0`); everything else uses `Value` equality.
fn values_equal(l: &Value, r: &Value) -> bool {
    if let (Some(a), Some(b)) = (l.as_f64(), r.as_f64()) {
        return a == b;
    }
    l == r
}

fn resolve<'a>(path: &[String], ctx: &'a Value) -> Option<&'a Value> {
    let mut cur = ctx;
    for seg in path {
        cur = cur.as_object()?.get(seg)?;
    }
    Some(cur)
}

fn depth(e: &Expr) -> usize {
    match e {
        Expr::Var(_) | Expr::Lit(_) => 1,
        Expr::Not(inner) => 1 + depth(inner),
        Expr::And(a, b) | Expr::Or(a, b) => 1 + depth(a).max(depth(b)),
        Expr::Cmp { lhs, rhs, .. } => 1 + depth(lhs).max(depth(rhs)),
    }
}

// ── Lexer ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    And,
    Or,
    Not,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    LParen,
    RParen,
    Ident(String),
    Num(f64),
    Str(String),
}

fn lex(s: &str) -> Result<Vec<Token>, VisibleWhenError> {
    let mut toks = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                chars.next();
            }
            '(' => {
                chars.next();
                toks.push(Token::LParen);
            }
            ')' => {
                chars.next();
                toks.push(Token::RParen);
            }
            '&' => {
                chars.next();
                if chars.peek() == Some(&'&') {
                    chars.next();
                    toks.push(Token::And);
                } else {
                    return Err(syntax("expected `&&`"));
                }
            }
            '|' => {
                chars.next();
                if chars.peek() == Some(&'|') {
                    chars.next();
                    toks.push(Token::Or);
                } else {
                    return Err(syntax("expected `||`"));
                }
            }
            '!' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    toks.push(Token::Ne);
                } else {
                    toks.push(Token::Not);
                }
            }
            '=' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    toks.push(Token::Eq);
                } else {
                    return Err(syntax("expected `==`"));
                }
            }
            '<' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    toks.push(Token::Le);
                } else {
                    toks.push(Token::Lt);
                }
            }
            '>' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    toks.push(Token::Ge);
                } else {
                    toks.push(Token::Gt);
                }
            }
            '"' | '\'' => {
                let quote = c;
                chars.next();
                let mut buf = String::new();
                let mut closed = false;
                for ch in chars.by_ref() {
                    if ch == quote {
                        closed = true;
                        break;
                    }
                    if ch == '\n' {
                        return Err(syntax("newline inside string literal"));
                    }
                    buf.push(ch);
                }
                if !closed {
                    return Err(syntax("unterminated string literal"));
                }
                toks.push(Token::Str(buf));
            }
            '-' | '0'..='9' => {
                let mut buf = String::new();
                if c == '-' {
                    buf.push('-');
                    chars.next();
                    if !matches!(chars.peek(), Some('0'..='9')) {
                        return Err(syntax("`-` not followed by a digit"));
                    }
                }
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() || d == '.' {
                        buf.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let n: f64 = buf
                    .parse()
                    .map_err(|_| syntax(format!("invalid number `{buf}`")))?;
                toks.push(Token::Num(n));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut buf = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_alphanumeric() || d == '_' || d == '.' {
                        buf.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                toks.push(Token::Ident(buf));
            }
            other => return Err(syntax(format!("unexpected character `{other}`"))),
        }
    }
    Ok(toks)
}

// ── Parser ───────────────────────────────────────────────────────

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_or(&mut self) -> Result<Expr, VisibleWhenError> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, VisibleWhenError> {
        let mut left = self.parse_cmp()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.advance();
            let right = self.parse_cmp()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> Result<Expr, VisibleWhenError> {
        let left = self.parse_unary()?;
        let op = match self.peek() {
            Some(Token::Eq) => CmpOp::Eq,
            Some(Token::Ne) => CmpOp::Ne,
            Some(Token::Lt) => CmpOp::Lt,
            Some(Token::Gt) => CmpOp::Gt,
            Some(Token::Le) => CmpOp::Le,
            Some(Token::Ge) => CmpOp::Ge,
            _ => return Ok(left),
        };
        self.advance();
        let right = self.parse_unary()?;
        Ok(Expr::Cmp {
            op,
            lhs: Box::new(left),
            rhs: Box::new(right),
        })
    }

    fn parse_unary(&mut self) -> Result<Expr, VisibleWhenError> {
        if matches!(self.peek(), Some(Token::Not)) {
            self.advance();
            let inner = self.parse_primary()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, VisibleWhenError> {
        match self.advance() {
            Some(Token::LParen) => {
                let inner = self.parse_or()?;
                match self.advance() {
                    Some(Token::RParen) => Ok(inner),
                    _ => Err(syntax("expected `)`")),
                }
            }
            Some(Token::Ident(s)) => {
                let s = s.clone();
                match s.as_str() {
                    "true" => Ok(Expr::Lit(Literal::Bool(true))),
                    "false" => Ok(Expr::Lit(Literal::Bool(false))),
                    _ => {
                        let segments: Vec<String> = s.split('.').map(str::to_string).collect();
                        if segments.iter().any(|seg| seg.is_empty()) {
                            return Err(syntax(format!("invalid variable path `{s}`")));
                        }
                        Ok(Expr::Var(segments))
                    }
                }
            }
            Some(Token::Num(n)) => Ok(Expr::Lit(Literal::Num(*n))),
            Some(Token::Str(s)) => Ok(Expr::Lit(Literal::Str(s.clone()))),
            Some(other) => Err(syntax(format!("unexpected token {other:?}"))),
            None => Err(syntax("unexpected end of expression")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> Value {
        json!({
            "plugin": {"installed": true, "enabled": true, "healthy": false},
            "config": {"use_tls": true, "port": 587, "name": "smtp", "tags": []},
            "tenant": {"id": "acme"},
            "user":   {"role": "admin"}
        })
    }

    fn ev(src: &str) -> bool {
        eval(&parse(src).expect("parse"), &ctx())
    }

    // ── parse: happy ─────────────────────────────────────────
    #[test]
    fn parse_single_var() {
        assert_eq!(
            parse("plugin.enabled").unwrap(),
            Expr::Var(vec!["plugin".into(), "enabled".into()])
        );
    }

    #[test]
    fn parse_bool_literals() {
        assert_eq!(parse("true").unwrap(), Expr::Lit(Literal::Bool(true)));
        assert_eq!(parse("false").unwrap(), Expr::Lit(Literal::Bool(false)));
    }

    #[test]
    fn parse_number_and_negative() {
        assert_eq!(parse("42").unwrap(), Expr::Lit(Literal::Num(42.0)));
        assert_eq!(parse("-3").unwrap(), Expr::Lit(Literal::Num(-3.0)));
        assert_eq!(parse("12.5").unwrap(), Expr::Lit(Literal::Num(12.5)));
    }

    #[test]
    fn parse_string_both_quotes() {
        assert_eq!(
            parse("\"admin\"").unwrap(),
            Expr::Lit(Literal::Str("admin".into()))
        );
        assert_eq!(
            parse("'admin'").unwrap(),
            Expr::Lit(Literal::Str("admin".into()))
        );
    }

    #[test]
    fn parse_and_or_not_parens() {
        assert!(parse("a && b").is_ok());
        assert!(parse("a || b").is_ok());
        assert!(parse("!a").is_ok());
        assert!(parse("!(a && b)").is_ok());
        assert!(parse("(a || b) && c").is_ok());
    }

    #[test]
    fn parse_all_comparison_ops() {
        for op in ["==", "!=", "<", ">", "<=", ">="] {
            assert!(parse(&format!("config.port {op} 500")).is_ok(), "op {op}");
        }
    }

    #[test]
    fn parse_precedence_or_lower_than_and() {
        // a || b && c  ==  a || (b && c)
        let e = parse("a || b && c").unwrap();
        assert!(matches!(e, Expr::Or(_, _)));
    }

    // ── parse: errors ────────────────────────────────────────
    #[test]
    fn parse_empty_is_error() {
        assert!(matches!(parse(""), Err(VisibleWhenError::Syntax(_))));
    }

    #[test]
    fn parse_unbalanced_paren_error() {
        assert!(parse("(a && b").is_err());
        assert!(parse("a && b)").is_err());
    }

    #[test]
    fn parse_trailing_token_error() {
        assert!(parse("a b").is_err());
    }

    #[test]
    fn parse_single_amp_error() {
        assert!(parse("a & b").is_err());
    }

    #[test]
    fn parse_single_pipe_error() {
        assert!(parse("a | b").is_err());
    }

    #[test]
    fn parse_lone_equals_error() {
        assert!(parse("a = b").is_err());
    }

    #[test]
    fn parse_dangling_dot_var_error() {
        assert!(parse("plugin.").is_err());
        assert!(parse(".enabled").is_err());
        assert!(parse("a..b").is_err());
    }

    #[test]
    fn parse_unterminated_string_error() {
        assert!(parse("\"abc").is_err());
    }

    #[test]
    fn parse_bad_number_error() {
        assert!(parse("1.2.3").is_err());
        assert!(parse("-x").is_err());
    }

    #[test]
    fn parse_unexpected_char_error() {
        assert!(parse("a @ b").is_err());
    }

    #[test]
    fn parse_too_long_error() {
        let src = "a".repeat(MAX_LEN + 1);
        assert_eq!(parse(&src), Err(VisibleWhenError::TooLong(MAX_LEN + 1)));
    }

    #[test]
    fn parse_too_deep_error() {
        // Left-assoc `&&` chain: a&&b&&c&&d&&e&&f → depth 6 > MAX_DEPTH.
        assert_eq!(parse("a&&b&&c&&d&&e&&f"), Err(VisibleWhenError::TooDeep));
    }

    #[test]
    fn parse_at_max_depth_ok() {
        // a&&b&&c&&d&&e → depth 5 (And nest) — exactly at the cap.
        assert!(parse("a&&b&&c&&d&&e").is_ok());
    }

    #[test]
    fn parse_stacked_not_is_syntax_error() {
        // Grammar allows a single `!` (unary = '!'? primary); stacked
        // `!` needs parens: `!(!a)`. Bare `!!a` is a syntax error.
        assert!(matches!(parse("!!a"), Err(VisibleWhenError::Syntax(_))));
        assert!(parse("!(!a)").is_ok());
    }

    // ── eval: truthiness ─────────────────────────────────────
    #[test]
    fn eval_var_bool_true() {
        assert!(ev("plugin.enabled"));
    }

    #[test]
    fn eval_var_bool_false() {
        assert!(!ev("plugin.healthy"));
    }

    #[test]
    fn eval_missing_var_is_false() {
        assert!(!ev("plugin.nonexistent"));
        assert!(!ev("ghost.deep.path"));
    }

    #[test]
    fn eval_nonempty_string_truthy() {
        assert!(ev("config.name"));
    }

    #[test]
    fn eval_empty_array_falsy() {
        assert!(!ev("config.tags"));
    }

    #[test]
    fn eval_number_nonzero_truthy() {
        assert!(ev("config.port"));
    }

    #[test]
    fn eval_bool_literals() {
        assert!(ev("true"));
        assert!(!ev("false"));
    }

    // ── eval: logic ──────────────────────────────────────────
    #[test]
    fn eval_and() {
        assert!(ev("plugin.installed && plugin.enabled"));
        assert!(!ev("plugin.enabled && plugin.healthy"));
    }

    #[test]
    fn eval_or() {
        assert!(ev("plugin.healthy || plugin.enabled"));
        assert!(!ev("plugin.healthy || false"));
    }

    #[test]
    fn eval_not() {
        assert!(ev("!plugin.healthy"));
        assert!(!ev("!plugin.enabled"));
    }

    #[test]
    fn eval_parens_override_precedence() {
        // !(enabled && installed) == false
        assert!(!ev("!(plugin.enabled && plugin.installed)"));
        // (healthy || enabled) && installed == true
        assert!(ev("(plugin.healthy || plugin.enabled) && plugin.installed"));
    }

    // ── eval: comparisons ────────────────────────────────────
    #[test]
    fn eval_eq_number() {
        assert!(ev("config.port == 587"));
        assert!(!ev("config.port == 25"));
    }

    #[test]
    fn eval_eq_int_float_equal() {
        let e = parse("config.port == 587.0").unwrap();
        assert!(eval(&e, &ctx()));
    }

    #[test]
    fn eval_ne() {
        assert!(ev("config.port != 25"));
        assert!(!ev("config.port != 587"));
    }

    #[test]
    fn eval_lt_gt_le_ge() {
        assert!(ev("config.port > 500"));
        assert!(ev("config.port < 1000"));
        assert!(ev("config.port >= 587"));
        assert!(ev("config.port <= 587"));
        assert!(!ev("config.port < 100"));
    }

    #[test]
    fn eval_eq_string() {
        assert!(ev("user.role == \"admin\""));
        assert!(!ev("user.role == \"viewer\""));
    }

    #[test]
    fn eval_string_lexicographic() {
        assert!(ev("tenant.id < \"zzz\""));
        assert!(!ev("tenant.id > \"zzz\""));
    }

    #[test]
    fn eval_bool_compared_to_literal() {
        assert!(ev("config.use_tls == true"));
        assert!(!ev("config.use_tls == false"));
    }

    #[test]
    fn eval_type_mismatch_relational_is_false() {
        // string vs number under `<` → incomparable → false
        assert!(!ev("user.role < 5"));
        // bool vs number under `>` → false
        assert!(!ev("config.use_tls > 1"));
    }

    #[test]
    fn eval_missing_var_comparison_false() {
        assert!(!ev("config.missing == 1"));
        // missing != literal → true (null != 1)
        assert!(ev("config.missing != 1"));
    }

    #[test]
    fn eval_complex_expression() {
        assert!(ev(
            "plugin.enabled && (user.role == \"admin\" || user.role == \"owner\") && config.port >= 25"
        ));
        assert!(!ev("plugin.healthy && config.port == 587"));
    }

    #[test]
    fn eval_not_with_comparison() {
        assert!(ev("!(config.port == 25)"));
    }

    #[test]
    fn parse_and_eval_helper_works() {
        assert_eq!(parse_and_eval("plugin.enabled", &ctx()), Ok(true));
        assert!(parse_and_eval("a &", &ctx()).is_err());
    }

    #[test]
    fn eval_against_empty_context_is_false_not_panic() {
        let empty = json!({});
        assert!(!eval(&parse("plugin.enabled").unwrap(), &empty));
        assert!(!eval(&parse("a.b.c.d == 1").unwrap(), &empty));
    }

    #[test]
    fn whitespace_insensitive() {
        assert!(ev("  plugin.enabled   &&plugin.installed  "));
    }
}
