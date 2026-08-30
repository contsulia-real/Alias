//! 可寻址 Place 的静态解析与目标检查。
//!
//! 语句检查只负责控制流编排；“这个操作指向哪段 storage、projection 类型是什么、赋值
//! 终端是否可写”属于 Place 语义。HIR lowering 与 codegen 只能消费已解析结果。

use super::hir::LowerPlaceInfo;
use super::types::{IntW, Ty};
use super::{Checker, Env, Scope};
use crate::ast::{Expr, Stmt};
use crate::{AliasError, AliasResult, Span};

impl Checker {
    /// 把一个源码表达式解析为结构化 Place。只有真正表示已有 storage 的 Ident/Field/Index
    /// 可以进入这里；constructor/call/ternary 等 Value 不能因为当前物理表示像 pointer 就被
    /// 后端当成 Place。Index 的用户表达式仍通过普通 expr checker 固化其类型 facts。
    pub(super) fn resolve_place_expr(
        &mut self,
        expr: &Expr,
        env: &Env,
    ) -> AliasResult<LowerPlaceInfo> {
        match expr {
            Expr::Ident(name, span) => {
                let Some(info) = Scope::get(env, name) else {
                    return Err(AliasError {
                        msg: format!("Place 根 '{name}' 未定义"),
                        span: *span,
                    });
                };
                Ok(LowerPlaceInfo::Local {
                    binding_id: info.id,
                    ty: info.ty,
                })
            }
            Expr::Field { recv, name, span } => {
                let base = self.resolve_place_expr(recv, env)?;
                let Ty::Struct(struct_name) = base.ty() else {
                    return Err(AliasError {
                        msg: format!("{} 没有字段 '{name}'", base.ty().name()),
                        span: *span,
                    });
                };
                let (field_index, field) = self.struct_field(struct_name, name, *span)?;
                Ok(LowerPlaceInfo::Field {
                    base: Box::new(base),
                    field_index,
                    ty: field.ty,
                })
            }
            Expr::Index { recv, idx, span } => {
                let base = self.resolve_place_expr(recv, env)?;
                let Ty::Array(elem) = base.ty() else {
                    return Err(AliasError {
                        msg: format!("下标 Place 需要 array 类型, 实际 {}", base.ty().name()),
                        span: *span,
                    });
                };
                let elem_ty = (**elem).clone();
                let index_ty = self.expr(idx, env)?;
                if !index_ty.is_unknown() && index_ty != Ty::Int(IntW::W32) {
                    return Err(AliasError {
                        msg: format!("下标需要 i32, 实际 {}", index_ty.name()),
                        span: idx.span(),
                    });
                }
                Ok(LowerPlaceInfo::Index {
                    base: Box::new(base),
                    ty: elem_ty,
                })
            }
            _ => Err(AliasError {
                msg: "该表达式不是可寻址 Place".into(),
                span: expr.span(),
            }),
        }
    }

    pub(super) fn check_local_assignment(
        &mut self,
        stmt: &Stmt,
        target: &str,
        value: &Expr,
        span: Span,
        env: &Env,
    ) -> AliasResult<()> {
        let Some(info) = Scope::get(env, target) else {
            return Err(AliasError {
                msg: format!("赋值目标 '{target}' 未定义"),
                span,
            });
        };
        if !info.mutable {
            return Err(AliasError {
                msg: format!("'{target}' 是 val 绑定, 不可重新赋值"),
                span,
            });
        }

        // 该地址只在本次 check → lower 链内作为临时 fact identity。若未来 AST
        // rewrite 会移动 Stmt，必须先换稳定 NodeId；不能用 lookup fallback 掩盖错配。
        self.assignment_places.insert(
            stmt as *const Stmt as usize,
            LowerPlaceInfo::Local {
                binding_id: info.id,
                ty: info.ty.clone(),
            },
        );
        self.expr_expected(value, env, &info.ty).map_err(|error| {
            let error = error.into_alias();
            AliasError {
                msg: format!("赋值目标 '{target}' 需要 {}: {}", info.ty.name(), error.msg),
                span: error.span,
            }
        })?;
        self.record_owning_slot_read(value, env, &info.ty)?;
        Ok(())
    }

    pub(super) fn check_field_assignment(
        &mut self,
        stmt: &Stmt,
        recv: &Expr,
        field: &str,
        value: &Expr,
        span: Span,
        env: &Env,
    ) -> AliasResult<()> {
        let base = self.resolve_place_expr(recv, env)?;
        let Ty::Struct(struct_name) = base.ty() else {
            return Err(AliasError {
                msg: format!("{} 没有字段 '{field}'", base.ty().name()),
                span,
            });
        };
        let (field_index, field_info) = self.struct_field(struct_name, field, span)?;
        if !field_info.mutable {
            return Err(AliasError {
                msg: format!("'{field}' 是 val 字段, 不可赋值"),
                span,
            });
        }

        // 源码 FieldAssign 与普通 Assign 仍共用一个 statement fact，但其内容现在是完整递归
        // Place projection；后续 borrow/move 不再需要从 terminal field 反向恢复 base identity。
        self.assignment_places.insert(
            stmt as *const Stmt as usize,
            LowerPlaceInfo::Field {
                base: Box::new(base),
                field_index,
                ty: field_info.ty.clone(),
            },
        );
        self.expr_expected(value, env, &field_info.ty)
            .map_err(|error| {
                let error = error.into_alias();
                AliasError {
                    msg: format!(
                        "字段 '{field}' 需要 {}: {}",
                        field_info.ty.name(),
                        error.msg
                    ),
                    span: error.span,
                }
            })?;
        self.record_owning_slot_read(value, env, &field_info.ty)?;
        Ok(())
    }
}
