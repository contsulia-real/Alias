use super::super::{validate_nesting, Parser};
use crate::ast::{ArmBody, Body, Expr, MatchArm, Param, Pattern, StrPartAst};
use crate::builtins::classify_result_constructor;
use crate::lexer::{StrPart, Tok, Token};
use crate::{AliasError, AliasResult};

impl Parser {
    pub(super) fn parse_primary(&mut self) -> AliasResult<Expr> {
        let span = self.span();
        match self.peek().cloned() {
            Some(Tok::Int(v)) => {
                self.bump()?;
                Ok(Expr::Int(v, span))
            }
            Some(Tok::Float(v)) => {
                self.bump()?;
                Ok(Expr::Float(v, span))
            }
            Some(Tok::Bool(b)) => {
                self.bump()?;
                Ok(Expr::Bool(b, span))
            }
            Some(Tok::Str(parts)) => {
                self.bump()?;
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
                            let mut sub = Parser::new(sub_toks);
                            let e = sub.parse_expr().map_err(|e| AliasError {
                                msg: format!("插值内表达式错误: {}", e.msg),
                                span: e.span,
                            })?;
                            // 插值是一个完整的表达式子流；允许 parse_expr 留下尾 token 会把
                            // `${1, 2}` 一类非法源码静默截断为 `${1}`，破坏 fail-closed 边界。
                            sub.expect_eof().map_err(|e| AliasError {
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
                    self.bump()?;
                    let target = self.parse_type()?;
                    self.expect(&Tok::RParen)?;
                    let expr = self.parse_unary()?;
                    Ok(Expr::Cast {
                        target,
                        expr: Box::new(expr),
                        span,
                    })
                } else {
                    self.bump()?;
                    if self.peek() == Some(&Tok::RParen) {
                        self.bump()?;
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
                self.bump()?;
                Ok(Expr::Ident("self".into(), span))
            }
            Some(Tok::ThisKw) => {
                self.bump()?;
                Ok(Expr::This(span))
            }
            Some(Tok::Match) => self.parse_match_expr(),
            Some(Tok::LBracket) => self.parse_array_lit(),
            Some(Tok::If) => Err(self.err_here("if 只能作为语句使用；条件取值请使用 ?: 或 match")),
            other => Err(self.err_here(format!("无法开始一个表达式: {:?}", other))),
        }
    }

    fn parse_array_lit(&mut self) -> AliasResult<Expr> {
        let span = self.span();
        self.bump()?;
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
        self.bump()?;
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
                if classify_result_constructor(&name).is_some()
                    && self.peek_at(1) == Some(&Tok::LParen) =>
            {
                self.bump()?;
                let ctor = classify_result_constructor(&name).ok_or_else(|| {
                    self.err_here("内部 parser 不变式被破坏: result Pattern 分类漂移")
                })?;
                self.expect(&Tok::LParen)?;
                let binding = match self.peek().cloned() {
                    Some(Tok::Ident(n)) => {
                        self.bump()?;
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
                self.bump()?;
                if name == "_" {
                    Ok(Pattern::Wildcard { span })
                } else {
                    Ok(Pattern::Binding { name, span })
                }
            }
            Some(Tok::Minus) if matches!(self.peek_at(1), Some(Tok::Int(_))) => {
                self.bump()?;
                let Some(Tok::Int(v)) = self.peek().cloned() else {
                    return Err(self.err_here("负整数 Pattern 缺少整数字面量"));
                };
                self.bump()?;
                let value = -(v as i128);
                Ok(Pattern::Int { value, span })
            }
            Some(Tok::Int(value)) => {
                self.bump()?;
                Ok(Pattern::Int {
                    value: value as i128,
                    span,
                })
            }
            Some(Tok::Bool(value)) => {
                self.bump()?;
                Ok(Pattern::Bool { value, span })
            }
            Some(Tok::Str(parts)) => {
                self.bump()?;
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

    fn parse_func_body(&mut self) -> AliasResult<Body> {
        if self.peek() == Some(&Tok::LBrace) {
            Ok(Body::Block(self.parse_block()?))
        } else {
            Ok(Body::Single(Box::new(self.parse_stmt()?)))
        }
    }
}
