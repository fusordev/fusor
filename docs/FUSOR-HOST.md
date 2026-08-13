# FUSOR-HOST 设计:Node 级宿主层

- 状态:已批准(2026-08-14,对话式分节审阅通过)
- 阶段:alpha —— **无任何版本兼容承诺**,内部格式可自由演化,不匹配即 fail closed
- 相关文档:ARCHITECTURE.md(信任边界)、EXTENSIONS.md(适配器要求)、PORTING.md(兼容清单)、BYTECODE_VERIFIER.md(验证哲学)

## 1. 目标与非目标

### 目标

把 CLI 与嵌入能力从"能跑的引擎切片"提升为 **Node.js 级别的底层实现完整性**:
事件循环、timers、进程生命周期、宿主绑定、快照启动、模块解析、TypeScript
支持——机制层完整可靠,不依赖"每个 API 手写一个闭包"的 demo 式做法。

### 非目标(YAGNI,后续需求各自走新的 spec 流程)

- Web 标准库与 Node 标准库的 **API 面**兼容(不做 `node:fs` 完整 API 等)
- worker threads、多 agent 并发模型
- 快照版本迁移与向后兼容(alpha 明确排除)
- 资源表随 fs/net 扩展(首版仅建机制)、`beforeExit` 钩子
- `exports` 条件数组、symlink 真实路径解析
- TS 不可擦除语法的 lowering(enum、带值 namespace、参数属性 → 明确拒绝)

### 全局质量要求

- **公开 API 文档**:所有对外公开接口(pub 项,含 fusor-runtime 嵌入 API、fusor-host、
  fusor-ops 宏)必须有**明确清晰的英语 rustdoc**:
  - 行为与语义(参数、返回值、副作用)
  - `# Errors` 段:何种误用产生哪个错误变体(对齐现有惯例,如 `create_host_function`/
    `set_global` 的文档风格)
  - 规范引用:ECMA-262 章节或内部方法名(如 `[[DefineOwnProperty]]`)
  - 明确"不 panic"保证;借入/所有权语义;非显然用法附示例
  - 宿主不可达的私有项不强制
- **强制手段**:公开 crate 开启 `#![warn(missing_docs)]`(存量补齐后再升级为 deny)

### 关键决策记录(已逐条确认)

| 决策 | 选择 |
| --- | --- |
| 执行器 | Tokio,单 owner 任务碰 JS 堆 |
| 事件循环语义 | 严格 ECMA-262 Host Hooks,不模仿 libuv 相位怪癖 |
| 绑定层模型 | Deno ops + serde(`#[op]` 宏 + JsValue↔serde 桥接) |
| 快照路线 | 完整堆序列化(V8/Node 式) |
| 组装机制 | Overlay(Deno Extension 式;曾用名 Plugin) |
| 全局暴露 | `Fusor` 命名空间对象,op 平铺为 `Fusor.ops.<snake_case 名>`;移除 `print` 全局 |
| 诊断渲染 | miette + 彩色,单一渲染管线 |
| 初始化脚本 | ESM 模块图,warmup / startup 双模式 |
| 成功标准 | conformance suite + 精选真实脚本 |

## 2. 审查基线(设计动机)

2026-08-14 对现有 Rust interop 实现的四域审查(host 函数边界 / 值与根化模型 /
运行时生命周期与 facade / 测试覆盖)得出的基线。本设计直接修复其中
"缺陷"与"缺口"两类:

### 已确认缺陷(本设计修复)

| 严重度 | 缺陷 | 位置 | 本设计对应 |
| --- | --- | --- | --- |
| 高 | host 函数构造路径损坏:`new f()` 无 this 对象、原语返回值不转换、无 `prototype` 属性,`instanceof` 抛错 | native.rs / context.rs | §子项目 1 构造修复 |
| 中 | `set_global` 不校验 foreign 值、重复 key 产生影子槽位、绕过 `[[Extensible]]`(冻结的 global 可加属性) | runtime/context.rs:1205 | §子项目 1 属性 API 重写 |
| 中 | `host_functions` 槽位永不释放,每次 `create_host_function` 永久泄漏闭包(含错误路径) | context.rs:1130 / gc.rs | §子项目 1(快照的 op 元数据重绑定同样依赖) |
| 中 | 同一 host 函数自我重入 → `EngineFault`(槽位 take/restore 破坏调用环) | native.rs:2563 | §子项目 1 错误契约统一 |
| 中 | 模块求值非异常失败被拍平成字符串,`exception() == None`,类型不可区分 | modules/evaluation.rs:714 | §错误分类 |
| 中 | 静态模块图无条件 `register_module` 覆盖,跨图重复求值违反每 realm 一次语义;动态路径却有 `has_module` 检查 | fusor/src/lib.rs:1335 | §子项目 1/6(模块注册契约) |
| 低 | `call_function`、host 回调返回值缺 `validate_owner`,外来值以 `EngineFault` 冒泡 | context.rs:1073 / native.rs:2578 | §子项目 1 契约统一 |
| 低 | host 驱动 `set_global`/`create_host_function` 绕过对象属性资源上限 | context.rs | §子项目 1 |
| 低 | `Runtime::create_realm` 内部不变量用 `.expect` panic | realm.rs:583 | §子项目 1(改为错误返回) |
| 低 | 任意根句柄 drop → 下次安全点满 GC(root churn 每迭代付满代价) | gc.rs:1967 | 记录为已知性能项,快照/loop 设计中规避 |

### 已确认健全(不重做)

- 根化模型:`Arc<ValueRoot>` + 每节点 `public_roots` + `ReleaseMailbox`,未发现悬挂路径
- 全 crate `#![forbid(unsafe_code)]`;借用检查强制单 Context 生命周期
- 跨 runtime id 混淆不可能(每 id 带 `RuntimeIdentity`,mailbox 地址在句柄存活期稳定)
- 引擎已有:Host Hooks 默认实现、FinalizationRegistry cleanup、`drain_host_jobs`、
  `PendingDynamicImport` park/resolve 模式、`InterruptHandler`、模块注册/求值/import defer/TLA

### 已确认缺口

- `Object` 句柄完全 opaque:无任何宿主属性 get/set/define/keys API
- `set_global` 零测试覆盖;host 函数仅 1 处测试;构造/重入/回调中 GC/信号全无测试
- facade 在公开签名泄漏 `fusor_runtime` 类型却不 re-export,"只用 fusor"不可能
- CLI 无事件循环、无 timers、无信号处理、无 exit code 体系;错误输出仅顶层 `Display`
- PORTING.md:65 文档漂移(`print` 与旧 crate 名)

## 3. 架构与子项目分解

```
fusor-ops      (proc-macro crate:#[op] 属性宏)
    ↓ 生成签名元数据 + serde 适配代码
fusor-host     (新 crate,宿主适配器)
    ├─ ops       op 注册表 + JsValue↔serde 桥接 + 资源表
    ├─ loop      ECMA-262 Host Hooks 事件循环 + timers
    ├─ process   信号/exit codes/uncaught 策略/取消与 shutdown
    ├─ overlay   组装机制(ops + 内嵌 ESM init)
    └─ loader    node_modules 解析(现 CLI resolver 迁入)
fusor-runtime  (引擎核心:嵌入 API 改进 + snapshot 模块,不感知 fusor-host)
fusor-frontend (TypeScript 剥离 pass)
fusor-cli      (瘦客户端,迁移到 fusor-host)
```

**依赖方向**:`fusor-cli → fusor-host → fusor-runtime`,无反向依赖;serde 与 Tokio
全量不进引擎核心(ARCHITECTURE.md:41 信任边界);快照 codec 自写、引擎零新依赖。

| # | 子项目 | 落点 | 依赖 |
| --- | --- | --- | --- |
| 1 | 嵌入 API 改进 | fusor-runtime | — |
| 2 | ops 绑定层 + 资源管理 | fusor-ops + fusor-host::ops | #1 |
| 3 | 事件循环核心 | fusor-host::loop | #1(Promise resolver) |
| 4 | 进程生命周期与诊断 | fusor-host::process | #3 |
| 5 | Snapshot 与初始化脚本(ESM) | fusor-runtime + fusor-host | #1、#2 |
| 6 | Overlay 组装机制 | fusor-host::overlay | #2、#3、#5 |
| 7 | node_modules 解析 | fusor-host::loader + CLI 迁移 | #6(loader 落点) |
| 8 | TypeScript 类型剥离 | fusor-frontend + loader 集成 | #7(goal 判定) |

**建议执行顺序**:1 → 2 → 3 → 4 → 5 → 6 → 7 → 8;#7/#8 可与 #4–#6 并行。

## 4. 子项目 1:嵌入 API 改进(fusor-runtime)

参照 V8 嵌入 API 改进;已确认的取舍:采纳其属性/异常/Promise resolver 概念,
**不采纳** Template 系统(C++ 特性,在 Rust + serde ops 模型下冗余)、Snapshot
之外的 scope 机制(Arc 根化已优于 `HandleScope` 纪律)、`Locker`(借用检查天然
单所有者)。

### 4.1 属性 API(`Object` 从 opaque 变完整)

```rust
pub fn get(&self, ctx: &mut Context, key: PropertyKey) -> Result<JsValue, ExecutionError>;                 // [[Get]]
pub fn set(&self, ctx: &mut Context, key: PropertyKey, value: JsValue) -> Result<(), ExecutionError>;       // [[Set]]
pub fn define_own_property(&self, ctx: &mut Context, key: PropertyKey, desc: PropertyDescriptor) -> Result<bool, ExecutionError>; // [[DefineOwnProperty]]
pub fn has(&self, ctx: &mut Context, key: PropertyKey) -> Result<bool, ExecutionError>;                     // [[HasProperty]]
pub fn delete(&self, ctx: &mut Context, key: PropertyKey) -> Result<bool, ExecutionError>;                  // [[Delete]]
pub fn own_property_keys(&self, ctx: &mut Context) -> Result<Vec<PropertyKey>, ExecutionError>;             // [[OwnPropertyKeys]]
```

- `PropertyKey` = string | symbol;`PropertyDescriptor` = value 或 get/set + writable/enumerable/configurable
- 全部路由到 `define_property.rs` 不变量校验路径(不可配置/不可写/`[[Extensible]]` 全生效)——宿主与 JS 共享同一套语义
- `Context::set_global` 改为 `[[DefineOwnProperty]]` 语义(`{value, writable: true, enumerable: false, configurable: true}`):重复 key 走已有属性更新路径;补 `validate_owner`;对冻结 global 按规范抛错

**API 补全(2026-08-14 实现期确认,原设计隐含但未写明)**:宿主需要构造 key 与取得对象,随本项一并落地:

- `Context::property_key(&str)`:字符串 key(整数字符串 → 规范数组索引 key)
- `Context::property_key_from_value(&JsValue)`:同 runtime String/Symbol 值 → key;不做 `ToPropertyKey` 隐式强转,其余类型报错(fail closed)
- `Context::global_object()`:realm global 为 `Object` 句柄,宿主读/写全局属性的入口(`set_global` 的逆操作)

**语义决策(实现期确认)**:六个方法无 `ExecutionLimits` 参数,可观察用户代码(getter/setter/Proxy trap)统一在 `ExecutionLimits::default()` 下执行,且不提供 dynamic-function 编译器;`set` 恒为严格模式(无 sloppy 静默吞错),拒绝写抛 `TypeError`——fail closed;`define_own_property` 与 `delete` 返回内部方法布尔结果(`Reflect.*` 契约,普通拒绝不抛错);`own_property_keys` 普通对象走零分配快照路径,Proxy 走 trap 验证机制。

### 4.2 构造路径修复(高严重度)

`create_host_function` 安装时创建规范 `prototype` 自有属性(普通对象 +
`constructor` 回指);`dispatch_host_function` 构造分支按 `[[Construct]]`:
`this = OrdinaryCreateFromConstructor(new_target, "%Object.prototype%")`,
回调返回对象用之、返回原语则返回 this;`HostCall::this()` 在构造调用中收到
真实 this 对象。

### 4.3 异常 API 与堆栈

`Context::error(kind, message)` 构造 Error/TypeError/RangeError/SyntaxError 实例
(可直接 `Err(value)` 抛出);`CallError::Thrown` 与 `ExecutionError::Exception`
携带结构化 `StackTrace`(frame:source/span/function name)。

### 4.4 Promise resolver

`Context::new_promise()` → `(Promise, PromiseResolver)`,resolver 提供
`resolve`/`reject`;内部复用 vm/promise.rs 机制,根化采用 `PendingDynamicImport`
的 park 模式。异步 op 与事件循环的依赖项。

### 4.5 值转换补全

在 `kind()/as_*` 之上补 `as_i32/as_u32/as_f64/as_bigint` 与
`to_string/to_number/to_boolean`(暴露 vm/conversions.rs 已实现的规范转换)。

### 4.6 错误契约统一

`call_function`、`set_global`、host 回调返回值三处补 `validate_owner`;外来/过期
值一律 `HandleError::{ForeignRuntime, Stale}`,杜绝 `EngineFault::StaleHeapEdge`
冒充内部引擎错误。`create_realm` 的 `.expect` 改为错误返回。host 驱动安装操作
纳入对象属性资源上限计量。`host_functions` 槽位释放:GC 收集 host Function 对象
时经终结路径清槽(与快照的 op 元数据重绑定共用此路径)。

## 5. 子项目 2:ops 绑定层 + 资源管理(fusor-ops + fusor-host::ops)

### 5.1 声明形态(Deno 式)

```rust
#[op]                                       // 同步:反序列化 → 调用 → 序列化
fn op_read_text(path: String) -> Result<String, OpError> { ... }

#[op(async)]                                // 异步:返回 Promise,future 在 Tokio 上跑
async fn op_sleep(ms: u64) -> Result<(), OpError> { ... }
```

- op 名 = 函数原 snake_case 名,可 `#[op(name = "...")]` 覆盖
- 约定:sync 命名 `op_*`、async 命名 `op_async_*`(仅约定,无机械变换)
- 宏生成:注册元数据、参数反序列化、返回/错误序列化;op 函数体在宿主 crate,
  不接触引擎类型

### 5.2 JsValue↔serde 桥接(引擎核心不引入 serde)

- `Deserializer` 消费 `JsValue`:整数类型仅接受安全整数范围内的整值 f64(非整值
  → `TypeError`,注明参数序号);`Option`/null、bool、String、Vec/元组 ← Array、
  struct/HashMap ← 普通对象(自有可枚举字符串键)
- `Serializer` 产生 `JsValue`(持有 `&mut Context`):unit→undefined、bool/number/
  str 直映、seq→Array、map/struct→Object、`Option::None`→null
- v1 不做 bytes/Uint8Array 绑定(fs/net 进入时再加)

### 5.3 错误模型

`OpError { class: Option<&'static str> /* JS Error 类,默认 Error */, message, code }`;
宏用 §4.3 异常 API 构造抛出;参数反序列化失败 → `TypeError`;不可序列化返回值 →
`InternalError`。

### 5.4 注册与安装:`Fusor.ops` 子命名空间

- init 阶段创建 `Fusor` 命名空间对象(普通对象,global 上
  `define_own_property` 安装:`writable: false, configurable: false, enumerable: false`),
  其下 `Fusor.ops` 子对象
- 每个 op 以**函数原 snake_case 名**安装为 `Fusor.ops` 属性;overlay 间同名冲突
  在组装期检测 → 构建期报错
- overlay 的 init ESM 负责把原始 op 包装成惯用 API(类型化 JS 包装层归属 overlay)
- 宿主全局惯例保留:`setTimeout/setInterval/clearTimeout/clearInterval/setImmediate`
  (浏览器/Node 惯例)、`queueMicrotask`(ECMA-262 内建,引擎所有);`print` 全局
  **移除**(输出能力由 overlay/op 提供)

### 5.5 异步 op 与单 owner 约束

宏生成的 host function 调 `Context::new_promise()`(§4.4)返回 Promise;参数反序列
化为自有 Rust 类型后,future spawn 到 fusor-host 的 Tokio runtime(宏编译期检查
`Send + 'static`);完成信号经 mpsc 回主任务,事件循环排空完成队列、在 owner 任务
上调 resolver。引擎堆始终只被主任务触碰。

### 5.6 资源管理(全局资源表,Deno rid 式)

```rust
pub trait Resource { fn name(&self) -> &'static str; fn close(self: Rc<Self>) {} }
pub struct ResourceTable { ... }   // add(Rc<dyn Resource>) -> ResourceId(u32 单调不复用)
                                   // get / get_mut / close / close_all
```

- 宿主运行时级一张表,所有 op 共享;纯宿主层概念,引擎零改动;`Rc` 而非 `Arc`
  (单 owner,资源只在主任务被触碰)
- `#[op]` 的 `ResourceId` 参数:宏特殊处理(表查找,查不到 → `TypeError` 风格
  `OpError`);`#[op(async)]` 在 await 前自动 clone 所需资源的 `Rc`,异步期间资源
  不可被 close 摘除
- 资源不是 JS 值、不进引擎堆、不参与 GC,JS 侧只见 rid 数字(首版不提供包装器)
- 生命周期:JS 主动 close(op) → 移除+清理;最后 `Rc` drop → `Drop` 清理;宿主
  shutdown → `close_all`(§进程生命周期联动)

### 5.7 宏实现

`fusor-ops` 为 proc-macro crate(Rust 硬性要求),输出普通 token 流,只引用
fusor-host 与 fusor-runtime 公开 API;展开可用 cargo expand 验证。

## 6. 子项目 3:事件循环核心(fusor-host::loop)

### 6.1 结构与单 owner

`HostLoop` 持有 Tokio `current_thread` runtime,单主任务驱动一切;引擎类型非
`Send`,所有引擎交互只在主任务发生——与现有 CLI 模式同构,但循环**持续存活**
而非一次性 drain-to-quiescence。

### 6.2 ECMA-262 对齐

- Job 队列语义由引擎已有机制承担(`HostEnqueuePromiseJob` FIFO、
  `HostMakeJobCallback`/`HostCallJobCallback` 默认、FinalizationRegistry cleanup
  jobs post-event);loop 负责调度时机
- 规范性约束(写死并文档化):每个 job 完整执行后才执行下一个;job 之间不插入
  宿主回调;**每个宿主事件处理后立即 drain 到静止**(微任务检查点)
- timers/`setImmediate` 是宿主 API(ECMA-262 不管),语义自定义并文档化,不模仿
  libuv 相位怪癖

### 6.3 事件源与 turn 结构

事件源:① timers(Tokio `Sleep`)② 异步 op 完成信号(mpsc,§5.5)③ 信号事件
(§7)④ `Atomics.waitAsync` deadline(已有惰性 Tokio 信号)⑤ 宿主自定义事件
(提交闭包到队列)。

```
事件到达 → 执行事件处理(timer 回调 / op 完成 → resolve|reject / 信号处理)
        → drain_host_jobs 至静止(微任务 + cleanup jobs)
        → 无事件时 Tokio select 等待下一事件源
        → 无存活事件源且无 pending 时退出(可配置)
```

### 6.4 timers

`setTimeout/setInterval/clearTimeout/clearInterval/setImmediate` 实现为常规 ops +
loop 内 timer 记录堆:到期时间排序、同刻到期按创建序;delay 取 ms 向下取整、负值
归 0(与 Node/浏览器一致);回调经引擎 `CallJobCallback` 语义调用;`setImmediate`
定义为"当前 turn 事件处理完成后、drain 前"的队列。pending timers 计为存活
(alive 判定简化版,文档化)。

### 6.5 宿主驱动 API

```rust
let host = HostRuntime::builder()
    .with_overlays(&[&CoreOverlay])   // 子项目 6
    .with_snapshot(blob)             // 子项目 5(可选)
    .with_init_sources(init)         // 子项目 5(可选)
    .build()?;
host.run_main("main.mjs", source)?;   // 主脚本,循环存活直到无 alive 事件
host.run_until_idle()?;               // 显式驱动(测试用)
```

### 6.6 引擎侧增量

近零:`drain_host_jobs`、`InterruptHandler`、Promise resolver(§4.4)均已有或在
计划内;loop 只消费公开 API。CLI 迁移:REPL entry 提交为宿主事件;CDP 请求通道、
stdin 通道作为事件源进入同一 turn 循环(替代现在 run_with_inspector 的手工多路
复用)。

## 7. 子项目 4:进程生命周期与诊断(fusor-host::process)

### 7.1 信号处理

`tokio::signal` 作为事件源(§6.3 ③)。语义:首次 SIGINT → 触发引擎
`InterruptHandler`(当前 JS 在指令边界抛不可捕获 `Interrupted`,REPL 中即"中断
当前输入求值");第二次 SIGINT / SIGTERM → 强制退出,exit code = `128 + n`
(Node 语义)。宿主可注册 JS 侧处理器(`process.on("SIGINT", ...)`,经 ops 暴露);
默认策略:无处理器时按上述退出。

### 7.2 exit codes(文档化)

| 情形 | code |
| --- | --- |
| 主脚本完成 + 无 alive 事件 | 0 |
| uncaughtException | 1 |
| unhandledRejection(对齐 Node 15+) | 1 |
| 强制信号 | 128 + n |
| 资源/限制类引擎中止(instruction limit 等) | 专用非零码 |

`process.exit(code)` 为常规 op;**退出不等待** pending 异步 op(文档化;
`beforeExit` YAGNI)。

### 7.3 uncaughtException / unhandledRejection

- 同步未捕获:主任务捕获 `CallError::Thrown`/`ExecutionError` → 宿主未注册处理
  器时:完整 stack trace(§4.3,frame 映射回源 span)+ exit 1;宿主注册的处理器
  作为 loop 事件回调
- 异步未捕获:引擎已有 `promise_rejection` 跟踪模块,fusor-host 在其上暴露事件,
  默认"警告 + exit 1"

### 7.4 干净取消与 shutdown 序列(文档化顺序)

```
① 停止接受新事件源
→ ② cancel 信号:pending 异步 op 的 future 被 drop(Tokio 取消语义,资源随 Rc drop 清理)
→ ③ close_all(资源表 §5.6)
→ ④ Atomics.waitAsync waiters 取消(引擎已有 Drop 路径)
→ ⑤ drop Runtime(引擎 Drop 已验证 clean)
```

shutdown 期间不再 drain 微任务(文档化)。

### 7.5 统一诊断渲染(miette + 彩色)

- **单一渲染管线**:所有错误层(编译/求值/模块/op/快照/解析)经适配实现 miette
  `Diagnostic`,CLI 与 REPL 共用一条 `GraphicalReportHandler` 渲染路径,无"只打印
  顶层 `Display`"的旁路
- **彩色输出**:ANSI 色(severity 配色、源片段 + span 下划线 + label、错误码/建议);
  TTY 检测 + `NO_COLOR` 尊重 + `--no-color` 显式开关;非 TTY 降级无色
- **内容**:结构化 StackTrace(§4.3)映射回源文件与 span;错误码与 §12.1 分类对应
  (沿用项目 `FUS-xxx-xxx` 惯例);相关行提供修复建议(如裸说明符解析失败 → 提示
  可能的 node_modules 路径)
- 项目已用 miette(ARCHITECTURE.md:35"host diagnostics 不以 Miette 输出为语义真
  值")——本项是渲染层完成,非引入

### 7.6 可测试性

信号作为**可注入事件源**(测试发合成信号事件,不依赖真实 OS 信号);shutdown 序列
每步可断言(资源表计数、waiters 状态、exit code)。

## 8. 子项目 5:Snapshot 与初始化脚本(fusor-runtime + fusor-host)

### 8.1 概念与边界

- **blob = 完整堆序列化**:创建期执行引擎安装 + overlay 组装(ops 安装 + init ESM
  求值,含 `Fusor` 命名空间)→ 序列化整个堆;加载期反序列化直接恢复,跳过安装执行
- **无版本兼容**(alpha 决策):magic + 格式戳,不匹配 fail closed 拒绝
- 实现落在 fusor-runtime(序列化器需堆内部访问);fusor-host 只提供 builder 侧
  创建/加载 API

### 8.2 序列化内容与不可序列化项

- 序列化:全部 heap records(objects + shapes + 属性表、functions、strings、atoms
  表、模块注册表、realm 表、binding cells)
- **Rust 闭包不可序列化**:堆内 host Function 对象解耦存储——blob 记录"host 槽位 +
  op 元数据",恢复时宿主重建 op 闭包表并重绑定;op 集按序匹配,不匹配 fail closed
- **资源表不可序列化**(fd 等运行时资源):快照中不存资源;overlay init 创建的资源
  必须遵循"启动期惰性重建"约束(Deno 同构,文档化)
- GC 状态不序列化:恢复后从干净标记状态开始;finalization registry 对象保留、
  待清理队列清空

### 8.3 序列化器与校验

- 自写紧凑二进制 codec(遵循项目 codec 惯例,fusor-bytecode/codec.rs 同风格,
  **引擎零新依赖**)
- **加载即校验**(BYTECODE_VERIFIER 同精神):magic + 格式戳 + 结构校验(引用完整
  性、shape/计数一致性、跨表引用闭包);恢复失败 → 明确错误 + 干净 drop,无 panic
- 恢复路径:空 Runtime 骨架 → 反序列化填充 → 校验 → 就绪

### 8.4 初始化脚本(ESM 模块图)

- 源:overlay 内嵌 ESM(§9)或宿主显式提供;Module goal 编译 + 模块图求值(引擎
  已有全套能力)
- 双模式:`warmup`(创建期求值,效果烘焙进快照,默认)/ `startup`(恢复后前置求值,
  宿主显式指定,用于依赖运行时环境的部分)
- 创建工具:CLI 子命令 `fusor snapshot -o blob` + builder API `build_snapshot()`

### 8.5 与 overlay 的关系

`HostRuntime::from_snapshot(blob, overlays)` —— overlays 仅用于重建 op 闭包表,
**不重新执行 init ESM**;warmup 效果已在 blob 中。

### 8.6 测试

- 往返一致性:创建 → 恢复 → 断言(全局形状、`Fusor.ops` 属性表、init 模块导出、
  同一脚本行为等价)
- 负例:截断/篡改 blob → fail closed 且无 panic;格式戳不匹配 → 拒绝;op 集不匹配
  → 拒绝
- 启动收益基准(挂 PORTING.md benchmark 待办)

## 9. 子项目 6:Overlay 组装机制(fusor-host::overlay)

Deno `Extension` 式;曾用名 Plugin,已确认改名 Overlay。

```rust
pub trait Overlay: 'static {
    fn name(&self) -> &'static str;
    fn ops(&self, registry: &mut OpRegistry);              // 注册 op(§5)
    fn init_sources(&self) -> Vec<OverlaySource>;          // 内嵌 ESM 源
    fn entry(&self) -> &'static str;                       // 入口模块说明符
    fn dependencies(&self) -> &'static [&'static str];     // 依赖的其他 overlay(排序)
}

pub struct OverlaySource { pub specifier: String, pub text: &'static str }
```

### 组装语义(`HostRuntime::builder().with_overlay(p1).with_overlay(p2).build()`)

1. 拓扑排序 overlay 依赖,环检测 → 构建期报错(alpha:不做运行时容错)
2. 所有 op 注册进 `OpRegistry` → 安装为 `Fusor.ops.<name>`(§5.4)
3. 各 overlay init 模块图按序求值(宿主全局注入发生在 init 模块里,替代现在的
   `set_global` 引导方式);init 模块可 `import` 其他 overlay 的 init 模块
   (`PluginModuleLoader` 解析内嵌虚拟模块说明符 + 可选文件系统回退)
4. 结果状态即快照输入(§8:组装 + init 求值后序列化;加载快照 = 跳过 1–3)

CLI 自身成为"核心 overlay + CLI overlay"的组合,不再手写安装逻辑。引擎侧零改动。
迁移备注:现 REPL 的 `print` 捕获缓冲(DevTools `Runtime.consoleAPICalled` 事件源)
改由 console overlay 承担。

## 10. 子项目 7:node_modules 解析(fusor-host::loader + CLI 迁移)

现 CLI resolver(相对/绝对路径、`node:` 内建)迁入 fusor-host::loader,新增:

- **裸说明符**(`foo`、`@scope/pkg`)→ 从 referrer 目录逐级向上找 `node_modules/`;命中后读 `package.json`:
  - v1 支持:`main` 字段、`exports`(字符串/简单对象,条件匹配仅 `default` + `import`)、`index.js` 回退、`.js/.mjs` 扩展名回退、`type` 字段决定 Script/Module goal(与 §11 联动)
  - 不支持:`exports` 条件数组/`require` 条件、symlink 真实路径解析(后续)
- 失败 fail closed,诊断注明解析步;与相对/绝对路径解析、`node:` 内建并列
- 依赖 #6(loader 落点);可注入 fixture 树测试

## 11. 子项目 8:TypeScript 类型剥离(fusor-frontend + loader 集成)

Node 22+ strip-types 同政策 = **仅可擦除语法**;EXTENSIONS.md 已列为可选层,现
正式纳入。

- `.ts`/`.mts`/`.cts` 源类型(Oxc TS 解析,前端已有 Oxc);`package.json` `type`
  字段 + 扩展名决定 Script/Module goal
- **可擦除**:类型注解、interface/type alias、`implements`、`as`/`satisfies`/非空
  断言、`import type`、泛型参数
- **不可擦除即失败关闭**:enum、带值的 namespace、参数属性(constructor 参数修饰)
  等 → 明确诊断拒绝,不做 lowering(Node 同政策)
- 管线:TS AST → 剥离 pass → 现有 JS 管线(bytecode 验证链不变);剥离在
  fusor-frontend 新模块,编译产物仍为普通 verified bytecode
- 测试:可擦除语料往返 + 不可擦除负例(诊断断言)

## 12. 错误分类与测试策略(全局)

### 12.1 错误分类(跨层统一,吸收审查发现)

| 层 | 类型 | 语义 |
| --- | --- | --- |
| 句柄误用 | `HandleError::{Orphaned, ForeignRuntime, Stale, WrongValueKind}` | 所有入口统一 `validate_owner`(§4.6) |
| 引擎执行 | `ExecutionError::{Exception, Interrupted, InstructionLimitExceeded, LimitExceeded, EngineFault, Handle}` | `Exception` 携带结构化 StackTrace |
| 宿主调用 | `CallError::{Thrown(JsValue), Execution}` | `Thrown` 保留抛出值同一性 |
| 模块 | `ModuleEvaluationError` 分阶段且保留类型化 cause + `ModuleResolutionError` | 修复"拍平成字符串"缺陷 |
| op 层 | `OpError { class, message, code }` | 反序列化失败 → `TypeError` |
| 快照 | `SnapshotError::{FormatMismatch, IntegrityViolation, OpSetMismatch, ...}` | fail closed,无 panic |
| 进程 | 文档化 exit code 表(§7.2) | 与错误分类一一对应 |

### 12.2 测试策略

- **引擎侧**(fusor-runtime):补审查发现的缺口——属性 API 全路径(含冻结对象负
  例)、host 构造路径(`new`、原语返回、instanceof)、`set_global` 三缺陷回归、
  `validate_owner` 契约、Promise resolver、GC 与根化交互(回调持句柄期间 collect)
- **fusor-host**:虚拟时钟(loop 测试不发真实 timer)、可注入事件源、ops serde 往
  返、资源表生命周期(close/drop/shutdown 全路径)、overlay 组装(拓扑/环/冲突)、
  快照往返 + 篡改负例、resolver fixture 树(node_modules 场景)、TS strip 语料
  (可擦除 + 不可擦除负例)
- **集成验收**(成功标准):conformance suite(ECMA-262 host hooks 断言 + 文档化宿
  主语义)+ 精选真实脚本(完整 REPL 会话、定时器/信号脚本)
- CI:全部并入 workspace 测试;快照/loop 测试不依赖真实时间与 OS 信号

## 13. 验收标准与范围外

### 验收(已确认:conformance + 真实脚本)

1. 自建 conformance suite:ECMA-262 host hooks 规范性要求(FIFO job 队列、
   `HostEnqueuePromiseJob` 语义、finalization cleanup)+ 宿主 API 文档化语义
   (timers、进程生命周期)为断言源,作为回归基线
2. 非平凡真实脚本作集成验收(完整 REPL 会话、含定时器/信号的脚本)
3. 文档:PORTING.md 漂移修正(`print` 现状、旧 crate 名)、ARCHITECTURE.md 的
   `fusor-tokio` 计划位更新为已落地 fusor-host

### 范围外(重申)

worker threads、Web/Node 标准库 API 面、快照版本迁移、`exports` 条件数组、
symlink 解析、TS 不可擦除 lowering、资源表随 fs/net 扩展、`beforeExit`、
bytes 绑定。后续需求各自走新的 spec → plan → 实现流程。

## 14. 风险与开放问题

| 风险 | 等级 | 缓解 |
| --- | --- | --- |
| 快照堆序列化工程量大(GC 图、跨表引用闭包) | 高 | 自写 codec 有项目先例;范围限定无版本兼容;测试驱动往返一致性 |
| `#[op]` 宏复杂度(签名解析、serde 生成) | 中 | 只生成公开 API 引用;cargo expand 验证;flat 参数首版 |
| 引擎侧 hook 增量超出"近零"预估 | 中 | 子项目 1 先行,以公开 API 为准绳;发现缺口即升级为独立子项目 |
| 已知性能项:root drop → 满 GC;host 闭包槽位泄漏(修复前) | 低 | 记录在案,§4.6 修槽位;root churn 由快照/loop 设计规避,后续 benchmark 评估 |
| 文档漂移(PORTING/ARCHITECTURE) | 低 | 验收第 3 条强制同步 |





