//! parser::exprs — 表达式解析。
//!
//! 优先级（高→低）：后缀/调用/方法 > 一元 - ! ~ > * / % > + - > << >>
//! > 比较 > == != > & > ^ > | > 无括号方法/调用边界 > && > || > ?:。
//! `if` 不在表达式文法中；条件取值只有 ?: / match。

use super::{validate_nesting, Parser, MAX_EXPR_CHAIN};
use crate::ast::{
    ArmBody, BinOp, Body, CallArg, CtorKind, Expr, MatchArm, Param, Pattern, StrPartAst,
};
use crate::lexer::{StrPart, Tok, Token};
use crate::{AliasError, AliasResult};

impl Parser {
    pub(super) fn parse_expr(&mut self) -> AliasResult<Expr> {
        self.parse_ternary()
    }

    /// ?: 最低优先级、右结合。后缀 result `?` 在 parse_postfix_on 中只在
    /// `?` 后不能开始表达式时消费；若后面能开始表达式，则留给本层并要求 `:`。
    fn parse_ternary(&mut self) -> AliasResult<Expr> {
        let cond = self.parse_or()?;
        if self.peek() != Some(&Tok::Question) {
            return Ok(cond);
        }
        let span = cond.span();
        self.bump();
        let then_expr = self.parse_ternary()?;
        self.expect(&Tok::Colon)?;
        let else_expr = self.parse_ternary()?;
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
            self.bump();
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
            self.bump();
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
                    let callee_is_bare_builtin = matches!(&lhs, Expr::Ident(n, _)
                    if matches!(
                        n.as_str(),
                        "println"
                            | "print"
                            | "increase"
                            | "decrease"
                            | "from"
                            | "try_from"
                            | "typeof"
                    ));
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
                        self.bump();
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
                Some(t)
                    if matches!(
                        t,
                        Tok::Int(_)
                            | Tok::Float(_)
                            | Tok::Bool(_)
                            | Tok::Str(_)
                            | Tok::LParen
                            | Tok::LBracket
                    ) =>
                {
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

    fn starts_unary_at(&self, n: usize) -> bool {
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

    fn parse_bit_or(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_bit_xor()?;
        let mut chain = 0usize;
        while self.peek() == Some(&Tok::Pipe) {
            self.bump();
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
            self.bump();
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
            self.bump();
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
            self.bump();
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
        self.bump();
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
            self.bump();
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
            self.bump();
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
            self.bump();
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
                    // 后一 token 能启动表达式 => 三元，不能在这里吞掉。
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

    pub(super) fn parse_unary(&mut self) -> AliasResult<Expr> {
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

    fn parse_primary(&mut self) -> AliasResult<Expr> {
        let span = self.span();
        match self.peek().cloned() {
            Some(Tok::Int(v)) => {
                self.bump();
                Ok(Expr::Int(v, span))
            }
            Some(Tok::Float(v)) => {
                self.bump();
                Ok(Expr::Float(v, span))
            }
            Some(Tok::Bool(b)) => {
                self.bump();
                Ok(Expr::Bool(b, span))
            }
            Some(Tok::Str(parts)) => {
                self.bump();
                let mut ast_parts = Vec::new();
                for p in parts {
                    match p {
                        StrPart::Lit(s) => ast_parts.push(StrPartAst::Lit(s)),
                        StrPart::Hole(toks) => {
                            let sub_toks: Vec<Token> = toks
                                .into_iter()
                                .map(|(tok, sp)| Token { tok, span: sp })
                                .collect();
                            validate_nesting(&sub_toks)?;
                            let mut sub = Parser {
                                toks: sub_toks,
                                pos: 0,
                            };
                            let e = sub.parse_expr().map_err(|e| AliasError {
                                msg: format!("插值内表达式错误: {}", e.msg),
                                span: e.span,
                            })?;
                            ast_parts.push(StrPartAst::Hole(Box::new(e)));
                        }
                    }
                }
                Ok(Expr::Str(ast_parts, span))
            }
            Some(Tok::LParen) => {
                if self.looks_like_func_lit() {
                    self.parse_func_lit()
                } else if self.looks_like_cast() {
                    self.bump();
                    let target = self.parse_type()?;
                    self.expect(&Tok::RParen)?;
                    let expr = self.parse_unary()?;
                    Ok(Expr::Cast {
                        target,
                        expr: Box::new(expr),
                        span,
                    })
                } else {
                    self.bump();
                    if self.peek() == Some(&Tok::RParen) {
                        self.bump();
                        return Err(AliasError {
                            msg: "() 不是值；unit 只表示函数不返回值".into(),
                            span,
                        });
                    }
                    let e = self.parse_expr()?;
                    self.expect(&Tok::RParen)?;
                    Ok(e)
                }
            }
            Some(Tok::Ident(_)) => {
                let name = self.expect_ident()?;
                Ok(Expr::Ident(name, span))
            }
            Some(Tok::SelfKw) => {
                self.bump();
                Ok(Expr::Ident("self".into(), span))
            }
            Some(Tok::ThisKw) => {
                self.bump();
                Ok(Expr::This(span))
            }
            Some(Tok::Match) => self.parse_match_expr(),
            Some(Tok::LBracket) => self.parse_array_lit(),
            // if 永远不是表达式；让这里产生明确诊断而非创建 Expr::If。
            Some(Tok::If) => Err(self.err_here("if 只能作为语句使用；条件取值请使用 ?: 或 match")),
            other => Err(self.err_here(format!("无法开始一个表达式: {:?}", other))),
        }
    }

    fn parse_array_lit(&mut self) -> AliasResult<Expr> {
        let span = self.span();
        self.bump();
        let mut elems = Vec::new();
        loop {
            self.skip_newlines();
            if self.peek() == Some(&Tok::RBracket) || self.peek().is_none() {
                break;
            }
            elems.push(self.parse_expr()?);
            self.skip_newlines();
            if self.eat(&Tok::Comma) {
                continue;
            }
            break;
        }
        self.skip_newlines();
        self.expect(&Tok::RBracket)?;
        Ok(Expr::ArrayLit { elems, span })
    }

    fn parse_match_expr(&mut self) -> AliasResult<Expr> {
        let span = self.span();
        self.bump();
        let subject = self.parse_expr()?;
        self.expect(&Tok::LBrace)?;
        let mut arms = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek() {
                Some(Tok::RBrace) | None => break,
                _ => {
                    arms.push(self.parse_match_arm()?);
                    self.skip_newlines();
                    self.eat(&Tok::Comma);
                }
            }
        }
        self.skip_newlines();
        self.expect(&Tok::RBrace)?;
        Ok(Expr::Match {
            subject: Box::new(subject),
            arms,
            span,
        })
    }

    fn parse_match_arm(&mut self) -> AliasResult<MatchArm> {
        let span = self.span();
        let pattern = self.parse_pattern()?;
        self.expect(&Tok::Arrow)?;
        let body = if self.peek() == Some(&Tok::LBrace) {
            ArmBody::Block(self.parse_block()?)
        } else if self.eat(&Tok::Return) {
            ArmBody::Ret(Box::new(self.parse_expr()?))
        } else {
            ArmBody::Value(Box::new(self.parse_expr()?))
        };
        Ok(MatchArm {
            pattern,
            body,
            span,
        })
    }

    fn parse_pattern(&mut self) -> AliasResult<Pattern> {
        let span = self.span();
        match self.peek().cloned() {
            Some(Tok::Ident(name))
                if matches!(name.as_str(), "ok" | "err")
                    && self.peek_at(1) == Some(&Tok::LParen) =>
            {
                self.bump();
                let ctor = if name == "ok" {
                    CtorKind::Ok
                } else {
                    CtorKind::Err
                };
                self.expect(&Tok::LParen)?;
                let binding = match self.peek().cloned() {
                    Some(Tok::Ident(n)) => {
                        self.bump();
                        if n == "_" {
                            None
                        } else {
                            Some(n)
                        }
                    }
                    other => {
                        return Err(self.err_here(format!(
                            "result 构造器 Pattern 的载荷必须是标识符或 _, 实际 {:?}",
                            other
                        )));
                    }
                };
                self.expect(&Tok::RParen)?;
                Ok(Pattern::Constructor {
                    ctor,
                    binding,
                    span,
                })
            }
            Some(Tok::Ident(name)) => {
                self.bump();
                if name == "_" {
                    Ok(Pattern::Wildcard { span })
                } else {
                    Ok(Pattern::Binding { name, span })
                }
            }
            Some(Tok::Minus) if matches!(self.peek_at(1), Some(Tok::Int(_))) => {
                self.bump();
                let Some(Tok::Int(v)) = self.peek().cloned() else {
                    unreachable!()
                };
                self.bump();
                let value = -(v as i128);
                Ok(Pattern::Int { value, span })
            }
            Some(Tok::Int(value)) => {
                self.bump();
                Ok(Pattern::Int {
                    value: value as i128,
                    span,
                })
            }
            Some(Tok::Bool(value)) => {
                self.bump();
                Ok(Pattern::Bool { value, span })
            }
            Some(Tok::Str(parts)) => {
                self.bump();
                let mut value = String::new();
                for part in parts {
                    match part {
                        StrPart::Lit(s) => value.push_str(&s),
                        StrPart::Hole(_) => {
                            return Err(AliasError {
                                msg: "match 字符串 Pattern 必须是纯字面量".into(),
                                span,
                            });
                        }
                    }
                }
                Ok(Pattern::Str { value, span })
            }
            other => Err(self.err_here(format!("无法开始 match Pattern: {:?}", other))),
        }
    }

    fn looks_like_func_lit(&self) -> bool {
        let mut depth: u32 = 0;
        let mut i = self.pos;
        while let Some(t) = self.toks.get(i) {
            match t.tok {
                Tok::LParen | Tok::LBrace | Tok::LBracket => depth += 1,
                Tok::RParen | Tok::RBrace | Tok::RBracket => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return matches!(self.toks.get(i + 1).map(|t| &t.tok), Some(Tok::Arrow));
                    }
                }
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// 显式转换仅接受 `(NamedType) unary`。泛型类型当前没有转换规则，
    /// 因此不为 `(array<i32>) value` 预留无语义的语法入口。
    fn looks_like_cast(&self) -> bool {
        matches!(self.peek_at(0), Some(Tok::LParen))
            && matches!(self.peek_at(1), Some(Tok::Ident(_)) | Some(Tok::Func))
            && matches!(self.peek_at(2), Some(Tok::RParen))
            && matches!(
                self.peek_at(3),
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

    fn parse_func_lit(&mut self) -> AliasResult<Expr> {
        let span = self.span();
        self.expect(&Tok::LParen)?;
        let mut params = Vec::new();
        self.skip_newlines();
        if !self.eat(&Tok::RParen) {
            loop {
                self.skip_newlines();
                let p_span = self.span();
                let ty = self.parse_type()?;
                let name = self.expect_ident()?;
                params.push(Param {
                    ty,
                    name,
                    span: p_span,
                });
                self.skip_newlines();
                if self.eat(&Tok::Comma) {
                    continue;
                }
                break;
            }
            self.skip_newlines();
            self.expect(&Tok::RParen)?;
        }
        self.expect(&Tok::Arrow)?;
        let body = self.parse_func_body()?;
        Ok(Expr::FuncLit {
            params,
            body: Box::new(body),
            span,
        })
    }

    /// 无花括号体 = 恰好一条真正的语句。绝不把裸表达式改写成 return。
    fn parse_func_body(&mut self) -> AliasResult<Body> {
        if self.peek() == Some(&Tok::LBrace) {
            Ok(Body::Block(self.parse_block()?))
        } else {
            Ok(Body::Single(Box::new(self.parse_stmt()?)))
        }
    }
}
