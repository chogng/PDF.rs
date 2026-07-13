# PDF.rs 代码规范

- 文档编号：RPE-STD-001
- 版本：0.1
- 状态：稳定态开发基线（持续迭代）
- 适用范围：Rust 核心、Wasm/桌面适配层、TypeScript 浏览器宿主、测试与工具

## 1. 目的与规范等级

本规范定义代码进入主干前必须满足的工程约束。架构边界以[独立 Rust PDF 引擎开发设计文档](../architecture/independent_rust_pdf_engine_development_spec.md)为上位规范；本规范不得被解释为允许产品接入外部 PDF/2D 引擎。

“必须”表示 CI 或评审阻断项；“应当”表示默认规则，偏离时必须在 PR 中说明；“可以”表示不影响公共契约的实现选择。

优先级依次为：

1. 安全与正确性；
2. 可取消、可预算、可观测；
3. 确定性和可测试性；
4. 清晰的所有权和模块边界；
5. 性能；
6. 局部简洁性。

不得以性能、兼容性或开发速度为理由绕过安全检查、结构化错误、预算或测试 oracle。

## 2. 自动化基线

仓库必须固定 Rust toolchain、格式化规则和 lint 配置。主干至少执行：

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

项目引入对应工具后，还必须执行依赖许可证/来源检查、重复依赖检查、Wasm 构建检查、文档链接检查和测试 manifest 校验。CI 命令应封装在仓库脚本或任务入口中，开发者不应记忆一组与 CI 不同的隐藏参数。

- 不允许在源文件中批量加入 `allow` 以消除警告。
- 单项 lint 例外必须限制到最小作用域，并附原因、issue 或安全说明。
- 格式化结果不参与个人风格讨论；以固定工具输出为准。
- 生成代码必须放在明确目录，并由生成器和输入 hash 可复现生成。

## 3. 模块与依赖方向

代码依赖必须遵守架构文档定义的单向关系：

```text
core primitives → syntax/object → document/content → scene/text
                                              ↓
                                   reference/fast/gpu
                                              ↓
                                          runtime
                                              ↓
                               browser/desktop adapters
```

- `core` 不得依赖 DOM、浏览器类型、文件系统、网络、具体 async runtime 或 GPU API。
- PDF 语义类型不得依赖 UI、transport 或平台 handle。
- 平台类型不得泄漏到公共核心 API；通过本项目 trait 和稳定数据结构隔离。
- `tools/baseline` 只依赖导出的测试协议；任何产品 crate 不得反向依赖 `tools`。
- 不允许用 feature flag 隐藏逆向依赖或循环依赖。
- 一个模块只能有一个明确的语义 owner；跨模块修改必须在 PR 中说明契约影响。

## 4. Rust 命名与文件组织

- 类型、trait、enum 使用 `UpperCamelCase`；函数、变量、模块使用 `snake_case`；常量使用 `SCREAMING_SNAKE_CASE`。
- 名称表达 PDF 语义或工程角色，避免 `manager`、`helper`、`utils`、`data` 等无边界名称。
- 带单位或坐标空间的值使用新类型或名称标注，例如 `DevicePixels`、`PdfPoints`、`ByteOffset`、`GlyphId`。
- ID、handle、revision、generation、epoch 不得互用裸整数；必须使用不同新类型。
- `mod.rs` 只做模块声明和小型 re-export；实质实现放入职责明确的文件。
- 公共 re-export 必须保持最小，避免把第三方类型或内部实现暴露为公共契约。

布尔参数会隐藏调用语义时必须改用 enum 或配置结构：

```rust
// 禁止
render(page, true, false);

// 推荐
render(page, RenderOptions {
    quality: Quality::Preview,
    annotations: AnnotationPolicy::Exclude,
});
```

## 5. 类型、不变量与状态机

- 使非法状态尽量无法构造。已验证对象与未验证输入必须使用不同类型。
- 跨阶段对象应表达状态，例如 `UnvalidatedEnvelope` → `ValidatedEnvelope`。
- 有限状态使用 enum，不使用多个可能互相矛盾的 bool。
- 构造函数必须建立并记录类型不变量；绕过构造函数的反序列化必须重新验证。
- 公共 enum 新增 variant 视为协议/API 变更，必须检查所有匹配点和版本策略。
- 缓存键必须包含影响结果的全部 identity、revision、profile、environment 和 renderer epoch。

解析器、writer、Range、request 和 surface 生命周期必须使用显式状态机，禁止依靠“通常按这个顺序调用”的隐式约定。

## 6. 错误处理

- 可预期失败使用 `Result`、`CapabilityDecision` 或协议事件，不使用 panic。
- 产品代码不得使用无说明的 `unwrap()`、`expect()`、`todo!()`、`unimplemented!()`。
- 只有已经由类型系统或同一函数内检查证明的不变量才可使用 `expect()`，消息必须说明不变量，而不是复述失败。
- 错误必须保留稳定 `code`、`category`、`recoverability` 和 `diagnostic_id`；不得只返回动态字符串。
- 低层错误转换不得丢失 object、stream、page、byte offset 等定位信息。
- `Unsupported`、损坏输入、资源超限、取消和内部错误必须区分。
- 不得把错误吞掉后返回空白页面、空文本或部分未标记结果。
- 不得通过 PDFium 或其他外部引擎重试产品请求。

错误消息供开发者诊断，错误码供程序决策。调用方不得解析错误字符串决定行为。

## 7. 所有权、并发与锁

- 优先不可变值和消息传递；共享可变状态必须有明确 owner。
- 禁止 `Arc<Mutex<EntireDocument>>` 或等价的文档级大锁。
- 锁的保护对象、顺序和最大持有范围必须可从类型或注释中看出。
- 持锁期间不得执行宿主回调、网络、文件 I/O、codec/FFI、阻塞等待或发送可能反压的消息。
- 不得跨 `.await`、yield、Wasm/JS 边界或 IPC 调用持锁。
- channel 和队列必须有容量、溢出策略和关闭语义；禁止无界生产者。
- 原子内存序必须附不变量说明；不能以 `SeqCst` 掩盖不清楚的并发设计。
- 后台任务必须属于 session/job scope，禁止无 owner 的 detached task。

所有长任务必须接受取消 token 和预算 scope；循环检查频率遵守安全预算规范。

## 8. Async、回调与调度

- 核心 parser 保持可暂停的同步状态机，不依赖具体 async runtime。
- runtime 负责等待 Range、调度 job、取消和重新入队；数据到达回调不得直接恢复深层 parser 栈。
- 回调不得在内部锁、FFI 栈或网络回调中同步调用宿主。
- request 完成事件可以乱序；任何依赖顺序的逻辑必须显式使用 sequence、generation 或 dependency。
- 取消是正常控制流，不得记录为 error 级别或触发 crash reporting。
- 任务完成前必须再次验证 session、request 和 viewport generation。

## 9. 数值、内存与不可信输入

- 文件偏移、对象长度和累计字节使用显式宽度，通常为 `u64`。
- `start + len`、`stride * height`、像素数量、分配大小和窄化转换必须使用 checked arithmetic。
- 输入声明的长度只能作为候选值；分配前必须验证上限、可用数据和预算。
- 禁止按不可信计数直接 `with_capacity` 或一次性分配。
- 浮点 NaN、无穷、`-0` 和奇异矩阵必须有明确 canonical/错误策略。
- 递归处理对象、资源、page tree、Form、pattern、mask 和函数时必须检测环并扣减深度预算。
- 敏感内存应缩短生命周期；密码、解密密钥和用户文本不得出现在 Debug 输出。

## 10. `unsafe`、FFI 与平台 handle

新 `unsafe` 代码必须同时满足：

- 不能以合理成本用安全 Rust 实现；
- 限制在最小模块和最小语句范围；
- 紧邻 `// SAFETY:` 注释列出调用者与实现者不变量；
- 有越界、空指针、对齐、别名、生命周期和 panic 边界测试；
- 由第二名熟悉 Rust 内存模型的 reviewer 审查。

FFI 入口必须验证长度、枚举、对齐、handle generation 和所有权。panic 不得穿越 FFI/Wasm/IPC 边界。外部 handle 必须封装为拥有明确 acquire/release 语义的类型，不得在日志中输出原始值。

可行时，unsafe/FFI 模块应纳入 Miri、sanitizer、fuzz 或平台验证层。

## 11. 日志、诊断与隐私

- 使用结构化字段，不使用拼接的大段自由文本承载机器语义。
- 每个跨线程请求至少携带 `session_id`、`request_id`、`generation` 和 `diagnostic_id` 中适用字段。
- 默认不得记录 PDF 字节、文本、文件名、完整 URL、密码、注释和表单内容。
- 不记录裸指针、密钥、共享内存内容和平台 handle。
- 高频循环内日志必须采样或聚合，不能改变性能测试结论。
- `Internal` 错误面向用户只暴露稳定 code 与 diagnostic ID；内部细节进入脱敏诊断包。

## 12. 依赖与 feature flag

- 新依赖必须完成语义 ownership、许可证、来源、预算、取消、Wasm/Native、维护状态和替换计划审查。
- 只启用必要 features；默认 features 必须显式审查。
- 不允许依赖携带完整 PDF/2D 引擎进入产品依赖图。
- 第三方类型不得成为稳定公共 API；必须由本项目 adapter 隔离。
- feature flag 用于构建能力或实验，不得改变同一协议字段的含义。
- 影响输出的 feature、字体、颜色、renderer 和算法版本必须进入 hash/epoch。

## 13. TypeScript 与浏览器宿主

- TypeScript 只承担 DOM、Worker、Canvas、事件、可访问性和 transport；不得实现 PDF 语义、字体映射、内容解释或 raster 算法。
- 禁止在协议边界使用未收窄的 `any`；外部消息必须经过生成的 runtime validator。
- discriminated union 必须穷尽处理；默认分支返回协议错误，不静默忽略 mandatory variant。
- ArrayBuffer、ImageBitmap、SharedArrayBuffer 和 OffscreenCanvas 必须明确 transfer/ownership。
- 监听器、Worker、observer 和定时器必须在 session close/unmount 时释放。
- UI 丢弃 stale generation 的结果，不依赖消息恰好按发送顺序到达。

## 14. 注释与公共文档

- 注释解释不变量、规范依据和“为什么”，不重复代码表面行为。
- PDF 特殊行为应引用 ISO 条款、勘误、Feature ID 或 research note。
- 公共类型和函数必须说明输入可信度、所有权、错误、预算、取消和线程语义。
- `unsafe`、协议兼容、缓存 identity 和 canonicalization 规则必须有就近说明。
- TODO 必须带 issue/owner 或明确删除条件；禁止无期限 TODO 作为错误处理。

## 15. 变更与评审清单

PR 作者必须确认：

- [ ] 依赖方向和 Native-only 产品边界未被破坏。
- [ ] 新循环、递归、解码和分配接入预算与取消。
- [ ] 错误、unsupported 和取消未被转换为空白或静默成功。
- [ ] 新公共类型、协议字段、缓存键和 epoch 已完成兼容性评估。
- [ ] 新 unsafe/FFI 具有安全不变量和相应测试。
- [ ] 日志与诊断不包含文档内容或秘密。
- [ ] 测试、provenance 和规范映射按对应规范更新。
- [ ] 格式化、lint、单测和相关集成测试通过。

偏离本规范必须在 ADR 或 PR 的 `Standard deviation` 小节中记录规则、原因、风险、补偿措施、owner 和到期条件。永久偏离应修改规范或形成 ADR，不得靠口头约定长期存在。
