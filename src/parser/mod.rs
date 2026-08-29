//! 递归下降 Parser。

mod exprs;
mod items;
mod stmts;

use crate::ast::{Program, TypeExpr};
use crate::lexer::{Tok, Token};
use crate::limits::MAX_NESTING;
use crate::{AliasError, AliasResult, Span};

pub fn parse(tokens: Vec<Token>) -> AliasResult<Program> {
    validate_nesting(&tokens)?;
    let mut p = Parser::new(tokens);
    p.parse_program()
}

fn validate_nesting(tokens: &[Token]) -> AliasResult<()> {
    let mut depth = 0usize;
    for t in tokens {
        match t.tok {
            Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
            Tok::RParen | Tok::RBracket | Tok::RBrace => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth > MAX_NESTING {
            return Err(AliasError {
                msg: format!("语法嵌套超过 {MAX_NESTING} 层上限"),
                span: t.span,
            });
        }
    }
    Ok(())
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// EOF 仍是具体源码位置。全零 Span 只留给没有源码位置的诊断。
    eof_span: Span,
}

impl Parser {
    fn new(toks: Vec<Token>) -> Self {
        let eof_span = toks.last().map_or(
            Span {
                line: 1,
                col: 1,
                len: 0,
            },
            |token| Span {
                line: token.span.line,
                col: token.span.col.saturating_add(token.span.len),
                len: 0,
            },
        );
        Self {
            toks,
            pos: 0,
            eof_span,
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.tok)
    }

    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.pos + n).map(|t| &t.tok)
    }

    fn span(&self) -> Span {
        self.toks
            .get(self.pos)
            .map(|t| t.span)
            .unwrap_or(self.eof_span)
    }

    fn bump(&mut self) -> AliasResult<Token> {
        let Some(token) = self.toks.get(self.pos).cloned() else {
            return Err(self.err_here("意外的文件结尾"));
        };
        self.pos += 1;
        Ok(token)
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == Some(tok) {
            // 已通过 get-backed peek 证明当前位置存在；直接推进可保持 eat 的 bool API，
            // 同时避免让不可信 EOF 通过下标或 unwrap 变成 parser panic。
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: &Tok) -> AliasResult<Token> {
        if self.peek() == Some(tok) {
            self.bump()
        } else {
            Err(self.err_here(format!("期望 {:?}, 实际 {:?}", tok, self.peek().cloned())))
        }
    }

    fn expect_eof(&self) -> AliasResult<()> {
        if self.peek().is_none() {
            Ok(())
        } else {
            Err(self.err_here(format!(
                "表达式结束后存在意外 token {:?}",
                self.peek().cloned()
            )))
        }
    }

    fn err_here(&self, msg: impl Into<String>) -> AliasError {
        AliasError {
            msg: msg.into(),
            span: self.span(),
        }
    }

    fn end_stmt(&mut self) {
        self.eat(&Tok::Semi);
        while self.eat(&Tok::Newline) {}
    }

    fn skip_newlines(&mut self) {
        while self.eat(&Tok::Newline) {}
    }

    fn expect_ident(&mut self) -> AliasResult<String> {
        match self.peek().cloned() {
            Some(Tok::Ident(n)) => {
                self.bump()?;
                Ok(n)
            }
            other => Err(self.err_here(format!("期望标识符, 实际 {:?}", other))),
        }
    }

    /// 类型表达式。`func` 同时是声明关键字和类型名，在类型槽位置降为 Named("func")。
    fn parse_type(&mut self) -> AliasResult<TypeExpr> {
        self.parse_type_at_depth(0)
    }

    fn parse_type_at_depth(&mut self, depth: usize) -> AliasResult<TypeExpr> {
        // 泛型尖括号不计入 delimiter validate_nesting，但本函数会为每层参数递归；这里
        // 必须独立限深，否则 `array<array<...>>` 可在 token 预算内耗尽宿主栈。
        if depth >= MAX_NESTING {
            return Err(self.err_here(format!("类型嵌套超过 {MAX_NESTING} 层上限")));
        }
        let name = match self.peek().cloned() {
            Some(Tok::Ident(n)) => {
                self.bump()?;
                n
            }
            Some(Tok::Func) => {
                self.bump()?;
                "func".to_string()
            }
            Some(Tok::Bool(_)) => return Err(self.err_here("bool 才是布尔类型名")),
            other => return Err(self.err_here(format!("期望类型名, 实际 {:?}", other))),
        };

        if self.eat(&Tok::Lt) {
            let mut args = Vec::new();
            loop {
                args.push(self.parse_type_at_depth(depth + 1)?);
                if self.eat(&Tok::Comma) {
                    continue;
                }
                break;
            }
            self.expect_type_gt()?;
            Ok(TypeExpr::Generic(name, args))
        } else {
            Ok(TypeExpr::Named(name))
        }
    }

    /// 类型上下文把 lexer 合并出的 `>>` 按两个泛型右尖括号消费。
    /// 表达式上下文仍保留单个 `Shr`，不会改变右移运算符的词法结果。
    fn expect_type_gt(&mut self) -> AliasResult<()> {
        match self.peek() {
            Some(Tok::Gt) => {
                self.bump()?;
                Ok(())
            }
            Some(Tok::Shr) => {
                let token = &mut self.toks[self.pos];
                token.tok = Tok::Gt;
                token.span.col = token.span.col.saturating_add(1);
                token.span.len = 1;
                Ok(())
            }
            _ => Err(self.err_here(format!(
                "期望 {:?}, 实际 {:?}",
                Tok::Gt,
                self.peek().cloned()
            ))),
        }
    }
}
