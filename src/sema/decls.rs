//! sema::decls — 顶层声明登记与校验。
//!
//! 拥有: struct 表构建 ([`Checker::struct_def`])、扩展方法签名注册
//! ([`Checker::method_def`])、Q④ main 校验 ([`Checker::validate_main`])。
//! 单一命名空间的重名拦截在此收口; 签名先入表后查体 — 方法可递归。

use super::types::{check_type_slot, types_match, IntW, Ty};
use super::{literal_slot_unify, Checker, Env, FieldInfo, MethodInfo, Scope, StructInfo};
use crate::ast::{Binding, Expr, Param, StructDef, TypeExpr};
use crate::{AliasError, AliasResult, Span};

impl Checker {
    // ---------- 结构体定义 (Phase 2a) ----------

    /// 登记结构体: 重名拦截 (单一命名空间) + 字段类型槽校验 +
    /// 字段重名/默认值一致性检查。字段类型只见表内已登记结构体 —
    /// 声明前不可见, 与绑定同序。
    pub(super) fn struct_def(&mut self, sd: &StructDef, env: &Env) -> AliasResult<()> {
        if self.structs.contains_key(&sd.name) {
            return Err(AliasError {
                msg: format!("'{}' 已定义为结构体, 不能重复定义", sd.name),
                span: sd.span,
            });
        }
        if Scope::get(env, &sd.name).is_some() {
            return Err(AliasError {
                msg: format!("'{}' 已定义为绑定, 不能再定义为结构体", sd.name),
                span: sd.span,
            });
        }
        let mut fields: Vec<FieldInfo> = Vec::with_capacity(sd.fields.len());
        for f in &sd.fields {
            if fields.iter().any(|fi: &FieldInfo| fi.name == f.name) {
                return Err(AliasError {
                    msg: format!("结构体 {} 重复定义字段 '{}'", sd.name, f.name),
                    span: f.span,
                });
            }
            let ty = check_type_slot(&f.ty, f.span, &self.structs)?;
            if let Some(d) = &f.default {
                // 默认值按声明期词汇环境校验 (构造期求值 — 无顶层副作用);
                // 裸数值字面量同样经槽位统一
                let dt = match literal_slot_unify(&ty, d) {
                    Some(r) => r?,
                    None => self.expr(d, env)?,
                };
                if !types_match(&ty, &dt) {
                    return Err(AliasError {
                        msg: format!(
                            "字段 '{}' 声明类型为 {}, 实际 {}",
                            f.name,
                            ty.name(),
                            dt.name()
                        ),
                        span: d.span(),
                    });
                }
            }
            fields.push(FieldInfo {
                name: f.name.clone(),
                mutable: f.mutable,
                ty,
                has_default: f.default.is_some(),
            });
        }
        self.structs.insert(sd.name.clone(), StructInfo { fields });
        Ok(())
    }

    // ---------- 方法定义与调用点 (Phase 2c) ----------

    /// 扩展方法定义: public? func <Ret> <RecvType>.<name> = (params) -> 体。
    /// 接收者 ∈ {string, bool, i32, 已登记结构体}; self 为隐式首参数
    /// (val 语义, 类型 = 接收者); 签名先入表后查体 — 方法可递归。
    pub(super) fn method_def(&mut self, b: &Binding, env: &Env) -> AliasResult<()> {
        let Some((recv, mname)) = b.receiver.clone() else {
            return Err(AliasError {
                msg: "内部: 无接收者的方法定义".into(),
                span: b.span,
            });
        };
        // 接收者合法性: 内建标量类型或已登记结构体; 已知但非法的类型
        // (unit/func) 与未知名各有独立诊断
        if !matches!(recv.as_str(), "string" | "bool" | "i32")
            && !self.structs.contains_key(&recv)
        {
            if recv == "unit" || recv == "func" {
                return Err(AliasError {
                    msg: format!("类型 {recv} 不能作为方法接收者"),
                    span: b.span,
                });
            }
            return Err(AliasError {
                msg: format!("未知类型名 '{recv}'"),
                span: b.span,
            });
        }
        let declared = check_type_slot(&b.ty, b.span, &self.structs)?;
        let Expr::FuncLit { params, body, span: fspan } = &b.value else {
            return Err(AliasError {
                msg: format!("方法 {recv}.{mname} 的体必须是函数字面量"),
                span: b.span,
            });
        };
        let mut ptys = Vec::with_capacity(params.len());
        for p in params {
            ptys.push(check_type_slot(&p.ty, p.span, &self.structs)?);
        }
        {
            let table = self.methods.entry(recv.clone()).or_default();
            if let Some(existing) = table.get(&mname) {
                if existing.builtin {
                    return Err(AliasError {
                        msg: format!("内建方法不可覆盖: {recv}.{mname}"),
                        span: b.span,
                    });
                }
                return Err(AliasError {
                    msg: format!("类型 {recv} 上已定义方法 '{mname}'"),
                    span: b.span,
                });
            }
            table.insert(
                mname.clone(),
                MethodInfo { params: ptys, ret: declared.clone(), public: b.public, builtin: false },
            );
        }
        // 体检查: self 注入为首个参数 (Q② val 语义 → 赋值/increase 静态拦截);
        // 返回类型槽即期望返回类型 — Q③/return 一致性/推断全部复用 funclit
        let self_param =
            Param { ty: TypeExpr::Named(recv.clone()), name: "self".into(), span: b.span };
        let mut all_params = Vec::with_capacity(params.len() + 1);
        all_params.push(self_param);
        all_params.extend(params.iter().cloned());
        self.funclit(&all_params, body, env, Some(&declared), *fspan)?;
        Ok(())
    }

    /// Q④ main 校验: 存在 / 零参 / 返回 ∈ {i32,bool,string,unit}。
    /// kind 非 Func 或初始化非函数值的 main 不入候选 — 与迁移前判定
    /// 的判定一致, 同样落入「找不到顶层 func main」。
    pub(super) fn validate_main(&mut self) -> AliasResult<()> {
        let Some((sig, bspan)) = self.main.take() else {
            // Q⑤: Span 为 default 时 Display 省略位置前缀 (lib.rs)
            return Err(AliasError {
                msg: "找不到顶层 func main".into(),
                span: Span::default(),
            });
        };
        match sig {
            Ty::Func { params, ret } => {
                if !params.is_empty() {
                    return Err(AliasError {
                        msg: "顶层 func main 不能声明参数".into(),
                        span: bspan,
                    });
                }
                if matches!(*ret, Ty::Int(IntW::W32) | Ty::Bool | Ty::Str | Ty::Unit) {
                    Ok(())
                } else {
                    Err(AliasError {
                        msg: format!(
                            "顶层 func main 返回类型必须是 i32/bool/string/unit, 实际 {}",
                            ret.name()
                        ),
                        span: bspan,
                    })
                }
            }
            // 多态函数值做 main: 签名不可知, 归入返回类型非法
            _ => Err(AliasError {
                msg: format!(
                    "顶层 func main 返回类型必须是 i32/bool/string/unit, 实际 {}",
                    sig.name()
                ),
                span: bspan,
            }),
        }
    }
}
