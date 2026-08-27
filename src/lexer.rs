//! Lexer — 把源码切成 token 流。
//!
//! 文法要点(全部来自已定稿宪法, 不得私自放宽):
//! - 单引号字符串 '...' 内含 $name / ${expr} 插值 → 切成 StrPart 片段
//! - 分号 Kotlin 式可省略 → lexer 发出 Newline token, 由 parser 决定边界;
//!   括号深度 > 0 时换行视为普通空白
//! - 行首延续符 '.' 抑制 Newline (支持链式调用续行)

use crate::{AliasError, AliasResult, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // 关键字
    Val,
    Var,
    Func,
    Struct,
    Pub,
    SelfKw,
    For,
    While,
    If,
    Else,
    In,
    Break,
    Continue,
    Return,
    Match,

    // 字面量
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Vec<StrPart>),
    Ident(String),

    // 符号
    Assign,   // =
    Arrow,    // ->
    Plus,     // +
    Minus,    // -
    Star,     // *
    Slash,    // /
    Percent,  // %
    Bang,     // !
    Tilde,    // ~
    Amp,      // &
    Pipe,     // |
    Caret,    // ^
    AndAnd,   // &&
    OrOr,     // ||
    Shl,      // <<
    Shr,      // >>
    Lt,       // <
    Le,       // <=
    Gt,       // >
    Ge,       // >=
    EqEq,     // ==
    NotEq,    // !=
    LParen,   // (
    RParen,   // )
    LBrace,   // {
    RBrace,   // }
    LBracket, // [
    RBracket, // ]
    Comma,    // ,
    Semi,     // ;
    Dot,      // .
    Question, // ?
    Colon,    // :
    Newline,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Lit(String),
    Hole(Vec<(Tok, Span)>),
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: u32,
    col: u32,
    paren_depth: u32,
    interp_depth: u16,
}

const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOKENS: usize = 200_000;

pub fn lex(src: &str) -> AliasResult<Vec<Token>> {
    if src.len() > MAX_SOURCE_BYTES {
        return Err(AliasError {
            msg: format!("源文件超过 {} MiB 上限", MAX_SOURCE_BYTES / 1024 / 1024),
            span: Span {
                line: 1,
                col: 1,
                len: 1,
            },
        });
    }
    let mut lx = Lexer {
        src: src.as_bytes(),
        pos: 0,
        line: 1,
        col: 1,
        paren_depth: 0,
        interp_depth: 0,
    };
    let mut out = Vec::new();
    while let Some(t) = lx.next_token()? {
        if out.len() >= MAX_TOKENS {
            return Err(AliasError {
                msg: format!("源文件 token 数超过 {MAX_TOKENS} 上限"),
                span: t.span,
            });
        }
        out.push(t);
    }
    Ok(out)
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.src.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn span_here(&self, len: u32) -> Span {
        Span {
            line: self.line,
            col: self.col.saturating_sub(len).max(1),
            len,
        }
    }

    fn err<T>(&self, msg: impl Into<String>) -> AliasResult<T> {
        Err(AliasError {
            msg: msg.into(),
            span: self.span_here(1),
        })
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') => {
                    self.bump();
                }
                Some(b'/') if self.peek2() == Some(b'/') => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => break,
            }
        }
    }

    fn newline_is_continuation(&self) -> bool {
        let mut i = self.pos;
        while i < self.src.len() {
            match self.src[i] {
                b' ' | b'\t' | b'\r' => i += 1,
                b'.' => return true,
                _ => return false,
            }
        }
        false
    }

    fn next_token(&mut self) -> AliasResult<Option<Token>> {
        loop {
            self.skip_trivia();
            let Some(c) = self.peek() else {
                return Ok(None);
            };
            let start_span = self.span_here(1);
            if c != b'\n' {
                break;
            }
            self.bump();
            if self.paren_depth == 0 && !self.newline_is_continuation() {
                return Ok(Some(Token {
                    tok: Tok::Newline,
                    span: start_span,
                }));
            }
        }

        let c = self.peek().expect("上方已排除 EOF");
        let start_span = self.span_here(1);
        let tok = match c {
            b'0'..=b'9' => self.lex_int()?,
            b'\'' => self.lex_string()?,
            c if is_ident_start(c) => self.lex_ident_or_keyword(),
            _ => self.lex_symbol()?,
        };

        let end_line = self.line;
        let end_col = self.col;
        let span = Span {
            line: start_span.line,
            col: start_span.col,
            len: if end_line == start_span.line {
                (end_col - start_span.col).max(1)
            } else {
                1
            },
        };
        Ok(Some(Token { tok, span }))
    }

    fn lex_int(&mut self) -> AliasResult<Tok> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.bump();
            } else {
                break;
            }
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') && self.peek2().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            is_float = true;
            self.bump();
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            is_float = true;
            self.bump();
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.bump();
            }
            let exp_start = self.pos;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.bump();
            }
            if self.pos == exp_start {
                return self.err("浮点指数缺少数字 — 例如 1e5");
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).map_err(|_| AliasError {
            msg: "数值字面量编码无效".into(),
            span: self.span_here(1),
        })?;
        if is_float {
            let v: f64 = text.parse().map_err(|_| AliasError {
                msg: "浮点字面量格式无效".into(),
                span: self.span_here((self.pos - start) as u32),
            })?;
            if !v.is_finite() {
                return self.err("浮点字面量超出 f64 表示范围");
            }
            return Ok(Tok::Float(v));
        }
        let n = text.parse::<i64>().map_err(|_| AliasError {
            msg: "整数字面量超出 i64 表示范围".into(),
            span: self.span_here((self.pos - start) as u32),
        })?;
        Ok(Tok::Int(n))
    }

    fn lex_ident_or_keyword(&mut self) -> Tok {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if is_ident_continue(c) {
                self.bump();
            } else {
                break;
            }
        }
        let word = &self.src[start..self.pos];
        match word {
            b"val" => Tok::Val,
            b"var" => Tok::Var,
            b"func" => Tok::Func,
            b"struct" => Tok::Struct,
            b"pub" => Tok::Pub,
            b"self" => Tok::SelfKw,
            b"for" => Tok::For,
            b"while" => Tok::While,
            b"if" => Tok::If,
            b"else" => Tok::Else,
            b"in" => Tok::In,
            b"break" => Tok::Break,
            b"continue" => Tok::Continue,
            b"return" => Tok::Return,
            b"match" => Tok::Match,
            b"true" => Tok::Bool(true),
            b"false" => Tok::Bool(false),
            _ => Tok::Ident(String::from_utf8_lossy(word).into_owned()),
        }
    }

    fn lex_symbol(&mut self) -> AliasResult<Tok> {
        let c = self.bump().unwrap();
        let tok = match c {
            b'=' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    Tok::EqEq
                } else {
                    Tok::Assign
                }
            }
            b'-' => {
                if self.peek() == Some(b'>') {
                    self.bump();
                    Tok::Arrow
                } else {
                    Tok::Minus
                }
            }
            b'+' => Tok::Plus,
            b'*' => Tok::Star,
            b'/' => Tok::Slash,
            b'%' => Tok::Percent,
            b'~' => Tok::Tilde,
            b'^' => Tok::Caret,
            b'<' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    Tok::Le
                } else if self.peek() == Some(b'<') {
                    self.bump();
                    Tok::Shl
                } else {
                    Tok::Lt
                }
            }
            b'>' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    Tok::Ge
                } else if self.peek() == Some(b'>') {
                    self.bump();
                    Tok::Shr
                } else {
                    Tok::Gt
                }
            }
            b'!' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    Tok::NotEq
                } else {
                    Tok::Bang
                }
            }
            b'&' => {
                if self.peek() == Some(b'&') {
                    self.bump();
                    Tok::AndAnd
                } else {
                    Tok::Amp
                }
            }
            b'|' => {
                if self.peek() == Some(b'|') {
                    self.bump();
                    Tok::OrOr
                } else {
                    Tok::Pipe
                }
            }
            b'(' => {
                self.paren_depth += 1;
                Tok::LParen
            }
            b')' => {
                self.paren_depth = self.paren_depth.saturating_sub(1);
                Tok::RParen
            }
            b'{' => Tok::LBrace,
            b'}' => Tok::RBrace,
            b'[' => Tok::LBracket,
            b']' => Tok::RBracket,
            b',' => Tok::Comma,
            b';' => Tok::Semi,
            b'.' => Tok::Dot,
            b'?' => Tok::Question,
            b':' => Tok::Colon,
            other => {
                return self.err(format!(
                    "无法识别的字符 '{}'",
                    char::from_u32(other as u32).unwrap_or('?')
                ));
            }
        };
        Ok(tok)
    }

    fn lex_string(&mut self) -> AliasResult<Tok> {
        self.bump();
        let mut parts: Vec<StrPart> = Vec::new();
        let mut lit = String::new();

        loop {
            let Some(c) = self.peek() else {
                return self.err("字符串未闭合 — 缺少收尾单引号");
            };
            match c {
                b'\'' => {
                    self.bump();
                    break;
                }
                b'\n' => return self.err("字符串内不允许裸换行"),
                b'$' => {
                    self.bump();
                    match self.peek() {
                        Some(b'{') => {
                            self.bump();
                            if self.interp_depth >= 128 {
                                return self.err("字符串插值嵌套超过 128 层上限");
                            }
                            self.interp_depth += 1;
                            let result = self.lex_hole_until_rbrace();
                            self.interp_depth -= 1;
                            let toks = result?;
                            if !lit.is_empty() {
                                parts.push(StrPart::Lit(std::mem::take(&mut lit)));
                            }
                            parts.push(StrPart::Hole(toks));
                        }
                        Some(c2) if is_ident_start(c2) => {
                            let name_start = self.pos;
                            while let Some(c3) = self.peek() {
                                if is_ident_continue(c3) {
                                    self.bump();
                                } else {
                                    break;
                                }
                            }
                            if self.peek() == Some(b'.') {
                                return self
                                    .err("$name 仅支持单个标识符 — 复杂表达式请使用 ${...}");
                            }
                            let name = String::from_utf8_lossy(&self.src[name_start..self.pos])
                                .into_owned();
                            let sub = lex(&name).map_err(|mut e| {
                                e.msg = format!("插值片段 '{name}' 解析失败: {}", e.msg);
                                e
                            })?;
                            let toks = sub.into_iter().map(|t| (t.tok, t.span)).collect();
                            if !lit.is_empty() {
                                parts.push(StrPart::Lit(std::mem::take(&mut lit)));
                            }
                            parts.push(StrPart::Hole(toks));
                        }
                        _ => lit.push('$'),
                    }
                }
                b'\\' => {
                    self.bump();
                    let Some(esc) = self.bump() else {
                        return self.err("转义符后意外结尾");
                    };
                    match esc {
                        b'n' => lit.push('\n'),
                        b't' => lit.push('\t'),
                        b'r' => lit.push('\r'),
                        b'0' => lit.push('\0'),
                        b'\\' => lit.push('\\'),
                        b'\'' => lit.push('\''),
                        b'"' => lit.push('"'),
                        b'$' => lit.push('$'),
                        other => {
                            return self.err(format!(
                                "未知转义 '\\{}' — 支持 \\n \\t \\r \\\\ \\' \\\" \\0 \\$",
                                char::from_u32(other as u32).unwrap_or('?')
                            ));
                        }
                    }
                }
                _ => {
                    let start = self.pos;
                    while let Some(n) = self.peek() {
                        if n == b'\'' || n == b'$' || n == b'\\' || n == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                    let seg = &self.src[start..self.pos];
                    lit.push_str(&String::from_utf8_lossy(seg));
                }
            }
        }

        if !lit.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(lit));
        }
        Ok(Tok::Str(parts))
    }

    fn lex_hole_until_rbrace(&mut self) -> AliasResult<Vec<(Tok, Span)>> {
        let mut depth = 1u32;
        let mut toks: Vec<(Tok, Span)> = Vec::new();
        loop {
            self.skip_trivia();
            let Some(c) = self.peek() else {
                return self.err("插值 ${...} 未闭合");
            };
            if c == b'}' {
                self.bump();
                depth -= 1;
                if depth == 0 {
                    break;
                }
                toks.push((Tok::RBrace, self.span_here(1)));
                continue;
            }
            let tok = match c {
                b'{' => {
                    self.bump();
                    depth += 1;
                    Tok::LBrace
                }
                b'\'' => self.lex_string()?,
                _ if is_ident_start(c) => self.lex_ident_or_keyword(),
                _ if c.is_ascii_digit() => self.lex_int()?,
                _ => self.lex_symbol()?,
            };
            toks.push((tok, self.span_here(1)));
        }
        Ok(toks)
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}
