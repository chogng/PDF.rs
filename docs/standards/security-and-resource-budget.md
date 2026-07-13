# PDF.rs 安全与资源预算规范

- 文档编号：RPE-STD-005
- 版本：0.1
- 状态：稳定态开发基线（持续迭代）
- 适用范围：所有处理 PDF、协议消息、平台 handle、字体、图片、颜色、保存和诊断的代码

## 1. 安全目标

所有 PDF、字体、图片、颜色 profile、Range 响应、IPC/Wasm 消息和持久化缓存均视为不可信输入。引擎必须：

- 不发生内存破坏、越权文件/网络访问或跨 session 数据泄漏；
- 对 CPU、内存、递归、解压、像素、GPU 和句柄使用设置硬边界；
- 在恶意输入下可取消、可终止、可分类失败；
- 不执行 PDF 内嵌 JavaScript、命令或未经宿主批准的外部动作；
- 不把密码、文档内容和用户标识写入默认日志或遥测；
- 不通过 PDFium 或其他外部引擎重试超限/失败的产品请求。

安全边界优先于兼容性。宽容修复只能恢复经过验证的结构，不能放宽资源、权限和完整性检查。

## 2. 信任边界

```text
Untrusted PDF / Range / font / image / ICC
                  │
                  ▼
       Native parser + budget checks
                  │ immutable validated types
                  ▼
           Scene / Text / Writer
                  │
          bounded renderer/runtime
                  │ validated protocol
                  ▼
         Browser/Desktop host UI
```

外部 baseline 位于独立开发/CI 进程边界，只处理固定 hash、获授权的测试对象；不属于产品信任链。

### 2.1 Host 的职责

Host 负责授予文件/网络/共享内存/GPU handle、执行用户交互、实施平台沙箱和安全更新。Core 不自行访问任意文件系统或网络。

### 2.2 Engine 的职责

Engine 负责验证 PDF 与协议数据、实施 deterministic fuel 和 runtime limit、隔离 codec/FFI、产生稳定错误与脱敏诊断。

## 3. 威胁模型

| 威胁 | 主要控制 |
| --- | --- |
| 越界、UAF、类型混淆 | 安全 Rust、checked arithmetic、handle generation、最小 unsafe |
| CPU DoS | FuelBudget、递归/操作符/路径上限、取消、watchdog |
| 内存 DoS | 分层内存预算、解压/像素/Surface/cache 计费 |
| 解压炸弹 | 输入/输出比、累计输出和 codec 隔离 |
| 对象/资源循环 | visited set、深度预算、状态机 |
| Range 混合攻击 | 不可变 SourceSnapshot、强 validator、SourceChanged 终止 |
| 恶意 action/JavaScript | 默认不执行、结构化 Unsupported、宿主 allowlist |
| 文件/网络越权 | Core 无 I/O 权限、宿主 capability、桌面沙箱 |
| IPC/Wasm 伪造 | schema validator、长度/枚举/handle/epoch 校验 |
| GPU 驱动/资源风险 | 输入验证、资源预算、context epoch、CPU Native 重试 |
| 密码/内容泄漏 | Secret 生命周期、日志脱敏、最小 crash bundle |
| 跨租户缓存泄漏 | 完整 identity、session scope、秘密不持久化 |
| 供应链攻击 | lock、来源/hash、SBOM、许可证、漏洞扫描、最小 features |

## 4. 预算模型

```rust
pub struct FuelBudget {
    pub max_input_bytes: u64,
    pub max_objects: u64,
    pub max_resolve_depth: u32,
    pub max_stream_output_bytes: u64,
    pub max_total_decode_bytes: u64,
    pub max_image_pixels: u64,
    pub max_font_bytes: u64,
    pub max_path_segments: u64,
    pub max_scene_commands: u64,
    pub max_group_depth: u32,
    pub operator_fuel: u64,
    pub decode_fuel: u64,
    pub schedule: FuelScheduleVersion,
}

pub struct RuntimeLimits {
    pub max_intermediate_surface_bytes: u64,
    pub max_resident_bytes: u64,
    pub watchdog_deadline: Deadline,
    pub cancellation_check_interval_fuel: u32,
}
```

预算分为：

- **FuelBudget**：与机器速度无关的确定性语义限制；
- **RuntimeLimits**：内存、句柄、队列和 wall-clock watchdog；
- **Platform sandbox**：进程级 CPU/内存/文件/网络/handle 限制。

三者都必须存在，互不替代。

## 5. Budget 层级与记账

```text
Global
└── Worker
    └── Session
        ├── Page
        │   └── Job
        └── Codec / GPU / Writer child scope
```

- 子 scope 从父 scope 领取配额，不得凭空扩大预算。
- 未使用配额可以归还；已消耗 fuel 不回滚。
- 共享资源按真实拥有者计费，并对使用者保留引用成本，避免重复免费占用。
- cache、GPU、Surface、Range backing data 和失败 artifact 都必须计费。
- 预算检查必须在执行/分配之前完成。
- 聚合计数使用 checked arithmetic；溢出按超限处理。
- 每次超限生成 budget kind、limit、consumed、scope 和安全位置摘要。

## 6. FuelSchedule

每个 token、对象、引用边、操作符、path segment、glyph、pixel/coverage 单位、codec 输出单位、颜色函数求值和递归边都必须按版本化 `FuelSchedule` 扣费。

- 同一输入、CapabilityProfile 和 FuelSchedule 在不同机器上应产生相同 fuel 结果。
- Schedule 变化必须生成新版本，并对 release corpus 重跑。
- Fuel 在工作发生前扣减；不能先分配/解码再发现超限。
- 可批量扣费，但批量大小必须保证取消与超限延迟符合门槛。
- wall-clock 不得作为 O0-O3 预期错误，因为其受机器和负载影响。

## 7. 取消与 watchdog

- 取消 token 与 fuel 分离；有 fuel 不代表可以忽略取消。
- 所有潜在长循环最多在 `cancellation_check_interval_fuel` 后检查一次取消。
- codec/FFI 若不能增量检查，必须在可终止 worker/process 中运行，并设置输入、输出、内存和 deadline 上限。
- watchdog 用于捕获实现 bug、driver/FFI 卡死和环境异常；触发后允许终止 worker。
- watchdog 终止后宿主可以重建 Native session，但不得自动把请求交给外部 PDF 引擎。
- `Cancelled`、`ResourceLimit` 和 `FatalInternal` 必须保持不同分类。

## 8. 字节、Range 与完整性

- 所有 offset/length 使用显式宽度和 checked arithmetic。
- `SourceSnapshot` 在 session 内不可变，并绑定 stable identity、length 和 validator。
- HTTP 使用 strong ETag/If-Range 或等价强验证；弱 validator 必须冻结为不可变响应 snapshot。
- validator 变化返回 `SourceChanged` 并终止 session；禁止拼接不同 revision 字节。
- Range 响应验证状态码、Content-Range、长度、总长和请求区间。
- 线性化 hint 只作优化，所有 offset/reference/length 重新验证。
- 不支持 Range 时可以完整下载，但仍受输入、内存和下载策略上限。

## 9. Parser、对象图与修复

- lexer/parser 对每个 token、字符串、name、array/dictionary 和 stream 长度设上限。
- 对象、page tree、name tree、resource、Form、pattern、mask 和函数递归必须检测循环。
- xref/revision 链设置对象数量、section 数、深度和扫描字节上限。
- Repair 只在明确 policy 下运行，并记录被修复结构和成本。
- 严格失败、宽容恢复和 Unsupported 必须分开。
- 修复后对象仍必须通过常规验证；不得把“能找到对象”当作可信。
- 超限、损坏或内部错误不得返回未标记的部分页面。

## 10. Stream、图片、字体与颜色 codec

每个 codec adapter 必须提供：

- 可验证的输入边界；
- 单 stream 输出上限和 session 累计输出上限；
- 预测输出尺寸时的 checked arithmetic；
- 取消/预算 hook 或可终止隔离；
- panic/exception/FFI 错误转换；
- 最小 feature 配置和版本锁定；
- fuzz、恶意 corpus 和漏洞响应 owner。

额外要求：

- 图片在分配前验证 width、height、components、bits、stride 和总 pixels。
- 字体 table/offset/count、charstring/CMap 递归和 glyph outline 段数受限。
- ICC/颜色函数的通道数、table、采样和求值次数受限。
- 逐层 filter 链按每层和累计输出记账，防止多层解压放大。
- 第三方 codec 不得携带 PDF 对象模型或完整渲染器进入产品。

## 11. Scene、Raster 与 GPU

- Scene command、path segment、clip/group/mask 深度、资源和 bounds index 均受限。
- 中间 Surface 按真实 stride、height、format 和并行存活数量计费。
- tile halo、滤镜和透明组不能绕过视口外内存预算。
- GPU buffer/texture/pipeline 按 session/global scope 计费。
- GPU command 的尺寸、offset、format 和资源状态必须在提交前验证。
- context lost 使绑定 epoch 的全部资源失效；可使用同一 Native Scene 以新 RenderConfig/epoch 切换 Fast CPU。
- 不得展示半完成、混合不同 Scene/RenderConfig 或混入外部 baseline 的 Surface。

## 12. Security Handler 与秘密

- 不自行设计加密原语；使用经审批的密码学叶子库。
- PDF Security Handler 的参数派生、对象边界和权限语义由本项目实现并测试。
- 密码和派生密钥使用短生命周期 Secret 类型，禁止 Clone/Debug/序列化，能清零时清零。
- 错误、trace、crash、遥测、URL 和浏览器 console 不得包含密码或解密后内容。
- 密码重试必须限速并由宿主交互；engine 不持久化密码。
- 权限标志不等同于安全沙箱授权；产品行为由宿主 policy 决定。

## 13. Actions、JavaScript 与外部资源

- PDF JavaScript、Launch、RichMedia、XFA 动态脚本等默认 `Unsupported`，不得执行。
- URI、GoToR、附件和外部资源只解析为结构化 metadata；执行必须经过宿主用户交互和 allowlist。
- Core 不发起网络请求，不读取任意路径，不启动进程。
- URI 显示、规范化和打开分离，防止混淆与 scheme 绕过。
- 嵌入文件提取必须验证名称、大小、类型并由宿主选择目标；不得路径穿越或自动执行。

## 14. Writer 与保存安全

- Writer 只消费已验证 ChangeSet 和固定 base revision snapshot。
- 所有 offset、object number、xref、length 和累计输出使用 checked arithmetic。
- 保存到新目标或宿主提供的原子写句柄；失败不得破坏原文件。
- 增量保存保持未修改字节，且不得在不知情时使签名状态看似有效。
- 重新打开保存结果并验证对象图、ChangeSet 语义和预算。
- 文件名、路径和权限由宿主管理；PDF 内容不能选择任意目标路径。

## 15. IPC、Wasm 与浏览器

- 所有消息先验证 envelope，再解码 payload。
- 验证 major/minor、schema hash、message type、flags、payload length、sequence、transfer slots 和 mandatory capability。
- TypeScript 静态类型不替代 runtime validator。
- 跨边界使用 opaque handle + generation，不传原生指针。
- Wasm pointer 只在同 Worker、同 memory epoch 使用；跨 realm 必须复制/transfer 或使用已协商 SharedArrayBuffer。
- SharedArrayBuffer 只在跨源隔离满足时启用。
- CSP/network/service-worker manifest 不得包含外部 PDF 引擎或动态下载路径。
- command/event 队列有容量和反压，关键 close/cancel/release/error 事件不得丢失。

## 16. 桌面沙箱

- Engine worker 仅持有宿主授予的文件、共享内存和 GPU handle。
- 默认无任意网络、文件系统、进程启动和设备权限。
- IPC 端点认证对端进程/会话，handle transfer 使用最小权限。
- 资源使用受 OS 进程/job/sandbox 限制；worker crash 不影响宿主完整性。
- 未保存 ChangeSet 由宿主持久化，不能只存在于受限 worker。
- 打包清单扫描确认没有 PDFium 或其他外部完整 PDF/2D 引擎。

## 17. 隐私与诊断

默认允许记录：

- 不可逆 source hash；
- 大小/页数/feature 分桶；
- 稳定错误、CapabilityDecision、budget kind 和 diagnostic ID；
- 脱敏性能与环境信息。

默认禁止记录：

- PDF 原始字节、提取文本、文件名、完整 URL/query；
- 密码、密钥、注释、表单值、附件；
- 原始内存、指针和平台 handle；
- 能稳定识别用户或私有文档的组合字段。

原始样例上传必须显式授权、访问控制、用途限制和保留期限。Crash bundle 默认只含最小技术摘要；需要文档时单独征得同意。

## 18. 供应链与依赖

- 锁定依赖版本和 source hash，生成 product/dev/test/tool/corpus 分账 SBOM。
- 新依赖完成许可证、维护、漏洞、budget、cancel、Wasm/Native、替换和 semantic owner 审查。
- 不启用不需要的默认 features。
- 漏洞扫描发现影响不可信输入处理的高风险问题时阻断 release。
- Vendored 数据/代码必须保留上游来源、许可证和修改说明。
- PDFium 只作开发/CI baseline；若分发包含它的工具或镜像，随分发物提供其实际许可证闭包。

## 19. 稳定安全错误

至少区分：

| 结果 | 含义 | Session 行为 |
| --- | --- | --- |
| `InvalidInput` | 输入违反语法/结构且不可恢复 | 按 scope 失败 |
| `SourceChanged` | source snapshot 完整性失效 | 终止 session |
| `Unsupported` | 能力不在 CapabilityProfile | 返回明确 scope，可保留安全 metadata |
| `ResourceLimit` | 确定性或 runtime 预算超限 | 终止 request/page，按 policy 保留 session |
| `Cancelled` | 用户/调度/close 取消 | 正常终态 |
| `PermissionDenied` | 宿主未授予动作/资源权限 | 不执行动作 |
| `Internal` | 不变量或实现故障 | 隔离 request 或终止 worker/session |

安全错误不得转换为空白成功，也不得触发外部引擎。

## 20. 安全测试基线

- 所有不可信 parser/codec/协议入口持续 fuzz。
- unsafe/FFI 运行 Miri、sanitizer 或等价平台检查。
- 每个 budget 字段测试边界、累计和并发分配。
- 测试压缩炸弹、超大尺寸、整数溢出、递归/循环、畸形 xref/font/image/ICC。
- 测试 SourceChanged、Range 欺骗、stale handle、重复 release、消息截断/未知 mandatory 字段。
- 测试 action/URI/附件不被自动执行或越权写入。
- 测试日志、crash 和 telemetry 中不存在敏感字段。
- Release 扫描依赖、二进制、Wasm imports、网络和安装 manifest。

## 21. 漏洞响应

- 安全问题使用私密渠道登记、分级和分配 owner。
- 保留最小复现、受影响版本、攻击面和是否包含敏感数据。
- 修复必须同时加入回归、fuzz seed、provenance 和 release note/公告判断。
- 评估所有受支持分支和已发布 artifact，不只修复主干。
- 在补丁可用前限制访问细节，发布后按政策披露。
- 来源/许可证事件按供应链事件处理，必要时停止再分发并重建 artifact。

## 22. 评审清单

- [ ] 所有输入和协议边界均按不可信处理。
- [ ] 计数、长度、offset、stride、pixels 和分配使用 checked arithmetic。
- [ ] 循环、递归、解码、raster、GPU 和队列受预算与取消控制。
- [ ] Fuel 结果确定性，watchdog 不被当作规范 oracle。
- [ ] secret、文档内容和用户标识未进入日志/遥测。
- [ ] action/外部资源只能由宿主授权执行。
- [ ] unsafe/FFI/codec 有隔离、测试和明确 owner。
- [ ] Writer/Range/缓存/handle 保持 snapshot 与 session 完整性。
- [ ] 超限或失败不会产生未标记部分结果或外部引擎重试。
- [ ] 供应链、许可证、SBOM 和发布扫描已更新。
