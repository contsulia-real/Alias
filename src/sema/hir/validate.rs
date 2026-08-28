use super::{
    ArmBody, Binding, BindingId, BindingOwner, Body, BuiltinCall, CallArg, CallTarget,
    CheckedProgram, CtorKind, Expr, Item, MethodId, MethodTarget, Stmt, StrPart,
};
use crate::builtins::{classify_call_builtin, classify_result_constructor, CallBuiltinName};
use crate::sema::types::{types_match, IntW, Ty};
use crate::{AliasError, AliasResult, Span};
use std::collections::{HashMap, HashSet};

enum HirValidationNode<'a> {
    Expr(&'a Expr),
    Stmt(&'a Stmt),
}

#[derive(Clone)]
struct UserMethodContract {
    receiver: Ty,
    params: Vec<Ty>,
    ret: Ty,
}

#[derive(Clone)]
struct StructFieldContract {
    ty: Ty,
    has_default: bool,
}

#[derive(Clone)]
struct StructContract {
    fields: Vec<StructFieldContract>,
}

fn invariant(span: Span, msg: impl Into<String>) -> AliasError {
    AliasError {
        msg: format!("内部 sema 不变式被破坏: {}", msg.into()),
        span,
    }
}

fn push_validation_body<'a>(stack: &mut Vec<HirValidationNode<'a>>, body: &'a Body) {
    match body {
        Body::Block(stmts) => {
            for stmt in stmts.iter().rev() {
                stack.push(HirValidationNode::Stmt(stmt));
            }
        }
        Body::Single(stmt) => stack.push(HirValidationNode::Stmt(stmt)),
    }
}

fn push_match_children<'a>(
    stack: &mut Vec<HirValidationNode<'a>>,
    subject: &'a Expr,
    arms: &'a [super::MatchArm],
) {
    for arm in arms.iter().rev() {
        match &arm.body {
            ArmBody::Block(stmts) => {
                for stmt in stmts.iter().rev() {
                    stack.push(HirValidationNode::Stmt(stmt));
                }
            }
            ArmBody::Value(value) | ArmBody::Ret(value) => {
                stack.push(HirValidationNode::Expr(value));
            }
        }
    }
    stack.push(HirValidationNode::Expr(subject));
}

fn push_expr_children<'a>(stack: &mut Vec<HirValidationNode<'a>>, expr: &'a Expr) {
    match expr {
        Expr::Str(parts, ..) => {
            for part in parts.iter().rev() {
                if let StrPart::Hole(hole) = part {
                    stack.push(HirValidationNode::Expr(hole));
                }
            }
        }
        Expr::Cast { expr, .. }
        | Expr::Convert { expr, .. }
        | Expr::Neg { expr, .. }
        | Expr::Not { expr, .. }
        | Expr::BitNot { expr, .. }
        | Expr::Propagate { expr, .. } => stack.push(HirValidationNode::Expr(expr)),
        Expr::Binary { lhs, rhs, .. } => {
            stack.push(HirValidationNode::Expr(rhs));
            stack.push(HirValidationNode::Expr(lhs));
        }
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            ..
        } => {
            stack.push(HirValidationNode::Expr(else_expr));
            stack.push(HirValidationNode::Expr(then_expr));
            stack.push(HirValidationNode::Expr(cond));
        }
        Expr::Call { callee, args, .. } => {
            for arg in args.iter().rev() {
                stack.push(HirValidationNode::Expr(&arg.value));
            }
            if matches!(
                expr,
                Expr::Call {
                    target: CallTarget::FunctionValue,
                    ..
                }
            ) {
                stack.push(HirValidationNode::Expr(callee));
            }
        }
        Expr::MethodCall { recv, args, .. } => {
            for arg in args.iter().rev() {
                stack.push(HirValidationNode::Expr(&arg.value));
            }
            stack.push(HirValidationNode::Expr(recv));
        }
        Expr::Field { recv, .. } => stack.push(HirValidationNode::Expr(recv)),
        Expr::Index { recv, idx, .. } => {
            stack.push(HirValidationNode::Expr(idx));
            stack.push(HirValidationNode::Expr(recv));
        }
        Expr::ArrayLit { elems, .. } => {
            for elem in elems.iter().rev() {
                stack.push(HirValidationNode::Expr(elem));
            }
        }
        Expr::Match { subject, arms, .. } => push_match_children(stack, subject, arms),
        Expr::FuncLit { body, .. } => push_validation_body(stack, body),
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..)
        | Expr::Typeof { .. } => {}
    }
}

fn push_stmt_children<'a>(stack: &mut Vec<HirValidationNode<'a>>, stmt: &'a Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(HirValidationNode::Expr(&binding.value)),
        Stmt::Assign { value, .. } => stack.push(HirValidationNode::Expr(value)),
        Stmt::FieldAssign { recv, value, .. } => {
            stack.push(HirValidationNode::Expr(value));
            stack.push(HirValidationNode::Expr(recv));
        }
        Stmt::Expr { expr } => stack.push(HirValidationNode::Expr(expr)),
        Stmt::Return { value } => {
            if let Some(value) = value {
                stack.push(HirValidationNode::Expr(value));
            }
        }
        Stmt::If {
            branches,
            else_body,
        } => {
            if let Some(body) = else_body {
                for stmt in body.iter().rev() {
                    stack.push(HirValidationNode::Stmt(stmt));
                }
            }
            for (cond, body) in branches.iter().rev() {
                for stmt in body.iter().rev() {
                    stack.push(HirValidationNode::Stmt(stmt));
                }
                stack.push(HirValidationNode::Expr(cond));
            }
        }
        Stmt::While { cond, body } => {
            for stmt in body.iter().rev() {
                stack.push(HirValidationNode::Stmt(stmt));
            }
            stack.push(HirValidationNode::Expr(cond));
        }
        Stmt::For { iterable, body, .. } => {
            for stmt in body.iter().rev() {
                stack.push(HirValidationNode::Stmt(stmt));
            }
            stack.push(HirValidationNode::Expr(iterable));
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn validate_binding_contract(binding: &Binding) -> AliasResult<()> {
    if binding.ty.contains_unknown() {
        return Err(invariant(binding.span, "绑定类型未确定"));
    }
    if !types_match(&binding.ty, binding.value.ty()) {
        return Err(invariant(
            binding.span,
            "绑定类型与初始化 HIR 表达式类型不一致",
        ));
    }
    Ok(())
}

fn collect_declared_ids(program: &CheckedProgram) -> HashSet<BindingId> {
    let mut ids = HashSet::new();
    let mut stack = Vec::new();
    for item in program.items.iter().rev() {
        match item {
            Item::Binding(binding) => {
                ids.insert(binding.binding_id);
                if let BindingOwner::Method { self_id, .. } = &binding.owner {
                    ids.insert(*self_id);
                }
                stack.push(HirValidationNode::Expr(&binding.value));
            }
            Item::StructDef(def) => {
                for field in def.fields.iter().rev() {
                    if let Some(default) = &field.default {
                        stack.push(HirValidationNode::Expr(default));
                    }
                }
            }
        }
    }
    while let Some(node) = stack.pop() {
        match node {
            HirValidationNode::Expr(expr) => {
                match expr {
                    Expr::FuncLit {
                        params,
                        implicit_bindings,
                        ..
                    } => {
                        ids.extend(params.iter().map(|param| param.binding_id));
                        ids.extend(implicit_bindings.iter().copied());
                    }
                    Expr::Match { arms, .. } => {
                        ids.extend(arms.iter().filter_map(|arm| arm.binding_id));
                    }
                    _ => {}
                }
                push_expr_children(&mut stack, expr);
            }
            HirValidationNode::Stmt(stmt) => {
                match stmt {
                    Stmt::Binding(binding) => {
                        ids.insert(binding.binding_id);
                    }
                    Stmt::For { binding_id, .. } => {
                        ids.insert(*binding_id);
                    }
                    _ => {}
                }
                push_stmt_children(&mut stack, stmt);
            }
        }
    }
    ids
}

fn collect_struct_contracts(
    program: &CheckedProgram,
) -> AliasResult<HashMap<String, StructContract>> {
    let mut structs = HashMap::new();
    for item in &program.items {
        let Item::StructDef(def) = item else {
            continue;
        };
        let contract = StructContract {
            fields: def
                .fields
                .iter()
                .map(|field| StructFieldContract {
                    ty: field.ty.clone(),
                    has_default: field.default.is_some(),
                })
                .collect(),
        };
        if structs.insert(def.name.clone(), contract).is_some() {
            return Err(invariant(
                Span::default(),
                format!("结构体 '{}' 在 HIR 中重复", def.name),
            ));
        }
    }
    Ok(structs)
}

fn collect_user_methods(
    program: &CheckedProgram,
) -> AliasResult<HashMap<MethodId, UserMethodContract>> {
    let mut methods = HashMap::new();
    for item in &program.items {
        let Item::Binding(binding) = item else {
            continue;
        };
        let BindingOwner::Method {
            method_id,
            self_id,
            receiver,
        } = &binding.owner
        else {
            continue;
        };
        if receiver.contains_unknown() {
            return Err(invariant(binding.span, "用户方法接收者类型未确定"));
        }
        let Ty::Func { params, ret } = &binding.ty else {
            return Err(invariant(binding.span, "用户方法绑定缺少完整函数类型"));
        };
        if params.first() != Some(receiver) {
            return Err(invariant(
                binding.span,
                "用户方法函数类型首参数与接收者不一致",
            ));
        }
        let Expr::FuncLit {
            implicit_bindings, ..
        } = &binding.value
        else {
            return Err(invariant(binding.span, "用户方法值不是 FuncLit"));
        };
        if implicit_bindings.as_slice() != [*self_id] {
            return Err(invariant(
                binding.span,
                "用户方法 FuncLit 未唯一携带已解析 self BindingId",
            ));
        }
        let contract = UserMethodContract {
            receiver: receiver.clone(),
            params: params[1..].to_vec(),
            ret: (**ret).clone(),
        };
        if methods.insert(*method_id, contract).is_some() {
            return Err(invariant(
                binding.span,
                format!("MethodId 重复 {method_id:?}"),
            ));
        }
    }
    Ok(methods)
}

fn validate_main_contract(program: &CheckedProgram) -> AliasResult<()> {
    let mut mains = program.items.iter().filter_map(|item| match item {
        Item::Binding(binding) if !binding.is_method() && binding.name == "main" => Some(binding),
        _ => None,
    });
    let Some(main) = mains.next() else {
        return Err(invariant(Span::default(), "HIR 缺少顶层 main 绑定"));
    };
    if mains.next().is_some() {
        return Err(invariant(Span::default(), "HIR 含多个顶层 main 绑定"));
    }
    if main.binding_id != program.main_id {
        return Err(invariant(
            main.span,
            "CheckedProgram.main_id 未指向顶层 main 绑定",
        ));
    }
    let Ty::Func { params, ret } = &main.ty else {
        return Err(invariant(main.span, "顶层 main HIR 类型不是函数"));
    };
    if !params.is_empty() || !matches!(ret.as_ref(), Ty::Int(IntW::W32)) {
        return Err(invariant(main.span, "顶层 main HIR 签名不是 () -> i32"));
    }
    Ok(())
}

fn collect_function_locals(expr: &Expr) -> HashSet<BindingId> {
    let Expr::FuncLit {
        params,
        implicit_bindings,
        body,
        ..
    } = expr
    else {
        return HashSet::new();
    };
    let mut locals: HashSet<BindingId> = params.iter().map(|param| param.binding_id).collect();
    locals.extend(implicit_bindings.iter().copied());
    let mut stack = Vec::new();
    push_validation_body(&mut stack, body);
    while let Some(node) = stack.pop() {
        match node {
            HirValidationNode::Expr(child) => match child {
                // Nested functions own their own locals and are deliberately not descended here.
                Expr::FuncLit { .. } => {}
                Expr::Match { subject, arms, .. } => {
                    locals.extend(arms.iter().filter_map(|arm| arm.binding_id));
                    push_match_children(&mut stack, subject, arms);
                }
                _ => push_expr_children(&mut stack, child),
            },
            HirValidationNode::Stmt(stmt) => {
                match stmt {
                    Stmt::Binding(binding) => {
                        locals.insert(binding.binding_id);
                    }
                    Stmt::For { binding_id, .. } => {
                        locals.insert(*binding_id);
                    }
                    _ => {}
                }
                push_stmt_children(&mut stack, stmt);
            }
        }
    }
    locals
}

fn direct_resolved_callee_name(callee: &Expr) -> AliasResult<&str> {
    match callee {
        Expr::Ident(name, None, ..) => Ok(name.as_str()),
        Expr::Ident(_, Some(_), ..) => Err(invariant(
            callee.span(),
            "resolved builtin/constructor callee 不应携带 BindingId",
        )),
        _ => Err(invariant(
            callee.span(),
            "resolved builtin/constructor callee 非直接名字",
        )),
    }
}

fn builtin_target_matches_name(name: &str, builtin: &BuiltinCall) -> bool {
    match classify_call_builtin(name) {
        Some(CallBuiltinName::Print) => builtin == &BuiltinCall::Print,
        Some(CallBuiltinName::Println) => builtin == &BuiltinCall::Println,
        Some(CallBuiltinName::Increase) => builtin == &BuiltinCall::Increase,
        Some(CallBuiltinName::Decrease) => builtin == &BuiltinCall::Decrease,
        Some(CallBuiltinName::From | CallBuiltinName::TryFrom | CallBuiltinName::Typeof) | None => {
            false
        }
    }
}

fn validate_call_target(
    expr: &Expr,
    callee: &Expr,
    args: &[CallArg],
    target: &CallTarget,
    structs: &HashMap<String, StructContract>,
) -> AliasResult<()> {
    match target {
        CallTarget::FunctionValue => {
            let Ty::Func { params, ret } = callee.ty() else {
                return Err(invariant(
                    callee.span(),
                    "FunctionValue target 的 callee 不是完整函数类型",
                ));
            };
            if params.len() != args.len() {
                return Err(invariant(
                    expr.span(),
                    "FunctionValue target 的实参数量与签名不一致",
                ));
            }
            for (arg, param) in args.iter().zip(params) {
                if !types_match(param, arg.value.ty()) {
                    return Err(invariant(
                        arg.value.span(),
                        "FunctionValue target 的实参类型与签名不一致",
                    ));
                }
            }
            if !types_match(ret, expr.ty()) {
                return Err(invariant(
                    expr.span(),
                    "FunctionValue target 的结果类型与签名不一致",
                ));
            }
        }
        CallTarget::StructConstructor {
            name,
            arg_field_indices,
        } => {
            let callee_name = direct_resolved_callee_name(callee)?;
            if callee_name != name {
                return Err(invariant(
                    callee.span(),
                    "结构体构造 callee 名与 resolved target 不一致",
                ));
            }
            if !matches!(expr.ty(), Ty::Struct(actual) if actual == name) {
                return Err(invariant(
                    expr.span(),
                    "结构体构造 target 与表达式结果类型不一致",
                ));
            }
            let Some(contract) = structs.get(name) else {
                return Err(invariant(
                    expr.span(),
                    format!("结构体构造 target 引用未知结构体 '{name}'"),
                ));
            };
            if arg_field_indices.len() != args.len() {
                return Err(invariant(expr.span(), "构造器实参与字段索引数量不一致"));
            }
            let mut seen = HashSet::new();
            for (arg, index) in args.iter().zip(arg_field_indices) {
                let Some(field) = contract.fields.get(*index) else {
                    return Err(invariant(expr.span(), "构造器字段索引越界"));
                };
                if !seen.insert(*index) {
                    return Err(invariant(expr.span(), "构造器字段索引重复"));
                }
                if !types_match(&field.ty, arg.value.ty()) {
                    return Err(invariant(
                        arg.value.span(),
                        "构造器实参类型与已解析字段类型不一致",
                    ));
                }
            }
            if contract
                .fields
                .iter()
                .enumerate()
                .any(|(index, field)| !field.has_default && !seen.contains(&index))
            {
                return Err(invariant(expr.span(), "结构体构造 target 遗漏无默认值字段"));
            }
        }
        CallTarget::ResultConstructor(kind) => {
            let callee_name = direct_resolved_callee_name(callee)?;
            if classify_result_constructor(callee_name) != Some(*kind) {
                return Err(invariant(
                    callee.span(),
                    "result 构造 callee 名与 resolved target 不一致",
                ));
            }
            let [arg] = args else {
                return Err(invariant(expr.span(), "result 构造 target 元数不是 1"));
            };
            let Ty::Result(ok, err) = expr.ty() else {
                return Err(invariant(
                    expr.span(),
                    "result 构造 target 的结果类型不是 result",
                ));
            };
            let payload = match kind {
                CtorKind::Ok => ok.as_ref(),
                CtorKind::Err => err.as_ref(),
            };
            if !types_match(payload, arg.value.ty()) {
                return Err(invariant(
                    arg.value.span(),
                    "result 构造 target 的载荷类型与结果类型不一致",
                ));
            }
        }
        CallTarget::Builtin(builtin) => {
            let callee_name = direct_resolved_callee_name(callee)?;
            if !builtin_target_matches_name(callee_name, builtin) {
                return Err(invariant(
                    callee.span(),
                    "内建调用 callee 名与 resolved target 不一致",
                ));
            }
            if args.len() != 1 || expr.ty() != &Ty::Unit {
                return Err(invariant(
                    expr.span(),
                    "内建调用 target 的元数或 unit 结果不一致",
                ));
            }
            if matches!(builtin, BuiltinCall::Increase | BuiltinCall::Decrease) {
                let [arg] = args else { unreachable!() };
                if !matches!(&arg.value, Expr::Ident(_, Some(_), ..))
                    || !arg.value.ty().is_numeric()
                {
                    return Err(invariant(
                        arg.value.span(),
                        "increase/decrease target 未指向已解析数值绑定",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_user_method_target(
    expr: &Expr,
    recv: &Expr,
    args: &[CallArg],
    receiver: &Ty,
    id: MethodId,
    user_methods: &HashMap<MethodId, UserMethodContract>,
) -> AliasResult<()> {
    let Some(contract) = user_methods.get(&id) else {
        return Err(invariant(
            expr.span(),
            format!("用户方法引用未知 MethodId {id:?}"),
        ));
    };
    if &contract.receiver != receiver || recv.ty() != receiver {
        return Err(invariant(
            expr.span(),
            format!("MethodId {id:?} 接收者与调用目标不一致"),
        ));
    }
    if contract.params.len() != args.len() {
        return Err(invariant(
            expr.span(),
            format!("MethodId {id:?} 实参数量与签名不一致"),
        ));
    }
    for (arg, param) in args.iter().zip(&contract.params) {
        if !types_match(param, arg.value.ty()) {
            return Err(invariant(
                arg.value.span(),
                format!("MethodId {id:?} 实参类型与签名不一致"),
            ));
        }
    }
    if !types_match(&contract.ret, expr.ty()) {
        return Err(invariant(
            expr.span(),
            format!("MethodId {id:?} 结果类型与签名不一致"),
        ));
    }
    Ok(())
}

fn validate_builtin_method_target(
    expr: &Expr,
    recv: &Expr,
    args: &[CallArg],
    target: &MethodTarget,
) -> AliasResult<()> {
    let Some(contract) = crate::sema::builtin_method_by_target(recv.ty(), target) else {
        return Err(invariant(
            expr.span(),
            "内建方法 target 与接收者静态类型不一致",
        ));
    };
    if contract.params().len() != args.len() {
        return Err(invariant(
            expr.span(),
            "内建方法 target 的实参数量与契约不一致",
        ));
    }
    for (arg, param) in args.iter().zip(contract.params()) {
        if !types_match(param, arg.value.ty()) {
            return Err(invariant(
                arg.value.span(),
                "内建方法 target 的实参类型与契约不一致",
            ));
        }
    }
    if !types_match(contract.ret(), expr.ty()) {
        return Err(invariant(
            expr.span(),
            "内建方法 target 的结果类型与契约不一致",
        ));
    }
    Ok(())
}

fn validate_funclit_contract(
    expr: &Expr,
    params: &[super::Param],
    implicit_bindings: &[BindingId],
) -> AliasResult<()> {
    let Ty::Func {
        params: signature_params,
        ..
    } = expr.ty()
    else {
        return Err(invariant(expr.span(), "FuncLit 缺少完整函数类型"));
    };
    if signature_params.len() != implicit_bindings.len() + params.len() {
        return Err(invariant(
            expr.span(),
            "FuncLit 参数数量与完整函数类型不一致",
        ));
    }
    let explicit_start = signature_params.len() - params.len();
    for (param, signature_ty) in params.iter().zip(&signature_params[explicit_start..]) {
        if !types_match(&param.ty, signature_ty) {
            return Err(invariant(
                expr.span(),
                "FuncLit 显式参数类型与完整函数类型不一致",
            ));
        }
    }
    Ok(())
}

fn resolved_field_ty(
    recv: &Expr,
    field_index: usize,
    structs: &HashMap<String, StructContract>,
    span: Span,
) -> AliasResult<Ty> {
    let Ty::Struct(name) = recv.ty() else {
        return Err(invariant(span, "字段索引的接收者不是 struct"));
    };
    let Some(contract) = structs.get(name) else {
        return Err(invariant(span, format!("字段索引引用未知结构体 '{name}'")));
    };
    let Some(field) = contract.fields.get(field_index) else {
        return Err(invariant(span, "已解析字段索引越界"));
    };
    Ok(field.ty.clone())
}

pub(super) fn validate_resolved_hir(program: &CheckedProgram) -> AliasResult<()> {
    // The source is untrusted and nesting is bounded but nontrivial; validation uses explicit
    // stacks so the final authority gate itself does not reintroduce host-recursion risk.
    // This pass is also the last cross-reference gate before codegen: resolved IDs, call/method
    // targets and struct field contracts are checked against declarations here so the backend
    // never has to rediscover or repair them.
    let known_ids = collect_declared_ids(program);
    let structs = collect_struct_contracts(program)?;
    let user_methods = collect_user_methods(program)?;
    validate_main_contract(program)?;
    if !known_ids.contains(&program.main_id) {
        return Err(invariant(Span::default(), "main BindingId 不存在"));
    }
    let globals: HashSet<BindingId> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Binding(binding) if !binding.is_method() => Some(binding.binding_id),
            _ => None,
        })
        .collect();

    let mut stack = Vec::new();
    for item in program.items.iter().rev() {
        match item {
            Item::Binding(binding) => {
                validate_binding_contract(binding)?;
                stack.push(HirValidationNode::Expr(&binding.value));
            }
            Item::StructDef(def) => {
                for field in def.fields.iter().rev() {
                    if field.ty.contains_unknown() {
                        return Err(invariant(field.span, "字段类型未确定"));
                    }
                    if let Some(default) = &field.default {
                        if !types_match(&field.ty, default.ty()) {
                            return Err(invariant(
                                default.span(),
                                "结构体字段默认值类型与声明类型不一致",
                            ));
                        }
                        stack.push(HirValidationNode::Expr(default));
                    }
                }
            }
        }
    }

    while let Some(node) = stack.pop() {
        match node {
            HirValidationNode::Expr(expr) => {
                if expr.ty().contains_unknown() {
                    return Err(invariant(
                        expr.span(),
                        format!("HIR 仍含未确定类型 {}", expr.ty().name()),
                    ));
                }
                match expr {
                    Expr::Ident(_, Some(id), ..) => {
                        if !known_ids.contains(id) {
                            return Err(invariant(
                                expr.span(),
                                format!("Ident 引用未知 BindingId {id:?}"),
                            ));
                        }
                    }
                    Expr::Ident(name, None, ..) => {
                        return Err(invariant(
                            expr.span(),
                            format!("可求值标识符 '{name}' 缺少 BindingId"),
                        ));
                    }
                    Expr::Typeof { type_name, .. } => {
                        if expr.ty() != &Ty::Str || type_name.is_empty() || type_name == "未知" {
                            return Err(invariant(
                                expr.span(),
                                "typeof 未固化有效的 string 类型名",
                            ));
                        }
                    }
                    Expr::Convert {
                        expr: inner,
                        mode: super::ResolvedConversion::Identity,
                        ..
                    } if inner.ty() != expr.ty() => {
                        return Err(invariant(expr.span(), "Identity 转换改变了静态类型"));
                    }
                    Expr::Call {
                        callee,
                        args,
                        target,
                        ..
                    } => {
                        validate_call_target(expr, callee, args, target, &structs)?;
                        if matches!(target, CallTarget::FunctionValue) {
                            stack.push(HirValidationNode::Expr(callee));
                        }
                        for arg in args.iter().rev() {
                            stack.push(HirValidationNode::Expr(&arg.value));
                        }
                    }
                    Expr::FuncLit {
                        captures,
                        params,
                        implicit_bindings,
                        body,
                        ..
                    } => {
                        validate_funclit_contract(expr, params, implicit_bindings)?;
                        let locals = collect_function_locals(expr);
                        let mut seen = HashSet::new();
                        for id in captures {
                            if !seen.insert(*id) {
                                return Err(invariant(expr.span(), format!("capture 重复 {id:?}")));
                            }
                            if !known_ids.contains(id) {
                                return Err(invariant(
                                    expr.span(),
                                    format!("capture 引用未知 BindingId {id:?}"),
                                ));
                            }
                            if globals.contains(id) {
                                return Err(invariant(
                                    expr.span(),
                                    format!("全局 BindingId {id:?} 不应进入 capture"),
                                ));
                            }
                            if locals.contains(id) {
                                return Err(invariant(
                                    expr.span(),
                                    format!("函数自身 local {id:?} 不应进入 capture"),
                                ));
                            }
                        }
                        let mut declared_here = HashSet::new();
                        for param in params {
                            if param.ty.contains_unknown() {
                                return Err(invariant(expr.span(), "函数参数类型未确定"));
                            }
                            if !declared_here.insert(param.binding_id) {
                                return Err(invariant(
                                    expr.span(),
                                    format!("函数入口 BindingId 重复 {:?}", param.binding_id),
                                ));
                            }
                        }
                        for id in implicit_bindings {
                            if !declared_here.insert(*id) {
                                return Err(invariant(
                                    expr.span(),
                                    format!("函数入口 BindingId 重复 {id:?}"),
                                ));
                            }
                        }
                        push_validation_body(&mut stack, body);
                    }
                    Expr::Str(parts, ..) => {
                        for part in parts.iter().rev() {
                            if let StrPart::Hole(hole) = part {
                                stack.push(HirValidationNode::Expr(hole));
                            }
                        }
                    }
                    Expr::Cast { expr, .. }
                    | Expr::Convert { expr, .. }
                    | Expr::Neg { expr, .. }
                    | Expr::Not { expr, .. }
                    | Expr::BitNot { expr, .. }
                    | Expr::Propagate { expr, .. } => {
                        stack.push(HirValidationNode::Expr(expr));
                    }
                    Expr::Binary { lhs, rhs, .. } => {
                        stack.push(HirValidationNode::Expr(rhs));
                        stack.push(HirValidationNode::Expr(lhs));
                    }
                    Expr::Ternary {
                        cond,
                        then_expr,
                        else_expr,
                        ..
                    } => {
                        stack.push(HirValidationNode::Expr(else_expr));
                        stack.push(HirValidationNode::Expr(then_expr));
                        stack.push(HirValidationNode::Expr(cond));
                    }
                    Expr::MethodCall {
                        recv, args, target, ..
                    } => {
                        match target {
                            MethodTarget::User { receiver, id } => validate_user_method_target(
                                expr,
                                recv,
                                args,
                                receiver,
                                *id,
                                &user_methods,
                            )?,
                            _ => validate_builtin_method_target(expr, recv, args, target)?,
                        }
                        for arg in args.iter().rev() {
                            stack.push(HirValidationNode::Expr(&arg.value));
                        }
                        stack.push(HirValidationNode::Expr(recv));
                    }
                    Expr::Field {
                        recv, field_index, ..
                    } => {
                        let field_ty =
                            resolved_field_ty(recv, *field_index, &structs, expr.span())?;
                        if !types_match(&field_ty, expr.ty()) {
                            return Err(invariant(
                                expr.span(),
                                "字段表达式类型与已解析字段声明不一致",
                            ));
                        }
                        stack.push(HirValidationNode::Expr(recv));
                    }
                    Expr::Index { recv, idx, .. } => {
                        stack.push(HirValidationNode::Expr(idx));
                        stack.push(HirValidationNode::Expr(recv));
                    }
                    Expr::ArrayLit { elems, .. } => {
                        for elem in elems.iter().rev() {
                            stack.push(HirValidationNode::Expr(elem));
                        }
                    }
                    Expr::Match { subject, arms, .. } => {
                        for arm in arms.iter().rev() {
                            if let Some(id) = arm.binding_id {
                                if !known_ids.contains(&id) {
                                    return Err(invariant(
                                        arm.pattern.span(),
                                        format!("Pattern 引用未知 BindingId {id:?}"),
                                    ));
                                }
                            }
                            match &arm.body {
                                ArmBody::Block(stmts) => {
                                    for stmt in stmts.iter().rev() {
                                        stack.push(HirValidationNode::Stmt(stmt));
                                    }
                                }
                                ArmBody::Value(value) | ArmBody::Ret(value) => {
                                    stack.push(HirValidationNode::Expr(value));
                                }
                            }
                        }
                        stack.push(HirValidationNode::Expr(subject));
                    }
                    Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) | Expr::This(..) => {}
                }
            }
            HirValidationNode::Stmt(stmt) => match stmt {
                Stmt::Binding(binding) => {
                    validate_binding_contract(binding)?;
                    stack.push(HirValidationNode::Expr(&binding.value));
                }
                Stmt::Assign { target_id, value } => {
                    if !known_ids.contains(target_id) {
                        return Err(invariant(
                            value.span(),
                            format!("Assign 引用未知 BindingId {target_id:?}"),
                        ));
                    }
                    stack.push(HirValidationNode::Expr(value));
                }
                Stmt::FieldAssign {
                    recv,
                    field_index,
                    value,
                } => {
                    let field_ty = resolved_field_ty(recv, *field_index, &structs, value.span())?;
                    if !types_match(&field_ty, value.ty()) {
                        return Err(invariant(
                            value.span(),
                            "字段赋值 RHS 类型与已解析字段声明不一致",
                        ));
                    }
                    stack.push(HirValidationNode::Expr(value));
                    stack.push(HirValidationNode::Expr(recv));
                }
                Stmt::Expr { expr } => stack.push(HirValidationNode::Expr(expr)),
                Stmt::Return { value } => {
                    if let Some(value) = value {
                        stack.push(HirValidationNode::Expr(value));
                    }
                }
                Stmt::If {
                    branches,
                    else_body,
                } => {
                    if let Some(body) = else_body {
                        for stmt in body.iter().rev() {
                            stack.push(HirValidationNode::Stmt(stmt));
                        }
                    }
                    for (cond, body) in branches.iter().rev() {
                        for stmt in body.iter().rev() {
                            stack.push(HirValidationNode::Stmt(stmt));
                        }
                        stack.push(HirValidationNode::Expr(cond));
                    }
                }
                Stmt::While { cond, body } => {
                    for stmt in body.iter().rev() {
                        stack.push(HirValidationNode::Stmt(stmt));
                    }
                    stack.push(HirValidationNode::Expr(cond));
                }
                Stmt::For {
                    binding_id,
                    ty,
                    iterable,
                    body,
                    span,
                } => {
                    if !known_ids.contains(binding_id) || ty.contains_unknown() {
                        return Err(invariant(*span, "for BindingId/类型未解析"));
                    }
                    for stmt in body.iter().rev() {
                        stack.push(HirValidationNode::Stmt(stmt));
                    }
                    stack.push(HirValidationNode::Expr(iterable));
                }
                Stmt::Break | Stmt::Continue => {}
            },
        }
    }
    Ok(())
}
