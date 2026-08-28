use crate::ast::{CallArg, Expr};
use crate::lexer::Tok;
use crate::limits::MAX_EXPR_CHAIN;
use crate::parser::Parser;
use crate::AliasResult;

impl Parser {
    pub(super) fn starts_unary_at(&self, n: usize) -> bool {
        matches!(
            self.peek_at(n),
            Some(Tok::Ident(_))
                | Some(Tok::SelfKw)
                | Some(Tok::ThisKw)
                | Some(Tok::Int(_))
                | Some(Tok::Float(_))
                | Some(Tok::Bool(_))
                | Some(Tok::Str(_))
                | Some(Tok::LParen)
                | Some(Tok::LBracket)
                | Some(Tok::Match)
                | Some(Tok::Minus)
                | Some(Tok::Bang)
                | Some(Tok::Tilde)
        )
    }

    /// `?` 的两种用途：
    /// - `expr?`：后缀 result 传播；
    /// - `cond ? a : b`：若问号后能开始表达式，问号留给最低优先级三元层。
    fn parse_postfix_on(&mut self, mut expr: Expr) -> AliasResult<Expr> {
        let mut chain = 0usize;
        loop {
            let span = self.span();
            match self.peek().cloned() {
                Some(Tok::Dot) => {
                    chain += 1;
                    self.bump();
                    let name = self.expect_ident()?;
                    if self.peek() == Some(&Tok::LParen) {
                        let args = self.parse_args()?;
                        expr = Expr::MethodCall {
                            recv: Box::new(expr),
                            name,
                            args,
                            span,
                        };
                    } else {
                        expr = Expr::Field {
                            recv: Box::new(expr),
                            name,
                            span,
                        };
                    }
                }
                Some(Tok::LBracket) => {
                    chain += 1;
                    self.bump();
                    let idx = self.parse_expr()?;
                    self.expect(&Tok::RBracket)?;
                    expr = Expr::Index {
                        recv: Box::new(expr),
                        idx: Box::new(idx),
                        span,
                    };
                }
                Some(Tok::LParen) => {
                    chain += 1;
                    let args = self.parse_args()?;
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        args,
                        span,
                    };
                }
                Some(Tok::Question) => {
                    if self.starts_unary_at(1) {
                        break;
                    }
                    chain += 1;
                    self.bump();
                    expr = Expr::Propagate {
                        expr: Box::new(expr),
                        span,
                    };
                }
                _ => break,
            }
            if chain > MAX_EXPR_CHAIN {
                return Err(self.err_here(format!("后缀表达式超过 {MAX_EXPR_CHAIN} 项上限")));
            }
        }
        Ok(expr)
    }

    pub(in crate::parser) fn parse_unary(&mut self) -> AliasResult<Expr> {
        let mut prefixes = Vec::new();
        loop {
            let tok = match self.peek() {
                Some(Tok::Minus) => Tok::Minus,
                Some(Tok::Bang) => Tok::Bang,
                Some(Tok::Tilde) => Tok::Tilde,
                _ => break,
            };
            if prefixes.len() >= MAX_EXPR_CHAIN {
                return Err(self.err_here(format!("一元表达式超过 {MAX_EXPR_CHAIN} 层上限")));
            }
            prefixes.push((tok, self.span()));
            self.bump();
        }
        let mut expr = self.parse_postfix()?;
        for (tok, span) in prefixes.into_iter().rev() {
            expr = match tok {
                Tok::Minus => Expr::Neg {
                    expr: Box::new(expr),
                    span,
                },
                Tok::Bang => Expr::Not {
                    expr: Box::new(expr),
                    span,
                },
                Tok::Tilde => Expr::BitNot {
                    expr: Box::new(expr),
                    span,
                },
                _ => unreachable!(),
            };
        }
        Ok(expr)
    }

    fn parse_postfix(&mut self) -> AliasResult<Expr> {
        let head = self.parse_primary()?;
        self.parse_postfix_on(head)
    }

    fn parse_args(&mut self) -> AliasResult<Vec<CallArg>> {
        self.expect(&Tok::LParen)?;
        let mut args = Vec::new();
        self.skip_newlines();
        if self.peek() == Some(&Tok::RParen) {
            self.bump();
            return Ok(args);
        }
        loop {
            self.skip_newlines();
            if self.peek() == Some(&Tok::RParen) {
                break;
            }
            args.push(self.parse_call_arg()?);
            self.skip_newlines();
            if self.eat(&Tok::Comma) {
                continue;
            }
            break;
        }
        self.skip_newlines();
        self.expect(&Tok::RParen)?;
        Ok(args)
    }

    fn parse_call_arg(&mut self) -> AliasResult<CallArg> {
        if let Some(Tok::Ident(label)) = self.peek().cloned() {
            if self.peek_at(1) == Some(&Tok::Assign) {
                let span = self.span();
                self.bump();
                self.bump();
                let value = self.parse_expr()?;
                return Ok(CallArg {
                    label: Some(label),
                    value,
                    span,
                });
            }
        }
        let value = self.parse_expr()?;
        let vspan = value.span();
        Ok(CallArg {
            label: None,
            value,
            span: vspan,
        })
    }
}
