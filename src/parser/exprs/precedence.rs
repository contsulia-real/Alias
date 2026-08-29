use super::super::Parser;
use crate::ast::{BinOp, CallArg, Expr};
use crate::builtins::is_no_paren_builtin;
use crate::lexer::Tok;
use crate::limits::MAX_EXPR_CHAIN;
use crate::{AliasError, AliasResult};

impl Parser {
    pub(in crate::parser) fn parse_expr(&mut self) -> AliasResult<Expr> {
        self.parse_ternary_at_depth(0)
    }

    /// ?: 最低优先级、右结合。后缀 result `?` 在 parse_postfix_on 中只在
    /// `?` 后不能开始表达式时消费；若后面能开始表达式，则留给本层并要求 `:`。
    fn parse_ternary_at_depth(&mut self, depth: usize) -> AliasResult<Expr> {
        let cond = self.parse_or()?;
        if self.peek() != Some(&Tok::Question) {
            return Ok(cond);
        }
        // 三元表达式的右结合递归不增加任何括号/方括号/花括号深度，因此通用
        // validate_nesting 无法约束它。这里必须独立应用表达式链预算，否则纯 token
        // 链可以在到达 token 上限前耗尽编译器工作线程栈。
        if depth >= MAX_EXPR_CHAIN {
            return Err(self.err_here(format!("三元表达式超过 {MAX_EXPR_CHAIN} 层上限")));
        }
        let span = cond.span();
        self.bump()?;
        let then_expr = self.parse_ternary_at_depth(depth + 1)?;
        self.expect(&Tok::Colon)?;
        let else_expr = self.parse_ternary_at_depth(depth + 1)?;
        Ok(Expr::Ternary {
            cond: Box::new(cond),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
            span,
        })
    }

    fn parse_or(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_and()?;
        let mut chain = 0usize;
        while self.peek() == Some(&Tok::OrOr) {
            self.bump()?;
            chain += 1;
            if chain > MAX_EXPR_CHAIN {
                return Err(self.err_here(format!("逻辑或表达式超过 {MAX_EXPR_CHAIN} 项上限")));
            }
            let rhs = self.parse_and()?;
            let span = lhs.span();
            lhs = Expr::Binary {
                op: BinOp::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_no_paren()?;
        let mut chain = 0usize;
        while self.peek() == Some(&Tok::AndAnd) {
            self.bump()?;
            chain += 1;
            if chain > MAX_EXPR_CHAIN {
                return Err(self.err_here(format!("逻辑与表达式超过 {MAX_EXPR_CHAIN} 项上限")));
            }
            let rhs = self.parse_no_paren()?;
            let span = lhs.span();
            lhs = Expr::Binary {
                op: BinOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    /// 无括号调用与方法中缀共享这一边界，但 parser 只做 token 形状能确定的裁决：
    /// - 预定义无括号调用名由 builtins owner 分类，不在 parser 复制名字表；
    /// - `a XXX b` 明确是 `a.XXX(b)`；
    /// - `f x` / `value method` 两项邻接保留为 Juxtapose，交给 sema 根据 lhs 静态类型裁决；
    /// - 非标识符实参的 `f 1`、`f 'x'` 等直接是单参函数调用。
    fn parse_no_paren(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_bit_or()?;
        let mut chain = 0usize;
        loop {
            let span = self.span();
            match self.peek().cloned() {
                Some(Tok::Ident(m)) => {
                    chain += 1;
                    if chain > MAX_EXPR_CHAIN {
                        return Err(self.err_here(format!("表达式链超过 {MAX_EXPR_CHAIN} 项上限")));
                    }
                    let callee_is_bare_builtin =
                        matches!(&lhs, Expr::Ident(n, _) if is_no_paren_builtin(n));
                    if callee_is_bare_builtin {
                        let arg = self.parse_unary()?;
                        let a_span = arg.span();
                        let args = vec![CallArg {
                            label: None,
                            value: arg,
                            span: a_span,
                        }];
                        let c_span = lhs.span();
                        lhs = Expr::Call {
                            callee: Box::new(lhs),
                            args,
                            span: c_span,
                        };
                        if self.starts_unary_at(0) {
                            return Err(AliasError {
                                msg: "无括号调用的实参后不能直接链接表达式 — 请使用括号".into(),
                                span: self.span(),
                            });
                        }
                        continue;
                    }
                    if self.starts_unary_at(1) {
                        self.bump()?;
                        let a = self.parse_unary()?;
                        let a_span = a.span();
                        let span = lhs.span();
                        lhs = Expr::MethodCall {
                            recv: Box::new(lhs),
                            name: m,
                            args: vec![CallArg {
                                label: None,
                                value: a,
                                span: a_span,
                            }],
                            span,
                        };
                    } else {
                        let rhs = self.parse_unary()?;
                        let span = lhs.span();
                        lhs = Expr::Juxtapose {
                            lhs: Box::new(lhs),
                            rhs: Box::new(rhs),
                            span,
                        };
                    }
                }
                Some(
                    Tok::Int(_)
                    | Tok::Float(_)
                    | Tok::Bool(_)
                    | Tok::Str(_)
                    | Tok::LParen
                    | Tok::LBracket,
                ) => {
                    chain += 1;
                    if chain > MAX_EXPR_CHAIN {
                        return Err(self.err_here(format!("表达式链超过 {MAX_EXPR_CHAIN} 项上限")));
                    }
                    let callee = match &lhs {
                        Expr::Ident(n, s) => Expr::Ident(n.clone(), *s),
                        _ => {
                            return Err(AliasError {
                                msg: "意外的表达式".into(),
                                span,
                            })
                        }
                    };
                    let a = self.parse_unary()?;
                    let a_span = a.span();
                    let c_span = callee.span();
                    lhs = Expr::Call {
                        callee: Box::new(callee),
                        args: vec![CallArg {
                            label: None,
                            value: a,
                            span: a_span,
                        }],
                        span: c_span,
                    };
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn parse_bit_or(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_bit_xor()?;
        let mut chain = 0usize;
        while self.peek() == Some(&Tok::Pipe) {
            self.bump()?;
            chain += 1;
            if chain > MAX_EXPR_CHAIN {
                return Err(self.err_here(format!("位或表达式超过 {MAX_EXPR_CHAIN} 项上限")));
            }
            let rhs = self.parse_bit_xor()?;
            let span = lhs.span();
            lhs = Expr::Binary {
                op: BinOp::BitOr,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_bit_xor(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_bit_and()?;
        let mut chain = 0usize;
        while self.peek() == Some(&Tok::Caret) {
            self.bump()?;
            chain += 1;
            if chain > MAX_EXPR_CHAIN {
                return Err(self.err_here(format!("位异或表达式超过 {MAX_EXPR_CHAIN} 项上限")));
            }
            let rhs = self.parse_bit_and()?;
            let span = lhs.span();
            lhs = Expr::Binary {
                op: BinOp::BitXor,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_bit_and(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_equality()?;
        let mut chain = 0usize;
        while self.peek() == Some(&Tok::Amp) {
            self.bump()?;
            chain += 1;
            if chain > MAX_EXPR_CHAIN {
                return Err(self.err_here(format!("位与表达式超过 {MAX_EXPR_CHAIN} 项上限")));
            }
            let rhs = self.parse_equality()?;
            let span = lhs.span();
            lhs = Expr::Binary {
                op: BinOp::BitAnd,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_equality(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_comparison()?;
        let mut chain = 0usize;
        loop {
            let op = match self.peek() {
                Some(Tok::EqEq) => BinOp::EqEq,
                Some(Tok::NotEq) => BinOp::NotEq,
                _ => break,
            };
            self.bump()?;
            chain += 1;
            if chain > MAX_EXPR_CHAIN {
                return Err(self.err_here(format!("相等比较表达式超过 {MAX_EXPR_CHAIN} 项上限")));
            }
            let rhs = self.parse_comparison()?;
            let span = lhs.span();
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_comparison(&mut self) -> AliasResult<Expr> {
        let lhs = self.parse_shift()?;
        let op = match self.peek() {
            Some(Tok::Lt) => BinOp::Lt,
            Some(Tok::Le) => BinOp::Le,
            Some(Tok::Gt) => BinOp::Gt,
            Some(Tok::Ge) => BinOp::Ge,
            _ => return Ok(lhs),
        };
        self.bump()?;
        let rhs = self.parse_shift()?;
        let span = lhs.span();
        Ok(Expr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            span,
        })
    }

    fn parse_shift(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_additive()?;
        let mut chain = 0usize;
        loop {
            let op = match self.peek() {
                Some(Tok::Shl) => BinOp::Shl,
                Some(Tok::Shr) => BinOp::Shr,
                _ => break,
            };
            self.bump()?;
            chain += 1;
            if chain > MAX_EXPR_CHAIN {
                return Err(self.err_here(format!("移位表达式超过 {MAX_EXPR_CHAIN} 项上限")));
            }
            let rhs = self.parse_additive()?;
            let span = lhs.span();
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_multiplicative()?;
        let mut chain = 0usize;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                _ => break,
            };
            self.bump()?;
            chain += 1;
            if chain > MAX_EXPR_CHAIN {
                return Err(self.err_here(format!("加减表达式超过 {MAX_EXPR_CHAIN} 项上限")));
            }
            let rhs = self.parse_multiplicative()?;
            let span = lhs.span();
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_unary()?;
        let mut chain = 0usize;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::Percent) => BinOp::Rem,
                _ => break,
            };
            self.bump()?;
            chain += 1;
            if chain > MAX_EXPR_CHAIN {
                return Err(self.err_here(format!("乘除余表达式超过 {MAX_EXPR_CHAIN} 项上限")));
            }
            let rhs = self.parse_unary()?;
            let span = lhs.span();
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }
}
