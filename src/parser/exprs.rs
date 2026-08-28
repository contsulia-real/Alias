//! parser::exprs — 表达式解析 facade。
//!
//! 优先级与原规则保持不变；实现按职责拆为 precedence / postfix / atoms。

use super::{validate_nesting, Parser, MAX_EXPR_CHAIN};
use crate::ast::{
    ArmBody, BinOp, Body, CallArg, CtorKind, Expr, MatchArm, Param, Pattern, StrPartAst,
};
use crate::lexer::{StrPart, Tok, Token};
use crate::{AliasError, AliasResult};

mod atoms;
mod postfix;
mod precedence;
