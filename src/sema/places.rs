//! 可赋值 Place 的静态解析与目标检查。
//!
//! 语句检查只负责控制流编排；“这个赋值写到哪里、目标是否可写、RHS 应采用什么类型”
//! 属于 Place 语义。该规则只在这里实现，HIR lowering 与 codegen 只能消费已解析结果。

use super::hir::LowerPlaceInfo;
use super::{Checker, Env, Scope};
use crate::ast::{Expr, Stmt};
use crate::sema::types::Ty;
use crate::{AliasError, AliasResult, Span};

impl Checker {
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
            },
        );
        self.expr_expected(value, env, &info.ty).map_err(|error| {
            let error = error.into_alias();
            AliasError {
                msg: format!("赋值目标 '{target}' 需要 {}: {}", info.ty.name(), error.msg),
                span: error.span,
            }
        })?;
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
        let recv_ty = self.expr(recv, env)?;
        if recv_ty.is_unknown() {
            self.expr(value, env)?;
            return Ok(());
        }
        let Ty::Struct(struct_name) = recv_ty else {
            return Err(AliasError {
                msg: format!("{} 没有字段 '{}'", recv_ty.name(), field),
                span,
            });
        };
        let (field_index, field_info) = self.struct_field(&struct_name, field, span)?;
        if !field_info.mutable {
            return Err(AliasError {
                msg: format!("'{field}' 是 val 字段, 不可赋值"),
                span,
            });
        }

        // FieldAssign 与普通 Assign 共用同一 AST-Stmt 生命周期约束。一个语句只记录一个
        // LowerPlaceInfo，避免 local/field 各自维护平行 fact 表并在后续 Index/Deref 扩散。
        self.assignment_places.insert(
            stmt as *const Stmt as usize,
            LowerPlaceInfo::Field { field_index },
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
        Ok(())
    }
}
