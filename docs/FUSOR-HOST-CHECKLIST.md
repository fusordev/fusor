# FUSOR-HOST 实现 Checklist

- 状态:alpha(2026-08-14)
- 设计文档:[FUSOR-HOST.md](FUSOR-HOST.md)(各条目引用其章节号)
- 使用方式:按子项目顺序执行,每项完成后勾选;每子项目的完成 = 其全部条目 + 全局 DoD 满足
- 执行顺序:1 → 2 → 3 → 4 → 5 → 6 → 7 → 8;#7/#8 可与 #4–#6 并行

## 全局 DoD(所有子项目适用)

- [ ] 保持引擎侧 `#![forbid(unsafe_code)]`
- [ ] 失败关闭:每条新拒绝路径有明确错误类型与诊断,无 panic
- [ ] 测试并入 workspace `cargo test`,CI 绿
- [ ] 不新增引擎核心依赖(serde 与 Tokio 全量不得进 fusor-runtime;快照 codec 自写)
- [ ] 公开接口文档:所有 pub 项有明确清晰的英语 rustdoc(行为/参数/返回、`# Errors` 错误变体、ECMA-262 引用、"不 panic"保证、非显然用法示例;风格对齐 `create_host_function`/`set_global` 现有惯例)
- [ ] 公开 crate 开启 `#![warn(missing_docs)]`(存量补齐后升级 deny)
- [ ] 相关文档同步(PORTING.md / ARCHITECTURE.md,见"收尾")

## 子项目 1:嵌入 API 改进(fusor-runtime)§4

### 属性 API(§4.1)

- [x] `Object::get/set/define_own_property/has/delete/own_property_keys` + `PropertyKey`/`PropertyDescriptor`
- [x] 全部路由到 `define_property.rs` 不变量校验路径(不可配置/不可写/`[[Extensible]]` 全生效)
- [x] `Context::set_global` 改为 `[[DefineOwnProperty]]` 语义(writable/enumerable:false/configurable)
- [x] `set_global` 三缺陷回归测试:foreign 值 → `HandleError::ForeignRuntime`;重复 key 覆盖(无影子槽位,delete 后无残留);冻结 global → 按规范抛错
- [x] 属性 API 全路径测试(每内部方法 × 正常 / 不变量违规 / 外来与过期句柄)

### 构造路径(§4.2)

- [x] `create_host_function` 安装规范 `prototype` 自有属性 + `constructor` 回指
- [x] `dispatch_host_function` 构造分支:`OrdinaryCreateFromConstructor`、原语返回值回退 this
- [x] 测试:`new f()` 得对象、`instanceof` 成立、`new_target` 身份、原语返回回退

### 异常 API(§4.3)

- [x] `Context::error(kind, message)`(Error/TypeError/RangeError/SyntaxError)
- [x] `CallError::Thrown` / `ExecutionError::Exception` 携带结构化 `StackTrace`(source/span/function name)
- [x] 测试:thrown 值同一性(JS `catch` 观察值与 Rust 侧一致);stack frame 映射

### Promise resolver(§4.4)

- [x] `Context::new_promise()` → `(Promise, PromiseResolver)`,resolver `resolve`/`reject`
- [x] 根化采用 park 模式(同 `PendingDynamicImport`)
- [x] 测试:resolve/reject 值同一性;跨 `drain_host_jobs` 存活

### 值转换(§4.5)

- [x] `as_i32/as_u32/as_f64/as_bigint`、`to_string/to_number/to_boolean`
- [x] 测试:每转换 × 边界值(安全整数界、NaN/Infinity、非法字符串)

### 错误契约(§4.6)

- [x] `call_function`、`set_global`、host 回调返回值三处补 `validate_owner`
- [x] `Runtime::create_realm` 的 `.expect` 改为错误返回
- [x] host 驱动安装操作纳入对象属性资源上限计量(测试:limit 触发路径)
- [x] `host_functions` 槽位释放:GC 收集 host Function 时清槽(泄漏回归测试:反复创建/丢弃)
- [x] host 函数自我重入:定义行为并文档化,不再以 `EngineFault` 暴露(测试:直接与间接重入)

### 模块契约

- [x] 静态模块图注册前 `has_module` 检查(与动态 import 路径一致);跨图共享模块只求值一次(测试:双图 + 副作用计数)
- [x] 模块求值错误保留类型化 cause(修复字符串拍平;测试:limit abort 与 JS exception 可区分)

## 子项目 2:ops 绑定层 + 资源管理(fusor-ops + fusor-host::ops)§5

- [x] `fusor-ops` proc-macro crate:`#[op]` / `#[op(async)]` / `#[op(name = "...")]`
- [x] `OpError { class, message, code }`;反序列化失败 → `TypeError`(注明参数序号)
- [x] JsValue `Deserializer`(整值检查、`Option`/null、seq/元组、map/struct)+ `Serializer`(unit/bool/str/seq/map/`Option::None`)
- [x] `Fusor` 命名空间对象(global 上 writable:false/configurable:false)+ `Fusor.ops` 子对象
- [x] op 以函数原 snake_case 名安装为 `Fusor.ops` 属性;overlay 间同名冲突组装期报错
- [x] 异步 op:`new_promise` + spawn(`Send + 'static` 编译期检查)+ mpsc 完成回主任务 resolve
- [x] 资源表:`add/get/get_mut/close/close_all`;`ResourceId(u32)` 单调不复用
- [x] `#[op]` 的 `ResourceId` 参数特化(查不到 → OpError);`#[op(async)]` await 前自动 clone `Rc`
- [x] 测试:serde 往返矩阵、资源生命周期全路径(close/drop/shutdown)、异步期间资源存活
- [x] 单 owner 断言:测试证明引擎交互只发生在主任务

## 子项目 3:事件循环核心(fusor-host::loop)§6

- [x] `HostLoop` 持续存活,非一次性 drain(虚拟时钟同步驱动;Tokio executor 随 §7 信号源回归)
- [x] turn 结构:事件处理 → `drain_host_jobs` 至静止 → 无事件时 select
- [x] 事件源:timers(虚拟时钟)、异步 op 完成 mpsc(§5.5)、自定义事件
- [ ] `Atomics.waitAsync` deadline 事件源(阻塞:引擎惰性信号接口未定,随子项目 4 信号工作一并接;信号事件源在子项目 4 首条)
- [x] timers ops + timer 记录堆(到期排序、同刻创建序、ms 向下取整、负值归 0)
- [x] `setImmediate` 队列语义(当前 turn 事件处理后、drain 前)
- [x] alive 判定(pending timers / 异步 op 队列);无 alive 且无 pending → 退出(可配置)
- [x] `run_main`(Global Script 权威字节码)/ `run_until_idle` API(§6.5 builder 形式随子项目 6)
- [x] 测试:虚拟时钟全 timer 场景;job 间不插入宿主回调(规范断言);退出条件矩阵

## 子项目 4:进程生命周期与诊断(fusor-host::process)§7

- [x] 信号事件源;SIGINT 首次 → 引擎 `InterruptHandler`;二次 SIGINT/SIGTERM → 128+n(可注入 + `spawn_signal_forwarder` OS 转发;OS 投递按 §7.6 用合成信号测试)
- [x] JS 侧 `Fusor.process.on("SIGINT", ...)` 注册(op 暴露;处理器替换默认策略、逐次投递、this=Fusor.process;SIGTERM 不可拦截,已文档化)
- [x] exit code 表(§7.2)实现与文档化(`ExitCode` 枚举 + `from_execution_error` 映射;引擎中止码=2 已补设计);`Fusor.process.exit(code)` op(8 位截断、下个 turn 边界生效、首个请求胜出)
- [x] uncaughtException / unhandledRejection 默认路径(exit 1)+ 宿主处理器(处理器经 `Fusor.process.on` 注册、作为 loop 事件回调;完整 stack 渲染随 miette 条目 §7.5 落地,当前默认路径渲染错误身份)
- [x] shutdown 序列 ①–⑤ 按序实现:`HostLoop::shutdown(self) -> ExitCode`(①消费 loop 停新源+forwarder 可控停止 ②take/drop OpRuntime 取消 future ③`close_all_resources` ④waiters 随引擎 Drop ⑤drop Runtime);期间不 drain;清理全部 thread-local 状态,同线程可再装新 loop
- [x] miette 统一渲染管线(`process::diagnostics`:`ColorPolicy` Auto/Always/Never + `resolve`/`from_env`、单一 `GraphicalReportHandler` 路径;求值层 `HostDiagnostic`(frame→源标签)、op 层 `OpDiagnostic`、默认路径 `MessageDiagnostic`;loop 默认 uncaught/unhandled 路径走该管线);编译/解析/快照层适配随各 crate 落地;CLI/REPL 接入随子项目 6 重组
- [ ] 错误码体系(纯数字码,按 §12.1 分类区间编排)与 §12.1 分类对应
- [ ] 测试:合成信号事件(不依赖 OS)、shutdown 每步断言、exit code 矩阵

## 子项目 5:Snapshot 与初始化脚本(fusor-runtime + fusor-host)§8

- [ ] 序列化:全部 heap records、shapes、atoms 表、模块注册表、realm 表、binding cells
- [ ] host Function 解耦:blob 记录槽位 + op 元数据;恢复时重建 op 闭包表并重绑定(不匹配 fail closed)
- [ ] 资源不进快照:文档化约束 + 启动期惰性重建模式
- [ ] 自写紧凑二进制 codec(零新依赖);magic + 格式戳 + 结构校验,加载即校验
- [ ] 失败路径:截断/篡改/格式戳/op 集不匹配 → `SnapshotError`,无 panic、干净 drop
- [ ] init ESM 双模式:warmup(默认,创建期烘焙)/ startup(显式,恢复后前置)
- [ ] `fusor snapshot -o blob` 子命令 + builder `build_snapshot()` / `from_snapshot(blob, overlays)`
- [ ] 测试:往返一致性(全局形状、`Fusor.ops`、init 导出、行为等价)、负例矩阵

## 子项目 6:Overlay 组装机制(fusor-host::overlay)§9

- [ ] `Overlay` trait + `OverlaySource`
- [ ] 拓扑排序 + 环检测(构建期报错)
- [ ] op 注册 → `Fusor.ops` 安装;init 模块图按序求值
- [ ] `PluginModuleLoader`(内嵌虚拟模块说明符 + 文件系统回退)
- [ ] CLI 重组为"核心 overlay + CLI overlay";`print` 全局移除
- [ ] DevTools `Runtime.consoleAPICalled` 捕获改由 console overlay 承担
- [ ] 测试:拓扑/环/冲突;init 模块互 import;快照交互(§8.5)

## 子项目 7:node_modules 解析(fusor-host::loader)§10

- [ ] 现 CLI resolver 迁入 fusor-host::loader
- [ ] 裸说明符从 referrer 逐级向上查找 `node_modules/`
- [ ] `package.json`:`main`、`exports`(字符串/简单对象,条件 `default`+`import`)、`index.js` 回退、`.js/.mjs` 扩展名回退、`type` 字段
- [ ] 解析失败诊断注明解析步,与相对/绝对路径、`node:` 内建并列
- [ ] 测试:fixture 树矩阵(scope 包、exports、回退链、失败路径)

## 子项目 8:TypeScript 类型剥离(fusor-frontend)§11

- [ ] Oxc TS 解析接入(`.ts`/`.mts`/`.cts`)
- [ ] 剥离 pass:类型注解、interface/type alias、`implements`、`as`/`satisfies`/非空断言、`import type`、泛型参数
- [ ] 不可擦除语法 → 明确诊断拒绝(enum、带值 namespace、参数属性)
- [ ] `package.json` `type` 字段 + 扩展名 → Script/Module goal(与 #7 联动)
- [ ] 测试:可擦除语料往返 + 不可擦除负例诊断断言

## 收尾(全部子项目后)

- [ ] PORTING.md 漂移修正(`print` 现状、旧 crate 名、嵌入 API 状态行)
- [ ] ARCHITECTURE.md `fusor-tokio` 计划位更新为已落地的 fusor-host
- [ ] benchmark:启动时间 / 快照收益(PORTING.md benchmark 待办挂钩)
- [ ] 集成验收:conformance suite(ECMA-262 host hooks 断言 + 文档化宿主语义)+ 精选真实脚本(§13)
- [ ] 已知性能项复评:root drop → 满 GC 的 root churn 代价(审查基线低危项,决定是否立项优化)
- [ ] CI 全绿 + release 构建
