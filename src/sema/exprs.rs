//! sema::exprs — 表达式静态语义。

use super::types::{types_match, FloatW, IntW, Ty, UIntW};
use super::{literal_slot_unify, op_mismatch, Checker, Env, Scope, VarInfo};
use crate::ast::{ArmBody, BinOp, CallArg, CtorKind, Expr, MatchArm, StrPartAst, Stmt};
use crate::{AliasError, AliasResult, Span};

impl Checker {
    /// 在已知目标类型语境中检查表达式。声明、赋值、参数、字段、三元与
    /// match 都走这一入口，使窄数值字面量的范围规则完全一致。
    pub(super) fn expr_expected(
        &mut self,
        e: &Expr,
        env: &Env,
        expected: &Ty,
    ) -> AliasResult<Ty> {
        if let Some(r) = literal_slot_unify(expected, e) {
            return r;
        }
        match (e, expected) {
            (
                Expr::Ternary { cond, then_expr, else_expr, span: _ },
                expected,
            ) => {
                self.require_bool(cond, env, "?: 条件")?;
                self.expr_expected(then_expr, env, expected)?;
                self.expr_expected(else_expr, env, expected)?;
                Ok(expected.clone())
            }
            (Expr::Match { subject, arms, span }, expected) => {
                self.match_expr(subject, arms, *span, env, Some(expected))
            }
            (Expr::ArrayLit { elems, .. }, Ty::Array(elem)) => {
                for item in elems {
                    match self.expr_expected(item, env, elem) {
                        Ok(_) => {}
                        Err(e) if e.msg.starts_with("需要 ") => {
                            let got = self.expr(item, env)?;
                            return Err(AliasError {
                                msg: format!("数组元素类型不一致: {} 与 {}", elem.name(), got.name()),
                                span: item.span(),
                            });
                        }
                        Err(e) => return Err(e),
                    }
                }
                Ok(expected.clone())
            }
            (Expr::Call { callee, args, span }, Ty::Result(ok, err)) => {
                if let Expr::Ident(name, _) = callee.as_ref() {
                    if Scope::get(env, name).is_none() && (name == "ok" || name == "err") {
                        let [arg] = args.as_slice() else {
                            return Err(AliasError {
                                msg: format!("{name} 构造恰好接受 1 个参数"),
                                span: *span,
                            });
                        };
                        if arg.label.is_some() {
                            return Err(AliasError {
                                msg: "result 构造不接受命名实参".into(),
                                span: arg.span,
                            });
                        }
                        let payload = if name == "ok" { ok.as_ref() } else { err.as_ref() };
                        self.expr_expected(&arg.value, env, payload)?;
                        return Ok(expected.clone());
                    }
                }
                self.check_inferred_expected(e, env, expected)
            }
            _ => self.check_inferred_expected(e, env, expected),
        }
    }

    fn check_inferred_expected(&mut self, e: &Expr, env: &Env, expected: &Ty) -> AliasResult<Ty> {
        let got = self.expr(e, env)?;
        if types_match(expected, &got) {
            Ok(expected.clone())
        } else {
            Err(AliasError {
                msg: format!("需要 {}, 实际 {}", expected.name(), got.name()),
                span: e.span(),
            })
        }
    }

    pub(super) fn expr(&mut self, e: &Expr, env: &Env) -> AliasResult<Ty> {
        match e {
            Expr::Int(..) => Ok(Ty::Int(IntW::W32)),
            Expr::Float(..) => Ok(Ty::Float(FloatW::F64)),
            Expr::Bool(..) => Ok(Ty::Bool),
            Expr::Unit(_) => Ok(Ty::Unit),
            Expr::Str(parts, _) => {
                for p in parts {
                    if let StrPartAst::Hole(h) = p {
                        self.expr(h, env)?;
                    }
                }
                Ok(Ty::Str)
            }
            Expr::Ident(name, span) => Scope::get(env, name)
                .map(|info| info.ty)
                .ok_or_else(|| AliasError {
                    msg: format!("未定义的绑定 '{name}'"),
                    span: *span,
                }),
            Expr::Neg { expr, .. } => {
                let t = self.expr(expr, env)?;
                if t.is_unknown() {
                    return Ok(Ty::Unknown);
                }
                match t {
                    Ty::Int(w) => Ok(Ty::Int(w)),
                    Ty::Float(w) => Ok(Ty::Float(w)),
                    other => Err(AliasError {
                        msg: format!("取负需要有符号整数或浮点, 实际 {}", other.name()),
                        span: expr.span(),
                    }),
                }
            }
            Expr::Not { expr, .. } => {
                self.require_bool(expr, env, "! 操作数")?;
                Ok(Ty::Bool)
            }
            Expr::Binary { op, lhs, rhs, span } => {
                let l = self.expr(lhs, env)?;
                // && / || 在运行时短路，但静态期仍检查右侧类型。
                let r = self.expr(rhs, env)?;
                self.binary(*op, l, r, *span)
            }
            Expr::Ternary { cond, then_expr, else_expr, span } => {
                self.require_bool(cond, env, "?: 条件")?;
                let a = self.expr(then_expr, env)?;
                let b = self.expr(else_expr, env)?;
                if a.is_unknown() {
                    Ok(b)
                } else if b.is_unknown() || types_match(&a, &b) {
                    Ok(a)
                } else {
                    Err(AliasError {
                        msg: format!("?: 两个值分支类型不一致: {} 与 {}", a.name(), b.name()),
                        span: *span,
                    })
                }
            }
            Expr::Call { callee, args, span } => self.call(callee, args, *span, env),
            Expr::MethodCall { recv, name, args, span } => {
                self.method_call(recv, name, args, *span, env)
            }
            Expr::Index { recv, idx, .. } => {
                let rt = self.expr(recv, env)?;
                if rt.is_unknown() {
                    return Ok(Ty::Unknown);
                }
                let Ty::Array(elem) = rt else {
                    return Err(AliasError {
                        msg: format!("下标访问需要 array 类型, 实际 {}", rt.name()),
                        span: recv.span(),
                    });
                };
                let it = self.expr(idx, env)?;
                if !it.is_unknown() && it != Ty::Int(IntW::W32) {
                    return Err(AliasError {
                        msg: format!("下标需要 i32, 实际 {}", it.name()),
                        span: idx.span(),
                    });
                }
                Ok(*elem)
            }
            Expr::ArrayLit { elems, .. } => {
                let mut elem_ty: Option<Ty> = None;
                for item in elems {
                    let t = self.expr(item, env)?;
                    match &elem_ty {
                        None => elem_ty = Some(t),
                        Some(first) if !types_match(first, &t) => {
                            return Err(AliasError {
                                msg: format!("数组元素类型不一致: {} 与 {}", first.name(), t.name()),
                                span: item.span(),
                            });
                        }
                        _ => {}
                    }
                }
                Ok(Ty::Array(Box::new(elem_ty.unwrap_or(Ty::Unknown))))
            }
            Expr::Field { recv, name, span } => {
                let rt = self.expr(recv, env)?;
                if rt.is_unknown() {
                    return Ok(Ty::Unknown);
                }
                match rt {
                    Ty::Struct(s) => {
                        let info = &self.structs[&s];
                        info.fields
                            .iter()
                            .find(|f| f.name == *name)
                            .map(|f| f.ty.clone())
                            .ok_or_else(|| AliasError {
                                msg: format!("结构体 {s} 没有字段 '{name}'"),
                                span: *span,
                            })
                    }
                    other => Err(AliasError {
                        msg: format!("{} 没有字段 '{}'", other.name(), name),
                        span: *span,
                    }),
                }
            }
            Expr::FuncLit { params, body, span } => self.funclit(params, body, env, None, *span),
            Expr::Match { subject, arms, span } => {
                self.match_expr(subject, arms, *span, env, None)
            }
            Expr::Propagate { expr, span } => self.propagate(expr, *span, env),
        }
    }

    fn require_bool(&mut self, e: &Expr, env: &Env, what: &str) -> AliasResult<()> {
        let t = self.expr(e, env)?;
        if t.is_unknown() || t == Ty::Bool {
            Ok(())
        } else {
            Err(AliasError {
                msg: format!("{what}需要 bool, 实际 {}", t.name()),
                span: e.span(),
            })
        }
    }

    fn match_expr(
        &mut self,
        subject: &Expr,
        arms: &[MatchArm],
        span: Span,
        env: &Env,
        expected: Option<&Ty>,
    ) -> AliasResult<Ty> {
        let st = self.expr(subject, env)?;
        if st.is_unknown() {
            return Ok(Ty::Unknown);
        }
        let Ty::Result(t_ty, e_ty) = st else {
            return Err(AliasError {
                msg: format!("match 主语需要 result 类型, 实际 {}", st.name()),
                span: subject.span(),
            });
        };
        let mut ok_arm = None;
        let mut err_arm = None;
        for arm in arms {
            match arm.ctor {
                CtorKind::Ok if ok_arm.is_none() => ok_arm = Some(arm),
                CtorKind::Err if err_arm.is_none() => err_arm = Some(arm),
                CtorKind::Ok => {
                    return Err(AliasError { msg: "match 重复覆盖 ok 臂".into(), span: arm.span })
                }
                CtorKind::Err => {
                    return Err(AliasError { msg: "match 重复覆盖 err 臂".into(), span: arm.span })
                }
            }
        }
        let (Some(ok_arm), Some(err_arm)) = (ok_arm, err_arm) else {
            return Err(AliasError { msg: "match 必须同时覆盖 ok 与 err".into(), span });
        };
        let ok_t = self.match_arm(ok_arm, &t_ty, env, expected)?;
        let err_t = self.match_arm(err_arm, &e_ty, env, expected)?;
        if let Some(want) = expected {
            return Ok(want.clone());
        }
        match (ok_t, err_t) {
            (None, None) => Ok(Ty::Unknown),
            (Some(t), None) | (None, Some(t)) => Ok(t),
            (Some(a), Some(b)) => {
                if a.is_unknown() {
                    Ok(b)
                } else if b.is_unknown() || types_match(&a, &b) {
                    Ok(a)
                } else {
                    Err(AliasError {
                        msg: format!("match 各臂类型不一致: {} 与 {}", a.name(), b.name()),
                        span: err_arm.span,
                    })
                }
            }
        }
    }

    fn match_arm(
        &mut self,
        arm: &MatchArm,
        bind_ty: &Ty,
        env: &Env,
        expected: Option<&Ty>,
    ) -> AliasResult<Option<Ty>> {
        let local = Scope::child(env);
        Scope::insert(&local, arm.binding.clone(), VarInfo { ty: bind_ty.clone(), mutable: false });
        match &arm.body {
            ArmBody::Value(e) => Ok(Some(match expected {
                Some(w) => self.expr_expected(e, &local, w)?,
                None => self.expr(e, &local)?,
            })),
            ArmBody::Ret(e) => {
                let Some(ret) = self.fn_ret.last().cloned() else {
                    return Err(AliasError { msg: "顶层不允许 return".into(), span: e.span() });
                };
                self.expr_expected(e, &local, &ret)?;
                Ok(None)
            }
            ArmBody::Block(stmts) => {
                for s in stmts {
                    self.stmt(s, &local)?;
                }
                if crate::sema::stmts::block_terminates_with_return(stmts) {
                    return Ok(None);
                }
                match stmts.last() {
                    Some(Stmt::ExprStmt { expr, .. }) => Ok(Some(match expected {
                        Some(w) => self.expr_expected(expr, &local, w)?,
                        None => self.expr(expr, &local)?,
                    })),
                    _ => Ok(Some(Ty::Unit)),
                }
            }
        }
    }

    fn propagate(&mut self, expr: &Expr, span: Span, env: &Env) -> AliasResult<Ty> {
        let t = self.expr(expr, env)?;
        if t.is_unknown() {
            return Ok(Ty::Unknown);
        }
        let Ty::Result(v_ty, e_ty) = t else {
            return Err(AliasError {
                msg: format!("? 只能作用于 result 值, 实际 {}", t.name()),
                span,
            });
        };
        let Some(ret) = self.fn_ret.last().cloned() else {
            return Err(AliasError { msg: "? 需要所在函数返回 result 类型".into(), span });
        };
        let Ty::Result(_, fn_e) = &ret else {
            return Err(AliasError {
                msg: format!("? 需要所在函数返回 result 类型, 实际 {}", ret.name()),
                span,
            });
        };
        if !types_match(fn_e, &e_ty) {
            return Err(AliasError {
                msg: format!(
                    "? 错误类型不匹配: 表达式错误为 {}, 所在函数错误为 {}",
                    e_ty.name(),
                    fn_e.name()
                ),
                span,
            });
        }
        Ok(*v_ty)
    }

    fn binary(&mut self, op: BinOp, l: Ty, r: Ty, span: Span) -> AliasResult<Ty> {
        use BinOp::*;
        if l.is_unknown() || r.is_unknown() {
            return Ok(Ty::Unknown);
        }
        if matches!(op, And | Or) {
            return if l == Ty::Bool && r == Ty::Bool {
                Ok(Ty::Bool)
            } else {
                Err(op_mismatch(op, &l, &r, span))
            };
        }
        let mixed = |span| {
            if l.is_numeric() && r.is_numeric() {
                AliasError {
                    msg: format!("{} 与 {} 禁止隐式混算", l.name(), r.name()),
                    span,
                }
            } else {
                op_mismatch(op, &l, &r, span)
            }
        };
        match op {
            Add | Sub | Mul | Div => match (&l, &r) {
                (Ty::Int(a), Ty::Int(b)) if a == b => Ok(Ty::Int(*a)),
                (Ty::UInt(a), Ty::UInt(b)) if a == b => Ok(Ty::UInt(*a)),
                (Ty::Float(a), Ty::Float(b)) if a == b => Ok(Ty::Float(*a)),
                _ => Err(mixed(span)),
            },
            Lt | Le | Gt | Ge | EqEq | NotEq => match (&l, &r) {
                (Ty::Int(a), Ty::Int(b)) if a == b => Ok(Ty::Bool),
                (Ty::UInt(a), Ty::UInt(b)) if a == b => Ok(Ty::Bool),
                (Ty::Float(a), Ty::Float(b)) if a == b => Ok(Ty::Bool),
                (Ty::Str, Ty::Str) => Ok(Ty::Bool),
                (Ty::Bool, Ty::Bool) if matches!(op, EqEq | NotEq) => Ok(Ty::Bool),
                _ => Err(mixed(span)),
            },
            And | Or => unreachable!(),
        }
    }

    fn call(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        if let Expr::Ident(name, _) = callee {
            if Scope::get(env, name).is_none() && self.structs.contains_key(name) {
                return self.construct(name, args, span, env);
            }
            if Scope::get(env, name).is_none() && (name == "ok" || name == "err") {
                return self.result_ctor(name, args, span, env);
            }
        }
        for a in args {
            if let Some(lbl) = &a.label {
                return Err(AliasError {
                    msg: format!("函数调用不接受命名实参 '{lbl}'"),
                    span: a.span,
                });
            }
        }
        if let Expr::Ident(name, _) = callee {
            if name == "increase" || name == "decrease" {
                return self.incdec(name, args, span, env);
            }
            if name == "println" || name == "print" {
                let [arg] = args else {
                    return Err(AliasError { msg: format!("{name} 恰好接受 1 个参数"), span });
                };
                self.expr(&arg.value, env)?;
                return Ok(Ty::Unit);
            }
            if let Some(target) = conv_builtin_ty(name) {
                let [arg] = args else {
                    return Err(AliasError { msg: format!("{name} 恰好接受 1 个参数"), span });
                };
                let t = self.expr(&arg.value, env)?;
                if !t.is_unknown() && !t.is_numeric() {
                    return Err(AliasError {
                        msg: format!("{name} 需要数值类型, 实际 {}", t.name()),
                        span: arg.value.span(),
                    });
                }
                return Ok(target);
            }
            if name == "typeof" {
                let [arg] = args else {
                    return Err(AliasError { msg: "typeof 恰好接受 1 个参数".into(), span });
                };
                let t = self.expr(&arg.value, env)?;
                if t.is_unknown() {
                    return Err(AliasError { msg: "typeof 无法确定实参的静态类型".into(), span: arg.value.span() });
                }
                return Ok(Ty::Str);
            }
        }
        let ft = self.expr(callee, env)?;
        match ft {
            Ty::Func { params, ret } => {
                if args.len() != params.len() {
                    return Err(AliasError {
                        msg: format!("期望 {} 个参数, 实际 {} 个", params.len(), args.len()),
                        span,
                    });
                }
                for (i, (a, pt)) in args.iter().zip(&params).enumerate() {
                    match self.expr_expected(&a.value, env, pt) {
                        Ok(_) => {}
                        Err(e) if e.msg.starts_with("需要 ") => {
                            let got = self.expr(&a.value, env)?;
                            return Err(AliasError {
                                msg: format!("第 {} 个实参需要 {}, 实际 {}", i + 1, pt.name(), got.name()),
                                span: a.value.span(),
                            });
                        }
                        Err(e) => return Err(e),
                    }
                }
                Ok(*ret)
            }
            Ty::FuncPoly | Ty::Unknown => {
                for a in args {
                    self.expr(&a.value, env)?;
                }
                Ok(Ty::Unknown)
            }
            other => Err(AliasError { msg: format!("{} 不是可调用值", other.name()), span }),
        }
    }

    fn construct(&mut self, name: &str, args: &[CallArg], span: Span, env: &Env) -> AliasResult<Ty> {
        let info = self.structs[name].clone();
        let mut covered = vec![false; info.fields.len()];
        for a in args {
            let Some(lbl) = &a.label else {
                return Err(AliasError { msg: format!("结构体 {name} 构造必须使用命名实参"), span: a.span });
            };
            let Some(idx) = info.fields.iter().position(|f| &f.name == lbl) else {
                return Err(AliasError { msg: format!("结构体 {name} 没有字段 '{lbl}'"), span: a.span });
            };
            if covered[idx] {
                return Err(AliasError { msg: format!("结构体 {name} 构造重复指定字段 '{lbl}'"), span: a.span });
            }
            covered[idx] = true;
            let want = &info.fields[idx].ty;
            match self.expr_expected(&a.value, env, want) {
                Ok(_) => {}
                Err(e) if e.msg.starts_with("需要 ") => {
                    let got = self.expr(&a.value, env)?;
                    return Err(AliasError {
                        msg: format!("字段 '{}' 需要 {}, 实际 {}", lbl, want.name(), got.name()),
                        span: a.value.span(),
                    });
                }
                Err(e) => return Err(e),
            }
        }
        for (f, done) in info.fields.iter().zip(&covered) {
            if !done && !f.has_default {
                return Err(AliasError { msg: format!("结构体 {name} 构造缺少字段 '{}'", f.name), span });
            }
        }
        Ok(Ty::Struct(name.to_string()))
    }

    fn result_ctor(&mut self, name: &str, args: &[CallArg], span: Span, env: &Env) -> AliasResult<Ty> {
        let [arg] = args else {
            return Err(AliasError { msg: format!("{name} 构造恰好接受 1 个参数"), span });
        };
        if arg.label.is_some() {
            return Err(AliasError { msg: "result 构造不接受命名实参".into(), span: arg.span });
        }
        let t = self.expr(&arg.value, env)?;
        Ok(if name == "ok" {
            Ty::Result(Box::new(t), Box::new(Ty::Unknown))
        } else {
            Ty::Result(Box::new(Ty::Unknown), Box::new(t))
        })
    }

    fn incdec(&mut self, name: &str, args: &[CallArg], span: Span, env: &Env) -> AliasResult<Ty> {
        let [arg] = args else {
            return Err(AliasError { msg: format!("{name} 恰好接受 1 个参数"), span });
        };
        let Expr::Ident(target, tspan) = &arg.value else {
            return Err(AliasError { msg: format!("{name} 的参数必须是可变绑定名"), span });
        };
        let Some(info) = Scope::get(env, target) else {
            return Err(AliasError { msg: format!("'{target}' 未定义"), span: *tspan });
        };
        if !info.mutable {
            return Err(AliasError { msg: format!("'{target}' 是 val 绑定, 不能 {name}"), span: *tspan });
        }
        if info.ty.is_unknown() || info.ty == Ty::Int(IntW::W32) {
            Ok(Ty::Unit)
        } else {
            Err(AliasError {
                msg: format!("{name} 需要 i32, 实际 {}", info.ty.name()),
                span: *tspan,
            })
        }
    }

    fn method_call(
        &mut self,
        recv: &Expr,
        name: &str,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let rt = self.expr(recv, env)?;
        if rt.is_unknown() {
            return Ok(Ty::Unknown);
        }
        for a in args {
            if let Some(lbl) = &a.label {
                return Err(AliasError {
                    msg: format!("方法调用不接受命名实参 '{lbl}'"),
                    span: a.span,
                });
            }
        }

        // array 的固有方法优先；其他名字继续进入完整类型扩展方法表。
        if let Ty::Array(elem) = &rt {
            match name {
                "len" => {
                    if !args.is_empty() {
                        return Err(AliasError { msg: format!("期望 0 个参数, 实际 {} 个", args.len()), span });
                    }
                    return Ok(Ty::Int(IntW::W32));
                }
                "push" => {
                    if args.len() != 1 {
                        return Err(AliasError { msg: format!("期望 1 个参数, 实际 {} 个", args.len()), span });
                    }
                    match self.expr_expected(&args[0].value, env, elem) {
                        Ok(_) => {}
                        Err(e) if e.msg.starts_with("需要 ") => {
                            let got = self.expr(&args[0].value, env)?;
                            return Err(AliasError {
                                msg: format!("第 1 个实参需要 {}, 实际 {}", elem.name(), got.name()),
                                span: args[0].value.span(),
                            });
                        }
                        Err(e) => return Err(e),
                    }
                    return Ok(Ty::Unit);
                }
                "pop" => {
                    if !args.is_empty() {
                        return Err(AliasError { msg: format!("期望 0 个参数, 实际 {} 个", args.len()), span });
                    }
                    return Ok((**elem).clone());
                }
                "iterator" => {
                    if !args.is_empty() {
                        return Err(AliasError { msg: format!("期望 0 个参数, 实际 {} 个", args.len()), span });
                    }
                    return Ok(Ty::Iterator(elem.clone()));
                }
                _ => {}
            }
        }

        let rname = rt.name();
        let Some(sig) = self.methods.get(&rname).and_then(|m| m.get(name)).cloned() else {
            return Err(AliasError { msg: format!("类型 {rname} 上没有方法 '{name}'"), span });
        };
        if args.len() != sig.params.len() {
            return Err(AliasError {
                msg: format!("期望 {} 个参数, 实际 {} 个", sig.params.len(), args.len()),
                span,
            });
        }
        for (i, (a, want)) in args.iter().zip(&sig.params).enumerate() {
            match self.expr_expected(&a.value, env, want) {
                Ok(_) => {}
                Err(e) if e.msg.starts_with("需要 ") => {
                    let got = self.expr(&a.value, env)?;
                    return Err(AliasError {
                        msg: format!("第 {} 个实参需要 {}, 实际 {}", i + 1, want.name(), got.name()),
                        span: a.value.span(),
                    });
                }
                Err(e) => return Err(e),
            }
        }
        Ok(sig.ret)
    }
}

fn conv_builtin_ty(name: &str) -> Option<Ty> {
    Some(match name {
        "to_i8" => Ty::Int(IntW::W8),
        "to_i16" => Ty::Int(IntW::W16),
        "to_i32" => Ty::Int(IntW::W32),
        "to_i64" => Ty::Int(IntW::W64),
        "to_u8" => Ty::UInt(UIntW::U8),
        "to_u16" => Ty::UInt(UIntW::U16),
        "to_u32" => Ty::UInt(UIntW::U32),
        "to_u64" => Ty::UInt(UIntW::U64),
        "to_f32" => Ty::Float(FloatW::F32),
        "to_f64" => Ty::Float(FloatW::F64),
        _ => return None,
    })
}
