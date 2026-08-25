//! parser::stmts — 语句解析。
//!
//! 拥有: 块解析 ([`Parser::parse_block`])、语句分派
//! ([`Parser::parse_stmt`], 含无括号单参调用吞参与字段链赋值承接)、
//! 左值赋值收口 ([`Parser::assign_from_lvalue`])、绑定声明解析
//! ([`Parser::parse_binding`], 顶层项与语句位共用; 扩展方法接收者
//! 在此拆出)。

use super::Parser;
use crate::ast::{BindKind, Binding, CallArg, Expr, Stmt};
use crate::lexer::Tok;
use crate::{AliasResult, Span};

impl Parser {
    pub(super) fn parse_block(&mut self) -> AliasResult<Vec<Stmt>> {
        self.expect(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek() {
                Some(Tok::RBrace) | None => break,
                _ => {
                    stmts.push(self.parse_stmt()?);
                    self.end_stmt();
                }
            }
        }
        self.expect(&Tok::RBrace)?;
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> AliasResult<Stmt> {
        let span = self.span();
        match self.peek() {
            Some(Tok::Return) => {
                self.bump();
                // return 后可无表达式? demo 里 return 总带值; 宽容支持裸 return
                let value = match self.peek() {
                    None | Some(Tok::Newline) | Some(Tok::Semi) | Some(Tok::RBrace) => None,
                    Some(_) => Some(self.parse_expr()?),
                };
                Ok(Stmt::Return { value, span })
            }
            Some(Tok::For) => {
                self.bump();
                let cond = self.parse_expr()?;
                let body = self.parse_block()?;
                Ok(Stmt::For { cond, body, span })
            }
            Some(Tok::While) => {
                self.bump();
                let cond = self.parse_expr()?;
                let body = self.parse_block()?;
                Ok(Stmt::While { cond, body, span })
            }
            Some(Tok::Val) | Some(Tok::Var) | Some(Tok::Func) | Some(Tok::Public) => {
                Ok(Stmt::Binding(self.parse_binding()?))
            }
            // 赋值语句: target = expr (P1 已裁决)
            Some(Tok::Ident(_)) if self.peek_at(1) == Some(&Tok::Assign) => {
                let target = self.expect_ident()?;
                self.bump(); // =
                let value = self.parse_expr()?;
                Ok(Stmt::Assign { target, value, span })
            }
            // self = expr (Phase 2c): 解析为普通赋值 — val 语义由 sema 拒绝
            Some(Tok::SelfKw) if self.peek_at(1) == Some(&Tok::Assign) => {
                self.bump(); // self
                self.bump(); // =
                let value = self.parse_expr()?;
                Ok(Stmt::Assign { target: "self".into(), value, span })
            }
            // 其余一律按表达式语句处理:
            //   带括号调用 cond(10) / 方法调用 ch.sender()
            //   无括号调用 increase i / println msg — 裸标识符后
            //   同行还跟着一个一元表达式起点时, 它被吞作唯一实参
            //   (文法上限 <= 1 参, 已裁决)
            Some(_) => {
                let expr = self.parse_expr()?;
                // 表达式后紧跟 '=' → 赋值语句 (简名已在上方特判;
                // 此处承接 Phase 2a 字段链 recv.field = expr)
                if self.peek() == Some(&Tok::Assign) {
                    return self.assign_from_lvalue(expr, span);
                }
                let bare = match &expr {
                    Expr::Ident(n, s) => Some((n.clone(), *s)),
                    _ => None,
                };
                if let Some((name, id_span)) = bare {
                    let at_boundary = matches!(
                        self.peek(),
                        None | Some(Tok::Newline) | Some(Tok::Semi) | Some(Tok::RBrace)
                    );
                    if !at_boundary {
                        let arg = self.parse_unary()?;
                        let arg_span = arg.span();
                        return Ok(Stmt::ExprStmt {
                            expr: Expr::Call {
                                callee: Box::new(Expr::Ident(name, id_span)),
                                args: vec![CallArg { label: None, value: arg, span: arg_span }],
                                span,
                            },
                            span,
                        });
                    }
                }
                Ok(Stmt::ExprStmt { expr, span })
            }
            None => Err(self.err_here("意外的文件结尾")),
        }
    }

    /// '=' 前的表达式必须是左值形态: 简名已在语句入口特判,
    /// 此处承接字段链; 下标目标 Phase 2d 明确拒绝 (只读索引裁决);
    /// 其余维持既有报错形态 ('=' 处无法开始表达式)
    fn assign_from_lvalue(&mut self, expr: Expr, span: Span) -> AliasResult<Stmt> {
        match expr {
            Expr::Field { recv, name, .. } => {
                self.bump(); // =
                let value = self.parse_expr()?;
                Ok(Stmt::FieldAssign { recv, field: name, value, span })
            }
            Expr::Index { .. } => Err(self.err_here("下标赋值尚未支持")),
            _ => Err(self.err_here(format!(
                "无法开始一个表达式: {:?}",
                self.peek().cloned()
            ))),
        }
    }

    // ---------- 绑定: (public)? (val|var|func) 类型 名字 = 表达式 ----------

    pub(super) fn parse_binding(&mut self) -> AliasResult<Binding> {
        let span = self.span();
        let public = self.eat(&Tok::Public);
        let kind = match self.peek() {
            Some(Tok::Val) => {
                self.bump();
                BindKind::Val
            }
            Some(Tok::Var) => {
                self.bump();
                BindKind::Var
            }
            Some(Tok::Func) => {
                self.bump();
                BindKind::Func
            }
            _ => return Err(self.err_here("绑定声明必须以 val/var/func 开头")),
        };

        // 类型槽强制非空 — 无推断
        // 前瞻 [Ident, Assign]: 名字后直接 '=', 说明类型槽被省略
        if matches!(self.peek(), Some(Tok::Ident(_))) && self.peek_at(1) == Some(&Tok::Assign)
        {
            return Err(self.err_here(format!(
                "{:?} 绑定的类型槽不能为空 — 本语言没有类型推断, 必须显式标注",
                kind
            )));
        }
        let ty = match self.peek() {
            Some(Tok::Ident(_)) | Some(Tok::Bool(true)) | Some(Tok::Bool(false)) => {
                self.parse_type()?
            }
            _ => {
                return Err(self.err_here(format!(
                    "{:?} 绑定的类型槽不能为空 — 本语言没有类型推断, 必须显式标注",
                    kind
                )));
            }
        };

        // 名字允许点路径(string.append 扩展方法定义)
        let name_tok = self.expect_ident()?;
        let mut name = name_tok;
        while self.peek() == Some(&Tok::Dot) {
            if matches!(self.peek_at(1), Some(Tok::Ident(_))) {
                self.bump(); // .
                name.push('.');
                name.push_str(&self.expect_ident()?);
            } else {
                break;
            }
        }

        // Phase 2c: func 绑定的单点路径名 = 扩展方法定义
        // (func <Ret> <RecvType>.<method>) — 拆出接收者, 名字归位为方法名。
        // 多点路径与非 func 绑定维持既有形态 (带点名字的普通绑定)。
        let mut receiver = None;
        if kind == BindKind::Func {
            let dotted: Vec<String> = name.split('.').map(str::to_string).collect();
            if dotted.len() == 2 {
                name = dotted[1].clone();
                receiver = Some((dotted[0].clone(), dotted[1].clone()));
            }
        }

        self.expect(&Tok::Assign)?;
        let value = if kind == BindKind::Func {
            // func 绑定的值几乎总是函数字面量; 也允许引用既有函数值
            self.parse_expr()?
        } else {
            self.parse_expr()?
        };

        Ok(Binding { public, kind, ty, name, receiver, value, span })
    }
}
