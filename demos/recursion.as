// ============================================================
// demos/recursion.as
// 试运行范围: this 自引用递归 / match 字面量分支
//
// 【已定稿文法】 this = 当前 func 绑定自身 (宪法 v0.2) /
//   match 值分支: 字面量模式 + _ 通配符, match 为表达式 (P7 已裁决)
//
// 【临时提案 · 待裁决】 本文件暂无
// ============================================================

import { print } from 'io'

func i32 fact = (i32 n) -> return match n {
    0 -> 1                         // match 是表达式, 分支直接产出值
    _ -> n * this(n - 1)           // this 即 fact; 改名不断链
}                                  // 匿名函数亦可借此递归

func i32 main = () -> {
    print('5! = ${fact(5)}')       // 此处若写 this(5) 指的是 main
    return 0                       // 自己 — this 永远指"当前 func"
}
