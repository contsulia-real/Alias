//! sema::decls — 顶层声明登记与校验。
//!
//! 拥有: struct 表构建、扩展方法签名注册、main 校验。
//! 单一命名空间的重名拦截在此收口；用户方法在第一次签名登记时同时分配 MethodId，
//! 后续 HIR/codegen 不再按接收者名字 + 方法名字重新解析。

use super::exprs::ExprCheckError;
use super::hir::BindingId;
use super::types::{check_return_type_slot, check_value_type_slot, IntW, Ty};
use super::{
    builtin_method, ensure_user_lexical_name, Checker, Env, FieldInfo, MethodInfo, Scope,
    StructInfo,
};
use crate::ast::{Binding, Expr, Param, StructDef};
use crate::{AliasError, AliasResult, Span};

impl Checker {
    pub(super) fn struct_def(&mut self, sd: &StructDef, env: &Env) -> AliasResult<()> {
        ensure_user_lexical_name(&sd.name, sd.span)?;
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
            // StructField fact 与其它 check→lower fact 共用短生命周期 AST 地址 identity；
            // lowering 前替换字段节点会让默认值类型关联到过期地址。
            self.field_types
                .insert(f as *const crate::ast::StructField as usize, ty.clone());
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

    /// 扩展函数定义: pub? func <Ret> <ReceiverType>.<name> = (params) -> 体。
    /// 签名第一次登记即固化 MethodId；self 第一次进入函数作用域即固化 BindingId。
    pub(super) fn method_def(&mut self, b: &Binding, env: &Env) -> AliasResult<()> {
        let _binding_id = self.binding_id_for(b)?;
        let Some(recv_expr) = b.receiver.clone() else {
            return Err(AliasError {
                msg: "内部: 无接收者的方法定义".into(),
                span: b.span,
            });
        };
        let recv_ty = check_value_type_slot(&recv_expr, b.span, &self.structs)?;
        self.receiver_types
            .insert(b as *const Binding as usize, recv_ty.clone());
        let recv = recv_ty.name();
        let mname = b.name.clone();
        if builtin_method(&recv_ty, &mname).is_some() {
            return Err(AliasError {
                msg: format!("内建方法不可覆盖: {recv}.{mname}"),
                span: b.span,
            });
        }
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
            ensure_user_lexical_name(&p.name, p.span)?;
            ptys.push(check_value_type_slot(&p.ty, p.span, &self.structs)?);
        }

        if self
            .methods
            .get(&recv)
            .and_then(|table| table.get(&mname))
            .is_some()
        {
            return Err(AliasError {
                msg: format!("类型 {recv} 上已定义方法 '{mname}'"),
                span: b.span,
            });
        }
        let method_id = self.fresh_method_id()?;
        self.methods.entry(recv.clone()).or_default().insert(
            mname.clone(),
            MethodInfo::User {
                id: method_id,
                params: ptys,
                ret: declared.clone(),
            },
        );
        self.method_ids
            .insert(b as *const Binding as usize, method_id);

        let self_param = Param {
            ty: recv_expr,
            name: "self".into(),
            span: b.span,
        };
        let mut all_params = Vec::with_capacity(params.len() + 1);
        all_params.push(self_param);
        all_params.extend(params.iter().cloned());
        let (method_ty, all_param_ids) =
            self.funclit(&all_params, body, env, Some(&declared), *fspan)?;
        let Ty::Func {
            params: all_param_types,
            ..
        } = &method_ty
        else {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: funclit 未产生函数类型".into(),
                span: *fspan,
            });
        };
        let Some(self_id) = all_param_ids.first().copied() else {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: 方法缺少隐式 self 参数".into(),
                span: *fspan,
            });
        };
        self.method_self_ids
            .insert(b as *const Binding as usize, self_id);
        self.record_params(params, &all_param_types[1..], &all_param_ids[1..])?;
        self.record_expr_type(&b.value, method_ty.clone());
        self.binding_types
            .insert(b as *const Binding as usize, method_ty);
        Ok(())
    }

    /// main 校验: 存在 / 零参 / 返回必须为 i32。成功后返回 sema 已固化的入口 BindingId。
    pub(super) fn validate_main(&mut self) -> AliasResult<BindingId> {
        let Some((main_id, sig, bspan)) = self.main.take() else {
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
                    Ok(main_id)
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
