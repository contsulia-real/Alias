use super::*;
pub(crate) fn vty_of_name<M: Module>(c: &Compiler<M>, frame: &Frame, name: &str) -> VTy {
    for (scope, vtys) in frame.scopes.iter().zip(frame.locals_vty.iter()) {
        if scope.contains_key(name) {
            return vtys.get(name).cloned().unwrap_or(VTy::Other);
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

/// 打印分派所需的最小静态类型投影 (推断不外泄 — 仅后端内部消费)。
pub(crate) fn static_vty<M: Module>(c: &Compiler<M>, frame: &Frame, e: &Expr) -> VTy {
    match e {
        Expr::Int(..) | Expr::Neg { .. } => VTy::Int,
        Expr::Bool(..) => VTy::Bool,
        Expr::Unit(_) => VTy::Unit,
        Expr::Str(..) => VTy::Str,
        Expr::FuncLit { .. } => VTy::Func,
        Expr::Binary { op, lhs: _, .. } => match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => VTy::Int,
            _ => VTy::Bool,
        },
        Expr::Ident(name, _) => vty_of_name(c, frame, name),
        // 字段链: recv 的结构体名 → 布局表 → 字段静态类型 (嵌套可递归)
        Expr::Field { recv, name, .. } => {
            if let VTy::Struct(s) = static_vty(c, frame, recv) {
                if let Some(layout) = c.struct_layouts.get(&s) {
                    if let Some((_, _, fvty)) =
                        layout.iter().find(|(n, ..)| n == name)
                    {
                        return fvty.clone();
                    }
                }
            }
            VTy::Other
        }
        Expr::Call { callee, .. } => match callee.as_ref() {
            Expr::Ident(name, _) => c
                .fn_ret_by_name
                .get(name)
                .cloned()
                // 结构体构造调用的结果即实例 (打印分派/字段偏移回查所需)
                .or_else(|| {
                    c.struct_layouts
                        .contains_key(name)
                        .then(|| VTy::Struct(name.clone()))
                })
                .unwrap_or(VTy::Other),
            Expr::FuncLit { .. } => VTy::Other,
            _ => VTy::Other,
        },
        // expr? 的静态类型 = 主语 T 侧反解 (打印 f(x)? 所需)
        Expr::Propagate { expr, .. } => match static_vty(c, frame, expr) {
            VTy::Result(t, _) => vty_of_type_name(&c.struct_layouts, &t),
            _ => VTy::Other,
        },
        // match 表达式的臂绑定不在当前帧 — 静态投影保守回退 Other
        // (直接打印 match 值被拒; 经声明绑定中转后打印不受影响)
        Expr::Match { .. } => VTy::Other,
        // 方法调用 (Phase 2c): 用户方法回查发射期返回类型表;
        // 内建字符串方法按冻结签名投影 — 链式调用由此逐级流动
        Expr::MethodCall { recv, name, .. } => {
            let rn = match static_vty(c, frame, recv) {
                VTy::Str => "string".to_string(),
                VTy::Int => "i32".to_string(),
                VTy::Bool => "bool".to_string(),
                VTy::Struct(s) => s,
                _ => return VTy::Other,
            };
            if let Some(v) = c.method_rets.get(&(rn.clone(), name.clone())) {
                return v.clone();
            }
            match (rn.as_str(), name.as_str()) {
                ("string", "len") => VTy::Int,
                ("string", "upper" | "lower" | "trim") => VTy::Str,
                _ => VTy::Other,
            }
        }
        _ => VTy::Other,
    }
}

/// 函数字面量返回类型的保守推断 (箭头体=体类型; 块体=末条 return 类型;
/// 其余落空=Unit)。仅用于打印分派与调用结果宽度 — 不外泄诊断。
pub(crate) fn infer_ret_vty<M: Module>(c: &Compiler<M>, frame: &Frame, params: &[Param], body: &Body) -> VTy {
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
        terminated: false,
        init_ctx: true,
        ret_block: None,
    };
    match body {
        Body::ArrowExpr(e) => static_vty(c, &inner, e),
        Body::Block(stmts) => match stmts.last() {
            Some(Stmt::Return { value, .. }) => match value {
                Some(e) => static_vty(c, &inner, e),
                None => VTy::Unit,
            },
            _ => VTy::Unit,
        },
    }
}

