// ============================================================
// demos/producer_consumer.as
// 试运行范围: isolate / channel / RAII 关闭语义 / match-option
//
// 【已定稿文法】 绑定统一文法 (val|var|func 类型 名字 = 值) /
//   类型前置 / 类型槽强制非空 — 无类型推断, 每个绑定显式标注产出类型 /
//   赋值语句: 绑定名 = 表达式 (P1 已裁决) /
//   单引号 $ 插值 / match 分支 pattern -> expr /
//   for-while bool 条件循环 / increase 无括号调用 / 匿名函数字面量 (参数) -> 体 /
//   成员调用 receiver.method() — 由 helper.as 推定成立;
//   其"调用即借用"的记账规则属运行时检测设计阶段, 非文法议题
//
// 【临时提案 · 待裁决】 本文件暂无
// ============================================================

import { print } from 'io'                // 裸名 = 工具链标准库 (不带 .as:
                                          // std 物理布局是工具链私事)
import { channel, spawn } from 'iso'      // 并发设施同居一个命名空间;
                                          // 直接点名绑定, 无模块前缀调用

func i32 main = () -> {
    val channel<string> ch = channel<string>()    // 工厂; 类型槽 = 产出契约, 强制非空
    val sender<string> tx = ch.sender()           // 发送端 — 独立所有权实体
    val receiver<string> rx = ch.receiver()       // 接收端 — 独立所有权实体

    spawn(() -> {                 // 函数字面量裸形态: (参数) -> 体;
                                  // func 是绑定词, 永不进 '=' 右边.
                                  // 函数值整体 move 过 isolate 边界,
        var i32 i = 0;            // tx 随之迁移, 本域从此不可再发
        for i < 10 {
            tx.send('消息 $i')
            increase i
        }
        // 函数体终结 => tx 析构 => 对 rx 而言信道关闭.
        // "忘记关闭"在此模型下无法书写 — RAII 兼职了生命周期管理.
    })

    var bool receiving = true;
    while receiving {
        match rx.recv() {         // 阻塞等待; 产出 option<string>
            some(msg) -> print(msg)
            none -> receiving = false    // 所有发送端已析构
        }
    }
    return 0
}