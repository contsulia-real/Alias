//! JIT 宿主运行时支持库 — 进程内注册的 extern "C" 实现。
//! AOT 侧由 aot_shim.rs 以相同符号契约实现 (spec-notes §五)。
use crate::codegen::SPAN_TABLE;
use cranelift_jit::JITBuilder;

extern "C" fn alias_cell_new(init: i64) -> i64 {
    Box::into_raw(Box::new(init)) as i64
}

extern "C" fn alias_env_new(count: i32) -> i64 {
    let slots = vec![0i64; count.max(0) as usize].into_boxed_slice();
    Box::leak(slots).as_mut_ptr() as i64
}

extern "C" fn alias_globals_new(count: i32) -> i64 {
    let slots = vec![0i64; count.max(0) as usize].into_boxed_slice();
    Box::leak(slots).as_mut_ptr() as i64
}

/// 闭包对象 {code, env} — 泄漏, 与进程同寿命。
extern "C" fn alias_closure_new(code: i64, env: i64) -> i64 {
    let pair = vec![code, env].into_boxed_slice();
    Box::leak(pair).as_mut_ptr() as i64
}

unsafe fn str_parts(blk: i64) -> (*const u8, usize) {
    let base = blk as *const u8;
    let p = base.cast::<u64>().read_unaligned() as *const u8;
    let l = base.add(8).cast::<u64>().read_unaligned() as usize;
    (p, l)
}

fn alloc_bytes(n: usize) -> *mut u8 {
    vec![0u8; n].leak().as_mut_ptr()
}

/// 块构造: {data_ptr, len} 两字。调用方保证 data 寿命为进程级 (泄漏)。
unsafe fn make_block(data: u64, len: u64) -> *mut u8 {
    let blk = alloc_bytes(16);
    blk.cast::<u64>().write_unaligned(data);
    blk.add(8).cast::<u64>().write_unaligned(len);
    blk
}

/// 字符串块构造: 复制字节 (数据段来源亦复制 — 统一所有权, 免生命周期论证)。
extern "C" fn alias_str_new(bytes: *const u8, len: i32) -> i64 {
    unsafe {
        if bytes.is_null() || len <= 0 {
            make_block(0, 0) as i64
        } else {
            let buf = alloc_bytes(len as usize);
            std::ptr::copy_nonoverlapping(bytes, buf, len as usize);
            make_block(buf as u64, len.max(0) as u64) as i64
        }
    }
}

extern "C" fn alias_str_concat(a: i64, b: i64) -> i64 {
    unsafe {
        let (pa, la) = str_parts(a);
        let (pb, lb) = str_parts(b);
        let total = la + lb;
        let out = alloc_bytes(total);
        std::ptr::copy_nonoverlapping(pa, out, la);
        std::ptr::copy_nonoverlapping(pb, out.add(la), lb);
        make_block(out as u64, total as u64) as i64
    }
}

/// 字典序字节比较: -1 / 0 / 1 (compare_str 语义, spec-notes 冻结)。
extern "C" fn alias_str_cmp(a: i64, b: i64) -> i32 {
    unsafe {
        let (pa, la) = str_parts(a);
        let (pb, lb) = str_parts(b);
        match std::slice::from_raw_parts(pa, la).cmp(std::slice::from_raw_parts(pb, lb)) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }
}

extern "C" fn alias_display_int(v: i32) -> i64 {
    let mut bytes = v.to_string().into_bytes().into_boxed_slice();
    let ptr = bytes.as_mut_ptr();
    let len = bytes.len();
    std::mem::forget(bytes);
    unsafe { make_block(ptr as u64, len as u64) as i64 }
}

extern "C" fn alias_display_bool(v: i32) -> i64 {
    static TRUE_: &[u8] = b"true";
    static FALSE_: &[u8] = b"false";
    let b = if v != 0 { TRUE_ } else { FALSE_ };
    unsafe { make_block(b.as_ptr() as u64, b.len() as u64) as i64 }
}

extern "C" fn alias_display_str(s: i64) -> i64 {
    s // Value::display: Str 原样 — 块即显示结果 (泄漏模型下共享安全)
}

extern "C" fn alias_display_unit() -> i64 {
    static UNIT: &[u8] = b"()";
    unsafe { make_block(UNIT.as_ptr() as u64, UNIT.len() as u64) as i64 }
}

extern "C" fn alias_display_func() -> i64 {
    static FUNC: &[u8] = b"<func>";
    unsafe { make_block(FUNC.as_ptr() as u64, FUNC.len() as u64) as i64 }
}

/// struct 显示 (Phase 2a): 固定 "<struct>" — 结构体值永不泄露内部布局。
extern "C" fn alias_display_struct() -> i64 {
    static STRUCT: &[u8] = b"<struct>";
    unsafe { make_block(STRUCT.as_ptr() as u64, STRUCT.len() as u64) as i64 }
}

/// result 显示 (Phase 2b): 入参为运行时 tag (0=ok 非0=err), 输出 "<ok>"/"<err>" 块。
extern "C" fn alias_display_result(tag: i32) -> i64 {
    static OK: &[u8] = b"<ok>";
    static ERR: &[u8] = b"<err>";
    let b = if tag == 0 { OK } else { ERR };
    unsafe { make_block(b.as_ptr() as u64, b.len() as u64) as i64 }
}

unsafe fn write_stdout_block(s: i64, newline: bool) {
    use std::io::Write;
    let (p, l) = str_parts(s);
    let out = std::io::stdout();
    let mut lock = out.lock();
    if l > 0 {
        let _ = lock.write_all(std::slice::from_raw_parts(p, l));
    }
    if newline {
        let _ = lock.write_all(b"\n");
    }
    let _ = lock.flush();
}

extern "C" fn alias_println_str(s: i64) {
    unsafe { write_stdout_block(s, true) }
}

extern "C" fn alias_print_str(s: i64) {
    unsafe { write_stdout_block(s, false) }
}

extern "C" fn alias_println_i32(v: i32) {
    println!("{v}");
}

extern "C" fn alias_println_bool(v: i32) {
    println!("{}", v != 0);
}

extern "C" fn alias_print_i32(v: i32) {
    use std::io::Write;
    print!("{v}");
    let _ = std::io::stdout().flush();
}

extern "C" fn alias_print_bool(v: i32) {
    use std::io::Write;
    print!("{}", v != 0);
    let _ = std::io::stdout().flush();
}

/// 除零/INT_MIN÷-1 中止: 按 span-ID 打印原始行:列, 退出码 1 (对齐黄金记录)。
extern "C" fn alias_abort_div(span_id: i32) {
    let table = SPAN_TABLE.lock().expect("SPAN_TABLE 锁中毒");
    let (line, col) = table.get(span_id.max(0) as usize).copied().unwrap_or((0, 0));
    drop(table);
    eprintln!("错误 @ {}:{} — 除以零", line, col);
    std::process::exit(1);
}

// ---- 内建 string 方法宿主实现 (P2c; AOT 侧 aot_shim.rs 同契约 IR shim) ----

extern "C" fn alias_str_len(s: i64) -> i32 {
    unsafe {
        let (_, l) = str_parts(s);
        l as i32
    }
}

/// ASCII 范围大小写转换核心: 命中范围加 delta, 否则原样
unsafe fn map_ascii(s: i64, lo: u8, hi: u8, delta: i32) -> i64 {
    let (p, l) = str_parts(s);
    let out = alloc_bytes(l);
    for i in 0..l {
        let mut b = *p.add(i);
        if (lo..=hi).contains(&b) {
            b = (b as i32 + delta) as u8;
        }
        *out.add(i) = b;
    }
    make_block(out as u64, l as u64) as i64
}

extern "C" fn alias_str_upper(s: i64) -> i64 {
    unsafe { map_ascii(s, b'a', b'z', -32) }
}

extern "C" fn alias_str_lower(s: i64) -> i64 {
    unsafe { map_ascii(s, b'A', b'Z', 32) }
}

extern "C" fn alias_str_trim(s: i64) -> i64 {
    unsafe {
        let (p, l) = str_parts(s);
        let is_ws = |b: u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n');
        let mut start = 0usize;
        while start < l && is_ws(*p.add(start)) {
            start += 1;
        }
        let mut end = l;
        while end > start && is_ws(*p.add(end - 1)) {
            end -= 1;
        }
        let n = end - start;
        let buf = alloc_bytes(n);
        std::ptr::copy_nonoverlapping(p.add(start), buf, n);
        make_block(buf as u64, n as u64) as i64
    }
}

pub(crate) fn register_host_fns(builder: &mut JITBuilder) {
    builder.symbol("alias.cell.new", alias_cell_new as *const u8);
    builder.symbol("alias.env.new", alias_env_new as *const u8);
    builder.symbol("alias.globals.new", alias_globals_new as *const u8);
    builder.symbol("alias.closure.new", alias_closure_new as *const u8);
    builder.symbol("alias.str.new", alias_str_new as *const u8);
    builder.symbol("alias.str.concat", alias_str_concat as *const u8);
    builder.symbol("alias.str.cmp", alias_str_cmp as *const u8);
    builder.symbol("alias.display.int", alias_display_int as *const u8);
    builder.symbol("alias.display.bool", alias_display_bool as *const u8);
    builder.symbol("alias.display.str", alias_display_str as *const u8);
    builder.symbol("alias.display.unit", alias_display_unit as *const u8);
    builder.symbol("alias.display.func", alias_display_func as *const u8);
    builder.symbol("alias.println.str", alias_println_str as *const u8);
    builder.symbol("alias.print.str", alias_print_str as *const u8);
    builder.symbol("alias.println.i32", alias_println_i32 as *const u8);
    builder.symbol("alias.println.bool", alias_println_bool as *const u8);
    builder.symbol("alias.print.i32", alias_print_i32 as *const u8);
    builder.symbol("alias.print.bool", alias_print_bool as *const u8);
    builder.symbol("alias.abort_div", alias_abort_div as *const u8);
    builder.symbol("alias.str.len", alias_str_len as *const u8);
    builder.symbol("alias.str.upper", alias_str_upper as *const u8);
    builder.symbol("alias.str.lower", alias_str_lower as *const u8);
    builder.symbol("alias.str.trim", alias_str_trim as *const u8);
    builder.symbol("alias.display.result", alias_display_result as *const u8);
    builder.symbol("alias.display.struct", alias_display_struct as *const u8);
}