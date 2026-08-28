//! parser::items — 顶层项解析。
//!
//! 拥有: 程序结构循环 ([`Parser::parse_program`])、struct 定义
//! (字段即实例内绑定)。
//! 绑定项 (含 pub? func 扩展方法定义) 委托 stmts::parse_binding。

use super::Parser;
use crate::ast::{Item, Program, StructDef, StructField};
use crate::lexer::Tok;
use crate::AliasResult;

impl Parser {
    // ---------- 程序结构 ----------

    pub(super) fn parse_program(&mut self) -> AliasResult<Program> {
        let mut items = Vec::new();
        self.skip_newlines();
        while self.peek().is_some() {
            match self.peek() {
                Some(Tok::Pub) | Some(Tok::Val) | Some(Tok::Var) | Some(Tok::Func) => {
                    items.push(Item::Binding(self.parse_binding()?));
                    self.end_stmt();
                }
                // struct 定义与绑定同属顶层项。
                Some(Tok::Struct) => {
                    items.push(self.parse_struct_def()?);
                    self.end_stmt();
                }
                other => {
                    return Err(self.err_here(format!(
                        "顶层只允许 val/var/func/pub 绑定, 实际 {:?}",
                        other.cloned()
                    )));
                }
            }
            self.skip_newlines();
        }
        Ok(Program { items })
    }

    // ---------- struct 定义：字段即实例内绑定 ----------

    /// struct <名字> { (val|var) <类型> <名字> (= 表达式)? ... }
    fn parse_struct_def(&mut self) -> AliasResult<Item> {
        let span = self.span();
        self.bump(); // struct
        let name = self.expect_ident()?;
        self.expect(&Tok::LBrace)?;
        let mut fields = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek() {
                Some(Tok::RBrace) | None => break,
                Some(Tok::Val) | Some(Tok::Var) => {
                    fields.push(self.parse_struct_field()?);
                    self.end_stmt();
                }
                other => {
                    return Err(self.err_here(format!(
                        "结构体字段必须以 val/var 开头, 实际 {:?}",
                        other.cloned()
                    )));
                }
            }
        }
        self.expect(&Tok::RBrace)?;
        Ok(Item::StructDef(StructDef { name, fields, span }))
    }

    /// 字段文法与绑定同构: 可变性词 + 强制类型槽 + 名字 + 可选默认值
    fn parse_struct_field(&mut self) -> AliasResult<StructField> {
        let span = self.span();
        let mutable = match self.peek() {
            Some(Tok::Var) => true,
            _ => false, // 调用方已保证 Val | Var
        };
        self.bump(); // val | var
        let ty = self.parse_type()?;
        let name = self.expect_ident()?;
        let default = if self.eat(&Tok::Assign) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(StructField {
            name,
            mutable,
            ty,
            default,
            span,
        })
    }
}
