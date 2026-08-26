//! sema::exprs — 表达式检查与类型推断。
//!
//! 拥有: 表达式分派 ([`Checker::expr`])、结构体构造 / ok-err 构造器 /
//! match 穷尽性 / ? 传播合法性 / 方法调用解析 / 字段访问 / 二元运算
//! 类型表 / 调用元数与实参一致性 / increase-decrease 内建。
//! 求值顺序 (lhs-before-rhs、先值后目标) 为黄金记录冻结, 逐行保留。

use super::types::{types_match, FloatW, IntW, Ty, UIntW};
use super::{op_mismatch, Checker, Env, Scope, VarInfo};
use crate::ast::{ArmBody, BinOp, CallArg, CtorKind, Expr, MatchArm, StrPartAst, Stmt};
use crate::{AliasError, AliasResult, Span};

impl Checker {
    // ---------- 表达式: 推断 + 检查 ----------

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
            Expr::Ident(name, span) => match Scope::get(env, name) {
                Some(info) => Ok(info.ty),
                None => Err(AliasError {
                    msg: format!("未定义的绑定 '{name}'"),
                    span: *span,
                }),
            },
            Expr::Neg { expr, .. } => {
                // 操作数先于错误 — 黄金记录冻结的求值顺序
                let t = self.expr(expr, env)?;
                if t.is_unknown() {
                    return Ok(Ty::Unknown);
                }
                match t {
                    // 取负按声明宽度 wrapping (Phase 3a); 无符号取负无定义
                    Ty::Int(w) => Ok(Ty::Int(w)),
                    Ty::Float(w) => Ok(Ty::Float(w)),
                    other => Err(AliasError {
                        msg: format!("取负需要有符号整数或浮点, 实际 {}", other.name()),
                        span: expr.span(),
                    }),
                }
            }
            Expr::Binary { op, lhs, rhs, span } => {
                // lhs-before-rhs 顺序保持 (黄金记录冻结的求值序)
                let l = self.expr(lhs, env)?;
                let r = self.expr(rhs, env)?;
                self.binary(*op, l, r, *span)
            }
            Expr::Call { callee, args, span } => self.call(callee, args, *span, env),
            // Phase 2c: 方法调用为真语义 — 静态分派 (接收者推断类型 → 方法表)
            Expr::MethodCall { recv, name, args, span } => {
                self.method_call(recv, name, args, *span, env)
            }
            // Phase 2d: 下标读真语义 — 主语须为 array<T>, 下标须 i32;
            // 主语类型不可知时级联抑制 (先例: match 主语)
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
            // Phase 2d: 数组字面量 — 元素按书写序检查, 首元素定候选类型,
            // 其余逐个统一; 不一致即编译错误 (span 落在违规元素上)
            Expr::ArrayLit { elems, .. } => {
                let mut elem_ty: Option<Ty> = None;
                for e in elems {
                    let t = self.expr(e, env)?;
                    match &elem_ty {
                        None => elem_ty = Some(t),
                        Some(first) if !types_match(first, &t) => {
                            return Err(AliasError {
                                msg: format!(
                                    "数组元素类型不一致: {} 与 {}",
                                    first.name(),
                                    t.name()
                                ),
                                span: e.span(),
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
                        match info.fields.iter().find(|f| f.name == *name) {
                            Some(f) => Ok(f.ty.clone()),
                            None => Err(AliasError {
                                msg: format!("结构体 {s} 没有字段 '{name}'"),
                                span: *span,
                            }),
                        }
                    }
                    other => Err(AliasError {
                        msg: format!("{} 没有字段 '{}'", other.name(), name),
                        span: *span,
                    }),
                }
            }
            Expr::FuncLit { params, body, span } => {
                self.funclit(params, body, env, None, *span)
            }
            Expr::Match { subject, arms, span } => {
                self.match_expr(subject, arms, *span, env)
            }
            Expr::Propagate { expr, span } => self.propagate(expr, *span, env),
        }
    }

    /// match 表达式检查 (Phase 2b): 主语须为 result<T,E> →
    /// ok 臂绑定 : T / err 臂绑定 : E; 穷尽性 = 恰一 ok + 恰一 err;
    /// 值 = 非 never 臂的公共类型。
    fn match_expr(
        &mut self,
        subject: &Expr,
        arms: &[MatchArm],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let st = self.expr(subject, env)?;
        if st.is_unknown() {
            // 级联抑制: 主语类型不可知时不下钻臂体 (先例: MethodCall 不下钻)
            return Ok(Ty::Unknown);
        }
        let Ty::Result(t_ty, e_ty) = st else {
            return Err(AliasError {
                msg: format!("match 主语需要 result 类型, 实际 {}", st.name()),
                span: subject.span(),
            });
        };
        let mut ok_arm: Option<&MatchArm> = None;
        let mut err_arm: Option<&MatchArm> = None;
        for arm in arms {
            match arm.ctor {
                CtorKind::Ok => {
                    if ok_arm.is_some() {
                        return Err(AliasError {
                            msg: "match 重复覆盖 ok 臂".into(),
                            span: arm.span,
                        });
                    }
                    ok_arm = Some(arm);
                }
                CtorKind::Err => {
                    if err_arm.is_some() {
                        return Err(AliasError {
                            msg: "match 重复覆盖 err 臂".into(),
                            span: arm.span,
                        });
                    }
                    err_arm = Some(arm);
                }
            }
        }
        let Some(ok_arm) = ok_arm else {
            return Err(AliasError {
                msg: "match 必须同时覆盖 ok 与 err".into(),
                span,
            });
        };
        let Some(err_arm) = err_arm else {
            return Err(AliasError {
                msg: "match 必须同时覆盖 ok 与 err".into(),
                span,
            });
        };
        let ok_t = self.match_arm(ok_arm, &t_ty, env)?;
        let err_t = self.match_arm(err_arm, &e_ty, env)?;
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

    /// 单臂检查: 绑定以 val 语义进入臂作用域; 返回 Some(臂值类型) 或
    /// None (never 流 — 臂以 return 收尾)。
    fn match_arm(
        &mut self,
        arm: &MatchArm,
        bind_ty: &Ty,
        env: &Env,
    ) -> AliasResult<Option<Ty>> {
        let local = Scope::child(env);
        Scope::insert(&local, arm.binding.clone(), VarInfo { ty: bind_ty.clone(), mutable: false });
        match &arm.body {
            ArmBody::Value(e) => Ok(Some(self.expr(e, &local)?)),
            ArmBody::Ret(e) => {
                // 镜像 Stmt::Return 的检查与求值顺序
                let t = self.expr(e, &local)?;
                match self.fn_ret.last() {
                    None => Err(AliasError {
                        msg: "顶层不允许 return".into(),
                        span: e.span(),
                    }),
                    Some(d) if !types_match(d, &t) => Err(AliasError {
                        msg: format!("return 需要 {}, 实际 {}", d.name(), t.name()),
                        span: e.span(),
                    }),
                    _ => Ok(None),
                }
            }
            ArmBody::Block(stmts) => {
                for s in stmts {
                    self.stmt(s, &local)?;
                }
                if matches!(stmts.last(), Some(Stmt::Return { .. })) {
                    return Ok(None);
                }
                // 尾表达式语句 = 臂值; 其余收尾 (绑定/循环等) 臂值 = unit
                match stmts.last() {
                    Some(Stmt::ExprStmt { expr, .. }) => {
                        Ok(Some(self.expr(expr, &local)?))
                    }
                    _ => Ok(Some(Ty::Unit)),
                }
            }
        }
    }

    /// expr? 传播糖检查 (P6): 仅当所在函数声明返回 result<_, E'> 且
    /// 主语错误类型 E == E' 时合法; 值类型 = 主语的 T。
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
            return Err(AliasError {
                msg: "? 需要所在函数返回 result 类型".into(),
                span,
            });
        };
        let Ty::Result(_, fn_e) = &ret else {
            return Err(AliasError {
                msg: format!(
                    "? 需要所在函数返回 result 类型, 实际 {}",
                    ret.name()
                ),
                span,
            });
        };
        if !e_ty.is_unknown() && !fn_e.is_unknown() && e_ty != *fn_e {
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
        // 数值混算拦截 (Phase 3a 裁决③): 同族同宽才合法 —
        // 数值×数值异型报「禁止隐式混算」, 数值×非数值沿用运算符不适用
        let mixed = |span: Span| {
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
                (Ty::Bool, Ty::Bool) => match op {
                    EqEq | NotEq => Ok(Ty::Bool),
                    // Q① 裁决: bool 有序比较由静默 false 收紧为编译错误
                    _ => Err(AliasError {
                        msg: format!(
                            "运算符 {} 不适用于 bool 与 bool — 有序比较仅限 i32 与 string",
                            op.display()
                        ),
                        span,
                    }),
                },
                _ => Err(mixed(span)),
            },
        }
    }

    fn call(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        // 结构体构造分派 (Phase 2a): 名字未绑定为变量且已登记为结构体。
        // 单一命名空间的镜像规则 — 名字被绑定遮蔽时按普通调用处理,
        // 由下方「不是可调用值」等既有诊断接管。
        if let Expr::Ident(name, _) = callee {
            if Scope::get(env, name).is_none() && self.structs.contains_key(name) {
                return self.construct(name, args, span, env);
            }
            // result 构造器分派 (Phase 2b): ok/err 为类型构造器而非名字
            // 分派的函数 — 遮蔽规则与结构体同 (被绑定遮蔽即普通调用)
            if (name == "ok" || name == "err") && Scope::get(env, name).is_none() {
                return self.result_ctor(name, args, span, env);
            }
        }
        // 命名实参仅结构体构造合法 — 函数/内建调用一律拒绝
        for a in args {
            if let Some(lbl) = &a.label {
                return Err(AliasError {
                    msg: format!("函数调用不接受命名实参 '{lbl}'"),
                    span: a.span,
                });
            }
        }
        // 内建特判仅限裸 Ident 被调方 — 黄金记录冻结的分派规则
        if let Expr::Ident(name, _) = callee {
            if name == "increase" || name == "decrease" {
                return self.incdec(name, args, span, env);
            }
            if name == "println" || name == "print" {
                let [arg] = args else {
                    return Err(AliasError {
                        msg: format!("{name} 恰好接受 1 个参数"),
                        span,
                    });
                };
                self.expr(&arg.value, env)?;
                return Ok(Ty::Unit);
            }
            // 转换内建 (Phase 3a): to_i8..to_f64 — 实参须数值族,
            // 结果 = 目标类型; 跨族/跨宽一律显式转换 (无隐式混算)
            if let Some(target) = conv_builtin_ty(name) {
                let [arg] = args else {
                    return Err(AliasError {
                        msg: format!("{name} 恰好接受 1 个参数"),
                        span,
                    });
                };
                if !arg.label.is_none() {
                    return Err(AliasError {
                        msg: format!("函数调用不接受命名实参 '{}'", arg.label.as_ref().unwrap()),
                        span: arg.span,
                    });
                }
                let t = self.expr(&arg.value, env)?;
                if !t.is_unknown() && !t.is_numeric() {
                    return Err(AliasError {
                        msg: format!("{name} 需要数值类型, 实际 {}", t.name()),
                        span: arg.value.span(),
                    });
                }
                return Ok(target);
            }
            // typeof 内建 (Phase 3a): 求值实参取副作用, 返回静态类型名字符串
            if name == "typeof" {
                let [arg] = args else {
                    return Err(AliasError {
                        msg: "typeof 恰好接受 1 个参数".into(),
                        span,
                    });
                };
                if !arg.label.is_none() {
                    return Err(AliasError {
                        msg: format!("函数调用不接受命名实参 '{}'", arg.label.as_ref().unwrap()),
                        span: arg.span,
                    });
                }
                let t = self.expr(&arg.value, env)?;
                if t.is_unknown() {
                    return Err(AliasError {
                        msg: "typeof 无法确定实参的静态类型".into(),
                        span: arg.value.span(),
                    });
                }
                return Ok(Ty::Str);
            }
        }
        let ft = self.expr(callee, env)?;
        let mut ats = Vec::with_capacity(args.len());
        for a in args {
            ats.push(self.expr(&a.value, env)?);
        }
        match ft {
            Ty::Func { params, ret } => {
                if args.len() != params.len() {
                    return Err(AliasError {
                        msg: format!("期望 {} 个参数, 实际 {} 个", params.len(), args.len()),
                        span,
                    });
                }
                // D3 新发明: 实参类型 ↔ 参数声明一致性
                for (i, (a, pt)) in args.iter().zip(&params).enumerate() {
                    let at = &ats[i];
                    if !types_match(pt, at) {
                        return Err(AliasError {
                            msg: format!(
                                "第 {} 个实参需要 {}, 实际 {}",
                                i + 1,
                                pt.name(),
                                at.name()
                            ),
                            span: a.value.span(),
                        });
                    }
                }
                Ok((*ret).clone())
            }
            // 多态函数值: 签名未知, 放行调用, 结果视为不可知
            Ty::FuncPoly | Ty::Unknown => Ok(Ty::Unknown),
            other => Err(AliasError {
                msg: format!("{} 不是可调用值", other.name()),
                span,
            }),
        }
    }

    /// 结构体构造检查: 全命名 / 无重复 / 无未知字段 / 全覆盖
    /// (显式传入或声明默认) / 字段类型一致。求值顺序 = 实参书写序。
    fn construct(
        &mut self,
        name: &str,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let info = self.structs[name].clone();
        let mut covered = vec![false; info.fields.len()];
        for a in args {
            let Some(lbl) = &a.label else {
                return Err(AliasError {
                    msg: format!("结构体 {name} 构造必须使用命名实参"),
                    span: a.span,
                });
            };
            let Some(idx) = info.fields.iter().position(|f| &f.name == lbl) else {
                return Err(AliasError {
                    msg: format!("结构体 {name} 没有字段 '{lbl}'"),
                    span: a.span,
                });
            };
            if covered[idx] {
                return Err(AliasError {
                    msg: format!("结构体 {name} 构造重复指定字段 '{lbl}'"),
                    span: a.span,
                });
            }
            covered[idx] = true;
            let vt = self.expr(&a.value, env)?;
            let want = &info.fields[idx].ty;
            if !types_match(want, &vt) {
                return Err(AliasError {
                    msg: format!(
                        "字段 '{}' 需要 {}, 实际 {}",
                        lbl,
                        want.name(),
                        vt.name()
                    ),
                    span: a.value.span(),
                });
            }
        }
        for (f, done) in info.fields.iter().zip(&covered) {
            if !done && !f.has_default {
                return Err(AliasError {
                    msg: format!("结构体 {name} 构造缺少字段 '{}'", f.name),
                    span,
                });
            }
        }
        Ok(Ty::Struct(name.to_string()))
    }

    /// ok/err 构造器检查: 恰一位置实参; 结果单侧推断 —
    /// ok(e) : result<typeof e, Unknown> / err(e) : result<Unknown, typeof e>,
    /// 另一侧由声明上下文经 types_match 统一 (语言无推断, 声明侧恒全知)。
    fn result_ctor(
        &mut self,
        name: &str,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        for a in args {
            if let Some(lbl) = &a.label {
                return Err(AliasError {
                    msg: format!("函数调用不接受命名实参 '{lbl}'"),
                    span: a.span,
                });
            }
        }
        let [arg] = args else {
            return Err(AliasError {
                msg: format!("{name} 构造恰好接受 1 个参数"),
                span,
            });
        };
        let t = self.expr(&arg.value, env)?;
        let payload = Box::new(t);
        Ok(if name == "ok" {
            Ty::Result(payload, Box::new(Ty::Unknown))
        } else {
            Ty::Result(Box::new(Ty::Unknown), payload)
        })
    }

    /// increase/decrease 四错 + 元数错 — 消息与 span 逐字节对齐迁移前报错。
    fn incdec(
        &mut self,
        name: &str,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let [arg] = args else {
            return Err(AliasError {
                msg: format!("{name} 恰好接受 1 个参数"),
                span,
            });
        };
        // 非标识符实参不求值 — 黄金记录冻结的求值规则
        let Expr::Ident(target, tspan) = &arg.value else {
            return Err(AliasError {
                msg: format!("{name} 的参数必须是可变绑定名"),
                span,
            });
        };
        let Some(info) = Scope::get(env, target) else {
            return Err(AliasError {
                msg: format!("'{target}' 未定义"),
                span: *tspan,
            });
        };
        if !info.mutable {
            // Q②: 参数注册为 immutable, 故对参数 increase/decrease 在此拦截
            return Err(AliasError {
                msg: format!("'{target}' 是 val 绑定, 不能 {name}"),
                span: *tspan,
            });
        }
        if info.ty.is_unknown() {
            return Ok(Ty::Unit);
        }
        match info.ty {
            Ty::Int(IntW::W32) => Ok(Ty::Unit),
            ref other => Err(AliasError {
                msg: format!("{name} 需要 i32, 实际 {}", other.name()),
                span: *tspan,
            }),
        }
    }

    /// 方法调用点: 接收者静态类型 → 方法表查找 → 命名实参拒绝 →
    /// 元数 (self 不计) → 实参类型; 返回签名返回类型流入推断。
    /// 接收者类型不可知时级联抑制 (先例: match 主语 / 旧 MethodCall 占位)。
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
        // 数组内建三件套 (Phase 2d): 编译器提供, 元素类型参数化 —
        // 不入名字键方法表 (表按接收者名索引, 泛型元素无法枚举播种),
        // 用户亦无法定义数组方法 (接收者文法不含 '<', 天然不可覆盖)
        if let Ty::Array(elem) = &rt {
            for a in args {
                if let Some(lbl) = &a.label {
                    return Err(AliasError {
                        msg: format!("方法调用不接受命名实参 '{lbl}'"),
                        span: a.span,
                    });
                }
            }
            match name {
                "len" if args.is_empty() => return Ok(Ty::Int(IntW::W32)),
                "push" if args.len() == 1 => {
                    let at = self.expr(&args[0].value, env)?;
                    if !types_match(elem, &at) {
                        return Err(AliasError {
                            msg: format!(
                                "第 1 个实参需要 {}, 实际 {}",
                                elem.name(),
                                at.name()
                            ),
                            span: args[0].value.span(),
                        });
                    }
                    return Ok(Ty::Unit);
                }
                "pop" if args.is_empty() => return Ok((**elem).clone()),
                _ => {}
            }
            let arity = match name {
                "len" | "pop" => 0,
                "push" => 1,
                _ => {
                    return Err(AliasError {
                        msg: format!("类型 {} 上没有方法 '{name}'", rt.name()),
                        span,
                    })
                }
            };
            return Err(AliasError {
                msg: format!("期望 {arity} 个参数, 实际 {} 个", args.len()),
                span,
            });
        }
        let rname = rt.name();
        let Some(sig) =
            self.methods.get(&rname).and_then(|m| m.get(name)).cloned()
        else {
            return Err(AliasError {
                msg: format!("类型 {rname} 上没有方法 '{name}'"),
                span,
            });
        };
        for a in args {
            if let Some(lbl) = &a.label {
                return Err(AliasError {
                    msg: format!("方法调用不接受命名实参 '{lbl}'"),
                    span: a.span,
                });
            }
        }
        let mut ats = Vec::with_capacity(args.len());
        for a in args {
            ats.push(self.expr(&a.value, env)?);
        }
        if args.len() != sig.params.len() {
            return Err(AliasError {
                msg: format!("期望 {} 个参数, 实际 {} 个", sig.params.len(), args.len()),
                span,
            });
        }
        for (i, (want, at)) in sig.params.iter().zip(&ats).enumerate() {
            if !types_match(want, at) {
                return Err(AliasError {
                    msg: format!(
                        "第 {} 个实参需要 {}, 实际 {}",
                        i + 1,
                        want.name(),
                        at.name()
                    ),
                    span: args[i].value.span(),
                });
            }
        }
        Ok(sig.ret)
    }
}

/// 转换内建名 → 目标类型 (Phase 3a 裁决⑤); 非转换名返回 None。
fn conv_builtin_ty(name: &str) -> Option<Ty> {
    let t = match name {
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
    };
    Some(t)
}
