//! sema::decls — 顶层声明登记与校验。
//!
//! 拥有: struct 表构建 ([`Checker::struct_def`])、扩展方法签名注册
//! ([`Checker::method_def`])、Q④ main 校验 ([`Checker::validate_main`])。
//! 单一命名空间的重名拦截在此收口; 签名先入表后查体 — 方法可递归。

use super::exprs::ExprCheckError;
use super::types::{check_return_type_slot, check_value_type_slot, IntW, Ty};
use super::{Checker, Env, FieldInfo, MethodInfo, Scope, StructInfo};
use crate::ast::{Binding, Expr, Param, StructDef};
use crate::{AliasError, AliasResult, Span};

impl Checker {
    // ---------- 结构体定义 (Phase 2a) ----------

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
            let ty = check_value_type_slot(&f.ty, f.span, &self.structs)?;
            if let Some(d) = &f.default {
                self.expr_expected(d, env, &ty)
                    .map_err(|error| match error {
                        ExprCheckError::Mismatch { actual, span, .. } => AliasError {
                            msg: format!(
                                "字段 '{}' 声明类型为 {}, 实际 {}",
                                f.name,
                                ty.name(),
                                actual.name()
                            ),
                            span,
                        },
                        other => other.into_alias(),
                    })?;
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

    // ---------- 扩展函数定义 ----------

    /// 扩展函数定义: pub? func <Ret> <ReceiverType>.<name> = (params) -> 体。
    /// 所有合法 Alias 值类型都可作为 receiver。
    /// self 为隐式首参数 (val 语义, 类型 = 完整 receiver 类型)。
    pub(super) fn method_def(&mut self, b: &Binding, env: &Env) -> AliasResult<()> {
        let Some(recv_expr) = b.receiver.clone() else {
            return Err(AliasError {
                msg: "内部: 无接收者的方法定义".into(),
                span: b.span,
            });
        };
        let recv_ty = check_value_type_slot(&recv_expr, b.span, &self.structs)?;
        let recv = recv_ty.name();
        let mname = b.name.clone();
        let declared = check_return_type_slot(&b.ty, b.span, &self.structs)?;
        let Expr::FuncLit {
            params,
            body,
            span: fspan,
        } = &b.value
        else {
            return Err(AliasError {
                msg: format!("方法 {recv}.{mname} 的体必须是函数字面量"),
                span: b.span,
            });
        };
        let mut ptys = Vec::with_capacity(params.len());
        for p in params {
            ptys.push(check_value_type_slot(&p.ty, p.span, &self.structs)?);
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
                MethodInfo {
                    params: ptys,
                    ret: declared.clone(),
                    is_pub: b.is_pub,
                    builtin: false,
                },
            );
        }

        let self_param = Param {
            ty: recv_expr,
            name: "self".into(),
            span: b.span,
        };
        let mut all_params = Vec::with_capacity(params.len() + 1);
        all_params.push(self_param);
        all_params.extend(params.iter().cloned());
        self.funclit(&all_params, body, env, Some(&declared), *fspan)?;
        Ok(())
    }

    /// Q④ main 校验: 存在 / 零参 / 返回必须为 i32。
    pub(super) fn validate_main(&mut self) -> AliasResult<()> {
        let Some((sig, bspan)) = self.main.take() else {
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
                if matches!(*ret, Ty::Int(IntW::W32)) {
                    Ok(())
                } else {
                    Err(AliasError {
                        msg: format!("顶层 func main 返回类型必须是 i32, 实际 {}", ret.name()),
                        span: bspan,
                    })
                }
            }
            _ => Err(AliasError {
                msg: format!("顶层 func main 返回类型必须是 i32, 实际 {}", sig.name()),
                span: bspan,
            }),
        }
    }
}
