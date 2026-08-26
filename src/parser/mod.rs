//! 递归下降 Parser。
//!
//! 关键裁决的落地位置:
//! - 分号 Kotlin 式可省略: 语句边界 = 显式 ';' 或 Newline;
//!   括号内的 Newline 已被 lexer 吞掉; 行首 '.' 续行同理
//! - 无括号调用仅当参数量 <= 1: 语句起始 Ident 后不跟 '='/'(' 时,
//!   吃一个一元级表达式作为参数(increase i / println x)
//! - 类型槽强制非空: val/var/func 后必须跟类型, 缺失即报错 —
//!   "语言没有类型推断"的法律在语法层执行
//! - 命名实参歧义裁决 (Phase 2a): `Ident = expr` (单 '=', lexer 已把
//!   '==' 折成 EqEq) 统一解析为带标签实参 — 被调方是结构体还是函数
//!   由 sema 裁决, parser 不预判
//!
//! 模块划分 (纯机械拆分, 无逻辑改动):
//! - [`items`]: 顶层项 (func 定义 / 扩展方法接收者拆分 / struct 定义 /
//!   import 解析)
//! - [`stmts`]: 语句解析 (绑定 / 赋值 / 表达式语句 / return / for /
//!   while / 块)
//! - [`exprs`]: 表达式 (前缀 / 中缀优先级 / 后缀链 call-field-index-? /
//!   match / 插值切分 / 命名实参探测)

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
            Tok::RParen | Tok::RBracket | Tok::RBrace => {
                depth = depth.saturating_sub(1)
            }
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
    // ---------- 基础导航 ----------

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

    /// 语句收尾: 可选 ';' + 任意换行/空行。
    /// 分号与换行都缺席时静默通过(箭头体等自含边界的场景)。
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

    /// 类型表达式: i32 / bool / string / array<string> / sender<string> 等
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
            // true/false 被 lexer 当关键字了, 但作为类型名不该出现 — 报清晰错误
            Some(Tok::Bool(_)) => {
                return Err(self.err_here("bool 才是布尔类型名"));
            }
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
