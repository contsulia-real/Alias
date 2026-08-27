use super::*;

pub(crate) fn vty_of_name<M: Module>(c: &Compiler<M>, frame: &Frame, name: &str) -> VTy {
    for vtys in frame.locals_vty.iter().rev() {
        if let Some(vty) = vtys.get(name) {
            return vty.clone();
        }
    }
    if let Some(v) = frame.caps_vty.get(name) {
        return v.clone();
    }
    c.globals_final
        .get(name)
        .map(|(_, v)| v.clone())
        .unwrap_or(VTy::Other)
}

pub(crate) fn static_vty<M: Module>(c: &Compiler<M>, frame: &Frame, e: &Expr) -> VTy {
    match e {
        Expr::Int(value, ..) => default_positive_int_vty(*value),
        Expr::Float(..) => VTy::F(FloatW::F64),
        Expr::Neg { expr, .. } => {
            if let Expr::Int(magnitude, _) = expr.as_ref() {
                default_negative_int_vty(*magnitude).unwrap_or(VTy::Other)
            } else {
                match static_vty(c, frame, expr) {
                    v @ (VTy::I(_) | VTy::F(_)) => v,
                    _ => VTy::Other,
                }
            }
        }
        Expr::Not { .. } => VTy::Bool,
        Expr::BitNot { expr, .. } => match static_vty(c, frame, expr) {
            v @ (VTy::I(_) | VTy::U(_)) => v,
            _ => VTy::Other,
        },
        Expr::Ternary {
            then_expr,
            else_expr,
            ..
        } => {
            let a = static_vty(c, frame, then_expr);
            if a != VTy::Other {
                a
            } else {
                static_vty(c, frame, else_expr)
            }
        }
        Expr::Bool(..) => VTy::Bool,
        Expr::Unit(_) => VTy::Unit,
        Expr::Str(..) => VTy::Str,
        Expr::FuncLit { params, body, .. } => VTy::Func(
            params
                .iter()
                .map(|p| decl_vty(&p.ty, &c.struct_layouts))
                .collect(),
            Box::new(infer_ret_vty(c, frame, params, body)),
        ),
        Expr::Binary { op, lhs, rhs, .. } => match op {
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Rem
            | BinOp::Shl
            | BinOp::Shr
            | BinOp::BitAnd
            | BinOp::BitXor
            | BinOp::BitOr => {
                let lt = static_vty(c, frame, lhs);
                if lt.is_numeric() {
                    lt
                } else {
                    let rt = static_vty(c, frame, rhs);
                    if rt.is_numeric() {
                        rt
                    } else {
                        VTy::Other
                    }
                }
            }
            _ => VTy::Bool,
        },
        Expr::Ident(name, _) => vty_of_name(c, frame, name),
        Expr::This(_) => frame.this_vty.clone().unwrap_or(VTy::Other),
        Expr::Cast { target, .. } => decl_vty(target, &c.struct_layouts),
        Expr::ArrayLit { elems, .. } => VTy::Array(Box::new(
            elems
                .first()
                .map(|e| static_vty(c, frame, e))
                .unwrap_or(VTy::Other),
        )),
        Expr::Index { recv, .. } => match static_vty(c, frame, recv) {
            VTy::Array(inner) => *inner,
            _ => VTy::Other,
        },
        Expr::Field { recv, name, .. } => {
            if let VTy::Struct(s) = static_vty(c, frame, recv) {
                if let Some(layout) = c.struct_layouts.get(&s) {
                    if let Some(field) = layout.fields.iter().find(|field| field.name == *name) {
                        return field.vty.clone();
                    }
                }
            }
            VTy::Other
        }
        Expr::Call { callee, .. } => match callee.as_ref() {
            Expr::Ident(name, _) => {
                if name == "typeof" {
                    return VTy::Str;
                }
                match vty_of_name(c, frame, name) {
                    VTy::Func(_, ret) => Some(*ret),
                    _ => None,
                }
                .or_else(|| {
                    c.struct_layouts
                        .contains_key(name)
                        .then(|| VTy::Struct(name.clone()))
                })
                .unwrap_or(VTy::Other)
            }
            Expr::FuncLit { params, body, .. } => infer_ret_vty(c, frame, params, body),
            other => match static_vty(c, frame, other) {
                VTy::Func(_, ret) => *ret,
                _ => VTy::Other,
            },
        },
        Expr::Propagate { expr, .. } => match static_vty(c, frame, expr) {
            VTy::Result(t, _) => vty_of_type_name(&c.struct_layouts, &t),
            _ => VTy::Other,
        },
        Expr::Match { subject, arms, .. } => {
            let subject_vty = static_vty(c, frame, subject);
            let payloads = match &subject_vty {
                VTy::Result(ok, err) => Some((
                    vty_of_type_name(&c.struct_layouts, ok),
                    vty_of_type_name(&c.struct_layouts, err),
                )),
                _ => None,
            };
            arms.iter()
                .find_map(|arm| {
                    let value = match &arm.body {
                        ArmBody::Value(e) => Some(e.as_ref()),
                        ArmBody::Block(stmts) => match stmts.last() {
                            Some(Stmt::ExprStmt { expr, .. }) => Some(expr),
                            _ => None,
                        },
                        ArmBody::Ret(_) => None,
                    }?;
                    let mut arm_frame = frame.clone();
                    arm_frame.scopes.push(HashMap::new());
                    let mut types = HashMap::new();
                    match &arm.pattern {
                        Pattern::Binding { name, .. } => {
                            types.insert(name.clone(), subject_vty.clone());
                        }
                        Pattern::Constructor {
                            ctor,
                            binding: Some(name),
                            ..
                        } => {
                            if let Some((ok, err)) = &payloads {
                                types.insert(
                                    name.clone(),
                                    match ctor {
                                        CtorKind::Ok => ok.clone(),
                                        CtorKind::Err => err.clone(),
                                    },
                                );
                            }
                        }
                        _ => {}
                    }
                    arm_frame.locals_vty.push(types);
                    Some(static_vty(c, &arm_frame, value))
                })
                .unwrap_or(VTy::Other)
        }
        Expr::MethodCall { recv, name, .. } => {
            let rvty = static_vty(c, frame, recv);
            if rvty.is_numeric() && matches!(name.as_str(), "plus" | "minus" | "times" | "div") {
                return rvty;
            }
            if rvty == VTy::Bool && name == "not" {
                return VTy::Bool;
            }
            if let VTy::Array(elem) = &rvty {
                match name.as_str() {
                    "len" => return VTy::I(IntW::W32),
                    "push" => return VTy::Unit,
                    "pop" => return (**elem).clone(),
                    "iterator" => return VTy::Iterator(elem.clone()),
                    _ => {}
                }
            }
            let rn = match rvty {
                VTy::Other | VTy::Unit => return VTy::Other,
                ref other => other.display_name(),
            };
            if let Some(v) = c.method_rets.get(&(rn.clone(), name.clone())) {
                return v.clone();
            }
            match (rn.as_str(), name.as_str()) {
                ("string", "len") => VTy::I(IntW::W32),
                ("string", "upper" | "lower" | "trim") => VTy::Str,
                _ => VTy::Other,
            }
        }
    }
}

fn default_positive_int_vty(value: u64) -> VTy {
    if value <= i32::MAX as u64 {
        VTy::I(IntW::W32)
    } else if value <= i64::MAX as u64 {
        VTy::I(IntW::W64)
    } else {
        VTy::U(UIntW::U64)
    }
}

fn default_negative_int_vty(magnitude: u64) -> Option<VTy> {
    if magnitude <= (i32::MAX as u64) + 1 {
        Some(VTy::I(IntW::W32))
    } else if magnitude <= (i64::MAX as u64) + 1 {
        Some(VTy::I(IntW::W64))
    } else {
        None
    }
}

pub(crate) fn infer_ret_vty<M: Module>(
    c: &Compiler<M>,
    frame: &Frame,
    params: &[Param],
    body: &Body,
) -> VTy {
    let mut scoped = frame.scopes.clone();
    let mut vtys = frame.locals_vty.clone();
    let mut pmap: HashMap<String, Slot> = HashMap::new();
    let mut pvty: HashMap<String, VTy> = HashMap::new();
    for p in params {
        let v = Variable::from_u32(9_999);
        pmap.insert(p.name.clone(), Slot::Local(v));
        pvty.insert(p.name.clone(), decl_vty(&p.ty, &c.struct_layouts));
    }
    scoped.push(pmap);
    vtys.push(pvty);
    let inner = Frame {
        scopes: scoped,
        locals_vty: vtys,
        globals: frame.globals,
        env: frame.env,
        caps: frame.caps.clone(),
        caps_vty: frame.caps_vty.clone(),
        this_fid: frame.this_fid,
        this_vty: frame.this_vty.clone(),
        ret_block: frame.ret_block,
        ret_vty: frame.ret_vty.clone(),
        terminated: false,
        loop_targets: Vec::new(),
        init_ctx: true,
    };
    match body {
        Body::Single(stmt) => ret_from_stmt(c, &inner, stmt).unwrap_or(VTy::Unit),
        Body::Block(stmts) => stmts
            .iter()
            .find_map(|s| ret_from_stmt(c, &inner, s))
            .unwrap_or(VTy::Unit),
    }
}

fn ret_from_stmt<M: Module>(c: &Compiler<M>, frame: &Frame, stmt: &Stmt) -> Option<VTy> {
    match stmt {
        Stmt::Return { value: Some(e), .. } => Some(static_vty(c, frame, e)),
        Stmt::Return { value: None, .. } => Some(VTy::Unit),
        Stmt::If {
            branches,
            else_body,
            ..
        } => branches
            .iter()
            .flat_map(|(_, body)| body.iter())
            .chain(else_body.iter().flat_map(|body| body.iter()))
            .find_map(|s| ret_from_stmt(c, frame, s)),
        _ => None,
    }
}
