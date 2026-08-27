//! 递归下降 Parser。

mod exprs;
mod items;
mod stmts;

use crate::ast::{Program, TypeExpr};
use crate::lexer::{Tok, Token};
use crate::{AliasError, AliasResult, Span};

pub fn parse(tokens: Vec<Token>) -> AliasResult<Program> {
    validate_nesting(&tokens)?;
    let mut p = Parser { toks: tokens, pos: 0 };
    p.parse_program()
}

pub(super) const MAX_EXPR_CHAIN: usize = 256;
const MAX_NESTING: usize = 128;

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
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.tok)
    }

    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.pos + n).map(|t| &t.tok)
    }

    fn span(&self) -> Span {
        self.toks.get(self.pos).map(|t| t.span).unwrap_or_default()
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        self.pos += 1;
        t
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == Some(tok) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: &Tok) -> AliasResult<Token> {
        if self.peek() == Some(tok) {
            Ok(self.bump())
        } else {
            Err(self.err_here(format!("期望 {:?}, 实际 {:?}", tok, self.peek().cloned())))
        }
    }

    fn err_here(&self, msg: impl Into<String>) -> AliasError {
        AliasError { msg: msg.into(), span: self.span() }
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
                self.bump();
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
        if depth >= MAX_NESTING {
            return Err(self.err_here(format!("类型嵌套超过 {MAX_NESTING} 层上限")));
        }
        let name = match self.peek().cloned() {
            Some(Tok::Ident(n)) => {
                self.bump();
                n
            }
            Some(Tok::Func) => {
                self.bump();
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
            self.expect(&Tok::Gt)?;
            Ok(TypeExpr::Generic(name, args))
        } else {
            Ok(TypeExpr::Named(name))
        }
    }
}
