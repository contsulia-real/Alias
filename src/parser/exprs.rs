//! parser::exprs — 表达式解析 (优先级爬升)。
//!
//! 拥有: 中缀优先级链 (comparison → additive → multiplicative → unary)、
//! 后缀链 (.field / .method(args) / [idx] / (args) / ?)、实参表与
//! 命名实参探测、match 文法 (ok/err 臂, file_wc.as 34-52 冻结形状)、
//! 字符串插值子表达式切分、函数字面量判定与解析。

use super::Parser;
use crate::ast::{
    ArmBody, BinOp, Body, CallArg, CtorKind, Expr, MatchArm, Param, StrPartAst,
};
use crate::lexer::{StrPart, Tok, Token};
use crate::{AliasError, AliasResult};

impl Parser {
    // ---------- 表达式 (优先级爬升) ----------

    pub(super) fn parse_expr(&mut self) -> AliasResult<Expr> {
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> AliasResult<Expr> {
        let lhs = self.parse_additive()?;
        let op = match self.peek() {
            Some(Tok::Lt) => BinOp::Lt,
            Some(Tok::Le) => BinOp::Le,
            Some(Tok::Gt) => BinOp::Gt,
            Some(Tok::Ge) => BinOp::Ge,
            Some(Tok::EqEq) => BinOp::EqEq,
            Some(Tok::NotEq) => BinOp::NotEq,
            _ => return Ok(lhs),
        };
        self.bump();
        let rhs = self.parse_additive()?;
        let span = lhs.span();
        Ok(Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), span })
    }

    fn parse_additive(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_multiplicative()?;
            let span = lhs.span();
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), span };
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> AliasResult<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_unary()?;
            let span = lhs.span();
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), span };
        }
        Ok(lhs)
    }

    pub(super) fn parse_unary(&mut self) -> AliasResult<Expr> {
        if self.peek() == Some(&Tok::Minus) {
            let span = self.span();
            self.bump();
            let expr = self.parse_unary()?;
            return Ok(Expr::Neg { expr: Box::new(expr), span });
        }
        self.parse_postfix()
    }

    /// primary + 后缀链: .field / .method(args) / [idx] / (args) / ?
    fn parse_postfix(&mut self) -> AliasResult<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            let span = self.span();
            match self.peek().cloned() {
                Some(Tok::Dot) => {
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
                        expr = Expr::Field { recv: Box::new(expr), name, span };
                    }
                }
                Some(Tok::LBracket) => {
                    self.bump();
                    let idx = self.parse_expr()?;
                    self.expect(&Tok::RBracket)?;
                    expr = Expr::Index { recv: Box::new(expr), idx: Box::new(idx), span };
                }
                Some(Tok::LParen) => {
                    let args = self.parse_args()?;
                    expr = Expr::Call { callee: Box::new(expr), args, span };
                }
                // ? 传播糖 (P6): 与字段访问/调用同级的后缀
                Some(Tok::Question) => {
                    self.bump();
                    expr = Expr::Propagate { expr: Box::new(expr), span };
                }
                _ => break,
            }
        }
        Ok(expr)
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
            // 尾逗号容忍 (Phase 2b): file_wc.as 构造实参跨行书写带尾逗号
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

    /// 命名/位置实参共用语法空间 (用户裁决): 当前 Ident 且下一 token 为单
    /// '=' (lexer 已把 '==' 折成 EqEq) 时解析为标签; 合法性归 sema 裁决
    fn parse_call_arg(&mut self) -> AliasResult<CallArg> {
        if let Some(Tok::Ident(label)) = self.peek().cloned() {
            if self.peek_at(1) == Some(&Tok::Assign) {
                let span = self.span();
                self.bump(); // 标签名
                self.bump(); // =
                let value = self.parse_expr()?;
                return Ok(CallArg { label: Some(label), value, span });
            }
        }
        let value = self.parse_expr()?;
        let vspan = value.span();
        Ok(CallArg { label: None, value, span: vspan })
    }

    fn parse_primary(&mut self) -> AliasResult<Expr> {
        let span = self.span();
        match self.peek().cloned() {
            Some(Tok::Int(v)) => {
                self.bump();
                Ok(Expr::Int(v, span))
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
                            let sub_toks = toks
                                .into_iter()
                                .map(|(tok, sp)| Token { tok, span: sp })
                                .collect();
                            let mut sub = Parser { toks: sub_toks, pos: 0 };
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
                    return self.parse_func_lit();
                } else {
                    self.bump(); // (
                    // () 是 unit 空占位
                    if self.peek() == Some(&Tok::RParen) {
                        self.bump();
                        return Ok(Expr::Unit(span));
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
            // self (Phase 2c): 关键字降为普通名字表达式 — 方法体内由 sema
            // 作用域解析 (隐式 val 绑定); 方法体外按未定义绑定拒绝
            Some(Tok::SelfKw) => {
                self.bump();
                Ok(Expr::Ident("self".into(), span))
            }
            // match 表达式 (Phase 2b): match <expr> { ctor(绑定) -> 体, ... }
            Some(Tok::Match) => self.parse_match_expr(),
            // 数组字面量 (Phase 2d): [e1, e2, ...] — 元素逗号分隔,
            // 尾逗号容忍 (M27 先例); 括号内换行由 skip_newlines 吸收
            Some(Tok::LBracket) => self.parse_array_lit(),
            other => Err(self.err_here(format!("无法开始一个表达式: {:?}", other))),
        }
    }

    /// '[' 处进入: [ 元素, 元素, ... ] — 空字面量 [] 合法
    /// (元素类型由声明上下文统一; 裸空字面量推断为 array<未知>)
    fn parse_array_lit(&mut self) -> AliasResult<Expr> {
        let span = self.span();
        self.bump(); // [
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

    /// match 文法 (file_wc.as 34-52 为冻结形状):
    ///   arm := ("ok"|"err") "(" IDENT ")" "->" 体 [","]?   尾逗号容忍
    ///   体  := "{" 块 "}" | "return" 表达式 | 表达式
    /// 臂构造器名只接受 ok/err — 其余在语法层拒绝 (语言无用户自定义枚举)
    fn parse_match_expr(&mut self) -> AliasResult<Expr> {
        let span = self.span();
        self.bump(); // match
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
                    // 臂间逗号可选 ([,]?): 换行与逗号皆可分隔, 尾逗号容忍
                    self.eat(&Tok::Comma);
                }
            }
        }
        self.skip_newlines();
        self.expect(&Tok::RBrace)?;
        Ok(Expr::Match { subject: Box::new(subject), arms, span })
    }

    fn parse_match_arm(&mut self) -> AliasResult<MatchArm> {
        let span = self.span();
        let ctor = match self.peek().cloned() {
            Some(Tok::Ident(n)) if n == "ok" => {
                self.bump();
                CtorKind::Ok
            }
            Some(Tok::Ident(n)) if n == "err" => {
                self.bump();
                CtorKind::Err
            }
            other => {
                return Err(self.err_here(format!(
                    "match 臂构造器必须是 ok 或 err, 实际 {:?}",
                    other
                )));
            }
        };
        self.expect(&Tok::LParen)?;
        let binding = self.expect_ident()?;
        self.expect(&Tok::RParen)?;
        self.expect(&Tok::Arrow)?;
        let body = if self.peek() == Some(&Tok::LBrace) {
            ArmBody::Block(self.parse_block()?)
        } else if self.eat(&Tok::Return) {
            ArmBody::Ret(Box::new(self.parse_expr()?))
        } else {
            ArmBody::Value(Box::new(self.parse_expr()?))
        };
        Ok(MatchArm { ctor, binding, body, span })
    }

    /// 判定 '(' 开头是否为函数字面量:
    /// 从 '(' 向前扫描到配对的 ')', 其后紧跟 '->' 即是。
    /// (i32 x) -> ... / () -> { ... }; 分组表达式内不存在顶层 ') ->' 序列。
    fn looks_like_func_lit(&self) -> bool {
        let mut depth: u32 = 0;
        let mut i = self.pos;
        while let Some(t) = self.toks.get(i) {
            match t.tok {
                Tok::LParen | Tok::LBrace | Tok::LBracket => depth += 1,
                Tok::RParen | Tok::RBrace | Tok::RBracket => {
                    depth -= 1;
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

    /// '(' 处进入, 解析 (类型 名字, ...) -> 体
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
                params.push(Param { ty, name, span: p_span });
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
        Ok(Expr::FuncLit { params, body: Box::new(body), span })
    }

    /// 体: '{' 块 '}' 或 'return' 表达式 (demo 先例均带 return; 宽容接受裸表达式)
    fn parse_func_body(&mut self) -> AliasResult<Body> {
        if self.peek() == Some(&Tok::LBrace) {
            Ok(Body::Block(self.parse_block()?))
        } else {
            self.eat(&Tok::Return);
            let e = self.parse_expr()?;
            Ok(Body::ArrowExpr(Box::new(e)))
        }
    }
}
