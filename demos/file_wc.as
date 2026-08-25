// ============================================================
// demos/file_wc.as
// 试运行范围: result 手工展开 / 错误换型 / 高阶迭代 / struct
//
// 【已定稿文法】 result<t, e> 小写枚举 ok/err / match 穷尽性 /
//   无迭代语法·全高阶函数 / 匿名 func 字面量 / 类型前置参数 /
//   struct 字段即绑定 (val/var 显式可变性) /
//   构造为等号命名传参: stat(lines = 3) /
//   下标访问 expr[i] (P5 已裁决) /
//   传播糖 expr? — 仅限同型错误传播, 脱糖 =
//     match e { ok(v) -> v, err(e) -> return err(e) } (P6 已裁决) /
//   字符串转义序列 \n 等 (P8 已裁决)
//
// 【临时提案 · 待裁决】 本文件暂无
//
// 【边界警示】 ? 不做错误换型. 本文件 count 把 io_error 换成
//   string, 无法用 ? 表达 — 错误换型机制 (map_err 或同类物)
//   尚未立案, 属下一设计议题; 在此之前换型处一律手写 match.
// ============================================================

import { print } from 'io'      // 裸名 = 工具链标准库; './' = 自己的文件
import { open } from 'fs'       // 模块直接交出自由函数, 无前缀调用
import { args } from 'os'

struct stat {          // 字段 = 实例内的绑定; 可变性写在脸上
    val i32 lines
    val i32 words      // val 字段: 实例上不可变
    var i32 bytes      // var 字段: 实例上可变, 运行时借用检测的盯梢对象
}

// 产出契约读法: 调用 count, 得到 result;
// ok 里装 stat, err 里装 string. 输入契约在函数体内立约.
func result<stat, string> count = (string path) -> {
    match open(path) {
        ok(file) -> {
            val string raw = file.read_all()
            val array<string> lines = raw.split('\n')
            val i32 n_lines = lines.len();
            val i32 n_words = lines
                .map((string line) -> return line.split(' ').len())
                .reduce(0, (i32 acc, i32 n) -> return acc + n);
            val i32 n_bytes = raw.len()
            return ok(stat(
                lines = n_lines,
                words = n_words,
                bytes = n_bytes,
            ))
        }
        err(e) -> return err('wc: 无法读取 $path — $e')
        // ↑ 此处必须手写 match: 它在做错误【换型】(io_error → string),
        //   ? 只管同型传播, 替代不了这一行 — 见头部边界警示.
    }
}

func i32 main = () -> {
    match count(args()[1]) {         // args 为 os 交出的自由函数, [1] 取第二个实参
        ok(s) -> {
            print('${s.lines} 行 ${s.words} 词 ${s.bytes} 字节')
            return 0
        }
        err(msg) -> {
            print(msg)
            return 1
        }
    }
}
