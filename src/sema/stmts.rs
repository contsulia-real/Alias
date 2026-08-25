//! sema::stmts — 语句、块与函数体检查。
//!
//! 拥有: 绑定统一处理 ([`Checker::bind`], 顶层与语句位共用)、
//! 赋值/字段赋值可变性检查、循环条件 bool 校验、return 一致性、
//! 函数字面量体校验 ([`Checker::funclit`]) 与 Q③ 严格落空规则
//! (expr_all_arms_never 终结性判定)。

use super::types::{check_type_slot, types_match, Ty};
use super::{decl_mismatch, Checker, Env, Scope, VarInfo};
use crate::ast::{ArmBody, BindKind, Binding, Body, Expr, Param, Stmt};
use crate::{AliasError, AliasResult, Span};

impl Checker {
    // ---------- 绑定 ----------

    /// val/var/func 绑定统一处理。求值顺序镜像迁移前语义:
    /// 先走初始化器、后插入符号 — 因此递归自引用在此处按未定义处理,
    /// 与运行时行为一致 (recursion 是 forward-spec 特性)。
    pub(super) fn bind(&mut self, b: &Binding, env: &Env) -> AliasResult<()> {
        // 方法定义只在顶层合法 — 语句位置的带接收者绑定在此拦截
        if b.receiver.is_some() {
            return Err(AliasError {
                msg: "方法定义只能出现在顶层".into(),
                span: b.span,
            });
        }
        // 单一命名空间 (Phase 2a): 结构体名不可被绑定/func 重占
        if self.structs.contains_key(&b.name) {
            return Err(AliasError {
                msg: format!("'{}' 已定义为结构体, 不能再定义为绑定", b.name),
                span: b.span,
            });
        }
        let declared = check_type_slot(&b.ty, b.span, &self.structs)?;
        if b.kind == BindKind::Func {
            // func 绑定的类型槽 = 期望返回类型 (语言约定, 见 spec-notes)
            let init_ty = match &b.value {
                Expr::FuncLit { params, body, span } => {
                    self.funclit(params, body, env, Some(&declared), *span)?
                }
                other => self.expr(other, env)?,
            };
            match &init_ty {
                Ty::Func { ret, .. } => {
                    if !types_match(&declared, ret) {
                        return Err(decl_mismatch(b, &declared, ret));
                    }
                }
                Ty::FuncPoly => {
                    if declared != Ty::FuncPoly {
                        return Err(decl_mismatch(b, &declared, &Ty::FuncPoly));
                    }
                }
                Ty::Unknown => {}
                other => return Err(decl_mismatch(b, &declared, other)),
            }
            if b.name == "main" && !init_ty.is_unknown() {
                self.main = Some((init_ty.clone(), b.span));
            }
            Scope::insert(
                env,
                b.name.clone(),
                VarInfo { ty: init_ty, mutable: b.kind == BindKind::Var },
            );
        } else {
            let init_ty = self.expr(&b.value, env)?;
            if !types_match(&declared, &init_ty) {
                return Err(decl_mismatch(b, &declared, &init_ty));
            }
            Scope::insert(
                env,
                b.name.clone(),
                VarInfo { ty: init_ty, mutable: b.kind == BindKind::Var },
            );
        }
        Ok(())
    }

    // ---------- 函数字面量与语句 ----------

    /// 检查函数字面量, 返回推断出的返回类型。
    ///
    /// `expected`: 绑定类型槽给出的期望返回类型 (None = 无标注的裸字面量)。
    /// 返回检查按 expected 逐条进行; 块体落空按 Q③ 保守规则裁决 (见下)。
    pub(super) fn funclit(
        &mut self,
        params: &[Param],
        body: &Body,
        env: &Env,
        expected: Option<&Ty>,
        fspan: Span,
    ) -> AliasResult<Ty> {
        let local = Scope::child(env);
        let mut param_tys = Vec::with_capacity(params.len());
        for p in params {
            let pt = check_type_slot(&p.ty, p.span, &self.structs)?;
            param_tys.push(pt.clone());
            // 参数隐式 val (Q②): immutable 注册, 赋值/increase 在静态期拦截
            Scope::insert(&local, p.name.clone(), VarInfo { ty: pt, mutable: false });
        }
        let ret = match body {
            Body::ArrowExpr(e) => {
                // 压栈使 expr? 等需要外围返回类型的检查在箭头体内同样可见
                // (箭头体无语句, return 检查不经过 fn_ret — 压栈无副作用)
                self.fn_ret.push(expected.cloned().unwrap_or(Ty::Unknown));
                let t = self.expr(e, &local)?;
                self.fn_ret.pop();
                if let Some(d) = expected {
                    if !types_match(d, &t) {
                        return Err(AliasError {
                            msg: format!("return 需要 {}, 实际 {}", d.name(), t.name()),
                            span: e.span(),
                        });
                    }
                    // 声明侧词汇优先 — 构造器单侧推断 (E=Unknown 类) 不外泄
                    d.clone()
                } else {
                    t
                }
            }
            Body::Block(stmts) => {
                self.fn_ret.push(expected.cloned().unwrap_or(Ty::Unknown));
                let mut last_ret: Option<Ty> = None;
                for s in stmts {
                    last_ret = self.stmt(s, &local)?;
                }
                self.fn_ret.pop();

                // Q③ 严格落空规则 (用户终裁, 推翻驱动尾豁免):
                //   声明返回非 unit 的块体, 其末条语句必须是 return。
                //   循环收尾不再豁免 — count_to_ten 语料已补 return 0
                //   (MIGRATION.md Q③ 条目)。
                //   Phase 2b 扩展: 全 never 臂的 match 等价 return 收尾
                //   (每臂 return 即无落空路径 — file_wc.as count 形状)。
                let terminal = match stmts.last() {
                    Some(Stmt::Return { .. }) => true,
                    Some(Stmt::ExprStmt { expr, .. }) => expr_all_arms_never(expr),
                    _ => false,
                };
                let expected_unit =
                    expected.map(|d| *d == Ty::Unit).unwrap_or(true);
                if !expected_unit && !terminal {
                    return Err(AliasError {
                        msg: format!(
                            "返回类型为 {} 的函数体必须以 return 语句收尾",
                            expected.map(|d| d.name()).unwrap_or_else(|| "未知".into())
                        ),
                        span: fspan,
                    });
                }
                // return 收尾 → 返回类型取声明侧词汇 (expected 已知时);
                // 裸字面量才用推断值; unit 函数落空 → Unit
                if terminal {
                    match expected {
                        Some(d) => d.clone(),
                        None => last_ret.unwrap_or(Ty::Unit),
                    }
                } else {
                    Ty::Unit
                }
            }
        };
        Ok(Ty::Func { params: param_tys, ret: Box::new(ret) })
    }

    /// 语句检查。返回 Some(t) 仅当本语句是 return 且其值类型为 t
    /// (供块体末句推断返回类型用)。
    pub(super) fn stmt(&mut self, s: &Stmt, env: &Env) -> AliasResult<Option<Ty>> {
        match s {
            Stmt::Binding(b) => {
                self.bind(b, env)?;
                Ok(None)
            }
            Stmt::Assign { target, value, span } => {
                // 先值后目标 — 黄金记录冻结的求值顺序
                self.expr(value, env)?;
                match Scope::get(env, target) {
                    None => Err(AliasError {
                        msg: format!("赋值目标 '{target}' 未定义"),
                        span: *span,
                    }),
                    Some(info) if !info.mutable => Err(AliasError {
                        msg: format!("'{target}' 是 val 绑定, 不可重新赋值"),
                        span: *span,
                    }),
                    // 赋值不做类型一致性检查 — D3 未列赋值, 不私自收紧
                    Some(_) => Ok(None),
                }
            }
            Stmt::FieldAssign { recv, field, value, span } => {
                // 先值后目标 — 与简名赋值同序; 字段级可变性独立于
                // 绑定可变性 (Phase 2a 裁决): 只看字段自身的 val/var
                self.expr(value, env)?;
                let rt = self.expr(recv, env)?;
                if rt.is_unknown() {
                    return Ok(None);
                }
                match rt {
                    Ty::Struct(s) => {
                        let info = &self.structs[&s];
                        let Some(f) = info.fields.iter().find(|fi| fi.name == *field) else {
                            return Err(AliasError {
                                msg: format!("结构体 {s} 没有字段 '{field}'"),
                                span: *span,
                            });
                        };
                        if !f.mutable {
                            return Err(AliasError {
                                msg: format!("'{field}' 是 val 字段, 不可赋值"),
                                span: *span,
                            });
                        }
                        Ok(None)
                    }
                    other => Err(AliasError {
                        msg: format!("{} 没有字段 '{}'", other.name(), field),
                        span: *span,
                    }),
                }
            }
            Stmt::ExprStmt { expr, .. } => {
                self.expr(expr, env)?;
                Ok(None)
            }
            Stmt::Return { value, span } => {
                let t = match value {
                    Some(e) => self.expr(e, env)?,
                    None => Ty::Unit,
                };
                match self.fn_ret.last() {
                    None => Err(AliasError {
                        msg: "顶层不允许 return".into(),
                        span: *span,
                    }),
                    Some(d) if !types_match(d, &t) => Err(AliasError {
                        msg: format!("return 需要 {}, 实际 {}", d.name(), t.name()),
                        span: value.as_ref().map(Expr::span).unwrap_or(*span),
                    }),
                    _ => Ok(Some(t)),
                }
            }
            Stmt::For { cond, body, span } => self.loop_stmt("for", cond, body, *span, env),
            Stmt::While { cond, body, span } => {
                self.loop_stmt("while", cond, body, *span, env)
            }
        }
    }

    /// 循环: 条件须 bool (消息/span 对齐迁移前报错);
    /// 体在子作用域检查一次 — 静态近似每迭代新作用域, 检查结论等价。
    fn loop_stmt(
        &mut self,
        kw: &str,
        cond: &Expr,
        body: &[Stmt],
        span: Span,
        env: &Env,
    ) -> AliasResult<Option<Ty>> {
        let ct = self.expr(cond, env)?;
        if !ct.is_unknown() && ct != Ty::Bool {
            return Err(AliasError {
                msg: format!("{kw} 条件需要 bool, 实际 {}", ct.name()),
                span,
            });
        }
        let child = Scope::child(env);
        for s in body {
            self.stmt(s, &child)?;
        }
        Ok(None)
    }
}

// ---------- Q③ 落空判定 ----------

/// match 表达式是否所有臂皆为 never 流 (Ret 臂 / return 收尾块臂 /
/// 递归地以全 never match 收尾的块臂) — Q③ 终结性判定用。
fn expr_all_arms_never(e: &Expr) -> bool {
    match e {
        Expr::Match { arms, .. } => arms.iter().all(|a| arm_body_never(&a.body)),
        _ => false,
    }
}

fn arm_body_never(b: &ArmBody) -> bool {
    match b {
        ArmBody::Ret(_) => true,
        ArmBody::Value(_) => false,
        ArmBody::Block(stmts) => match stmts.last() {
            Some(Stmt::Return { .. }) => true,
            Some(Stmt::ExprStmt { expr, .. }) => expr_all_arms_never(expr),
            _ => false,
        },
    }
}
