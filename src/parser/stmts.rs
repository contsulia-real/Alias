//! parser::stmts — 语句解析。

use super::Parser;
use crate::ast::{BindKind, Binding, CallArg, Expr, Stmt};
use crate::lexer::Tok;
use crate::{AliasError, AliasResult, Span};

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

    pub(super) fn parse_stmt(&mut self) -> AliasResult<Stmt> {
        let span = self.span();
        match self.peek() {
            Some(Tok::Return) => {
                self.bump();
                let value = match self.peek() {
                    None | Some(Tok::Newline) | Some(Tok::Semi) | Some(Tok::RBrace) => None,
                    Some(_) => Some(self.parse_expr()?),
                };
                Ok(Stmt::Return { value, span })
            }
            Some(Tok::If) => self.parse_if_stmt(),
            Some(Tok::For) => {
                self.bump();
                let ty = self.parse_type()?;
                let name = self.expect_ident()?;
                self.expect(&Tok::In)?;
                let iterable = self.parse_expr()?;
                let body = self.parse_block()?;
                Ok(Stmt::For {
                    ty,
                    name,
                    iterable,
                    body,
                    span,
                })
            }
            Some(Tok::While) => {
                self.bump();
                let cond = self.parse_expr()?;
                let body = self.parse_block()?;
                Ok(Stmt::While { cond, body, span })
            }
            Some(Tok::Break) => {
                self.bump();
                self.require_boundary("break")?;
                Ok(Stmt::Break { span })
            }
            Some(Tok::Continue) => {
                self.bump();
                self.require_boundary("continue")?;
                Ok(Stmt::Continue { span })
            }
            Some(Tok::Pub) => Err(AliasError {
                msg: "pub 只能用于顶层绑定".into(),
                span,
            }),
            Some(Tok::Val) | Some(Tok::Var) | Some(Tok::Func) => {
                Ok(Stmt::Binding(self.parse_binding()?))
            }
            Some(Tok::Ident(_)) if self.peek_at(1) == Some(&Tok::Assign) => {
                let target = self.expect_ident()?;
                self.bump();
                let value = self.parse_expr()?;
                Ok(Stmt::Assign {
                    target,
                    value,
                    span,
                })
            }
            Some(Tok::SelfKw) if self.peek_at(1) == Some(&Tok::Assign) => {
                self.bump();
                self.bump();
                let value = self.parse_expr()?;
                Ok(Stmt::Assign {
                    target: "self".into(),
                    value,
                    span,
                })
            }
            // `println f 0` / `print f 'x'`：外层输出内建的唯一实参可以是
            // 一个完整的普通无括号单参调用。只在第二个标识符后确实存在
            // 普通无括号实参起始 token 时启用，因此 `println x + 1` 以及
            // `dup 5 + 1` 的既有“无括号绑定紧于二元运算”规则完全不变。
            Some(Tok::Ident(name))
                if matches!(name.as_str(), "println" | "print")
                    && matches!(self.peek_at(1), Some(Tok::Ident(_)))
                    && matches!(
                        self.peek_at(2),
                        Some(Tok::Int(_))
                            | Some(Tok::Float(_))
                            | Some(Tok::Bool(_))
                            | Some(Tok::Str(_))
                            | Some(Tok::LParen)
                            | Some(Tok::LBracket)
                    ) =>
            {
                let outer_name = self.expect_ident()?;
                let inner = self.parse_expr()?;
                let inner_span = inner.span();
                Ok(Stmt::Expr {
                    expr: Expr::Call {
                        callee: Box::new(Expr::Ident(outer_name, span)),
                        args: vec![CallArg {
                            label: None,
                            value: inner,
                            span: inner_span,
                        }],
                        span,
                    },
                })
            }
            Some(_) => {
                let expr = self.parse_expr()?;
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
                        return Ok(Stmt::Expr {
                            expr: Expr::Call {
                                callee: Box::new(Expr::Ident(name, id_span)),
                                args: vec![CallArg {
                                    label: None,
                                    value: arg,
                                    span: arg_span,
                                }],
                                span,
                            },
                        });
                    }
                }
                Ok(Stmt::Expr { expr })
            }
            None => Err(self.err_here("意外的文件结尾")),
        }
    }

    fn parse_if_stmt(&mut self) -> AliasResult<Stmt> {
        self.expect(&Tok::If)?;
        let first_cond = self.parse_expr()?;
        let first_body = self.parse_block()?;
        let mut branches = vec![(first_cond, first_body)];
        let mut else_body = None;

        loop {
            self.skip_newlines();
            if !self.eat(&Tok::Else) {
                break;
            }
            if self.eat(&Tok::If) {
                let cond = self.parse_expr()?;
                let body = self.parse_block()?;
                branches.push((cond, body));
                continue;
            }
            else_body = Some(self.parse_block()?);
            break;
        }

        Ok(Stmt::If {
            branches,
            else_body,
        })
    }

    fn require_boundary(&self, kw: &str) -> AliasResult<()> {
        if matches!(
            self.peek(),
            None | Some(Tok::Newline) | Some(Tok::Semi) | Some(Tok::RBrace)
        ) {
            Ok(())
        } else {
            Err(self.err_here(format!("{kw} 后不能跟值或标签")))
        }
    }

    fn assign_from_lvalue(&mut self, expr: Expr, span: Span) -> AliasResult<Stmt> {
        match expr {
            Expr::Field { recv, name, .. } => {
                self.bump();
                let value = self.parse_expr()?;
                Ok(Stmt::FieldAssign {
                    recv,
                    field: name,
                    value,
                    span,
                })
            }
            Expr::Index { .. } => Err(self.err_here("下标赋值尚未支持")),
            _ => Err(self.err_here(format!("无法开始一个表达式: {:?}", self.peek().cloned()))),
        }
    }

    // 普通绑定: (pub)? (val|var|func) <类型> <名字> = <表达式>
    // 扩展函数: (pub)? func <返回类型> <完整接收者类型>.<名字> = <函数字面量>
    pub(super) fn parse_binding(&mut self) -> AliasResult<Binding> {
        let span = self.span();
        self.eat(&Tok::Pub);
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

        if matches!(self.peek(), Some(Tok::Ident(_))) && self.peek_at(1) == Some(&Tok::Assign) {
            return Err(self.err_here(format!(
                "{:?} 绑定的类型槽不能为空 — 本语言没有类型推断, 必须显式标注",
                kind
            )));
        }
        let ty = match self.peek() {
            Some(Tok::Ident(_)) => self.parse_type()?,
            _ => {
                return Err(self.err_here(format!(
                    "{:?} 绑定的类型槽不能为空 — 本语言没有类型推断, 必须显式标注",
                    kind
                )));
            }
        };

        let (name, receiver) = if kind == BindKind::Func {
            let save = self.pos;
            match self.parse_type() {
                Ok(recv_ty) if self.peek() == Some(&Tok::Dot) => {
                    self.bump();
                    let method = self.expect_ident()?;
                    (method, Some(recv_ty))
                }
                _ => {
                    self.pos = save;
                    (self.expect_ident()?, None)
                }
            }
        } else {
            (self.expect_ident()?, None)
        };

        self.expect(&Tok::Assign)?;
        let value = self.parse_expr()?;
        if kind == BindKind::Func && !matches!(value, Expr::FuncLit { .. }) {
            let msg = match &receiver {
                Some(recv) => format!("方法 {}.{} 的体必须是函数字面量", recv.display(), name),
                None => "func 绑定必须由函数字面量初始化".into(),
            };
            let err_span = if receiver.is_some() {
                span
            } else {
                value.span()
            };
            return Err(AliasError {
                msg,
                span: err_span,
            });
        }

        Ok(Binding {
            kind,
            ty,
            name,
            receiver,
            value,
            span,
        })
    }
}
