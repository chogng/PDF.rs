# PDF.rs Engine 交互协议规范

- 文档编号：RPE-PROTO-001
- 版本：0.1
- 协议版本：`0.1`
- 状态：稳定态开发基线（持续迭代）
- 适用范围：浏览器主线程 ↔ Engine Worker、桌面 Host ↔ Engine Worker、同进程 EnginePort adapter

## 1. 目标与边界

本协议定义宿主与 Native PDF engine 之间的命令、事件、版本、所有权和错误语义。它必须在浏览器 structured clone/transfer 和桌面二进制 IPC 上表达同一逻辑契约。

协议不定义：

- PDF 语义本身；
- UI 布局和产品交互样式；
- 外部 baseline runner 协议；
- 具体桌面 IPC 框架或浏览器 bundler；
- 第三方 PDF 引擎兼容接口。

产品协议只连接本项目 Native engine。PDFium 等外部 baseline 不得实现 EnginePort 或接收产品请求。

## 2. 设计原则

- 命令与事件异步；不得假定调用返回即操作完成。
- 每个 Request 有唯一终态。
- 消息可以乱序到达；通过 ID、sequence 和 generation 关联。
- close、cancel、release 幂等。
- 所有消息在访问 payload 前验证 envelope。
- 跨边界只传 opaque handle 和可验证数据，不传原生指针。
- 所有权转移、反压、取消和资源回收必须显式。
- Unsupported、错误、取消和 ResourceLimit 不能静默转换为成功。

## 3. 协议事实来源与生成

仓库应维护单一 canonical IDL/schema，并由它生成：

- Rust command/event 类型；
- TypeScript 类型与 runtime validator；
- 桌面 binary codec；
- 消息 ID 注册表；
- schema hash 和兼容性测试向量；
- 协议文档中的字段表。

禁止手工维护彼此独立的 Rust/TypeScript/IPC 字段定义。生成器版本和 schema hash 必须进入构建与发布证据。

## 4. Transport 抽象

```rust
pub trait EnginePort: Send + Sync {
    fn submit(&self, command: CommandEnvelope) -> Result<(), ProtocolError>;
    fn try_recv(&self) -> Option<EventEnvelope>;
    fn wake_handle(&self) -> WakeHandle;
}
```

- `submit` 成功只表示消息被 transport 接受，不表示 command 完成。
- Event 通过 queue/port 接收，禁止隐式全局回调。
- 回调不得在 engine 锁、网络回调、codec/FFI 或 driver 栈内同步调用宿主。
- Browser adapter 使用 `postMessage` 与 transfer list；Desktop adapter 使用认证 IPC 和显式 handle transfer。

## 5. 握手

```rust
pub struct ProtocolHello {
    pub major: u16,
    pub minor: u16,
    pub schema_hash: [u8; 16],
    pub endpoint_role: EndpointRole,
    pub capabilities: EndpointCapabilities,
    pub max_message_bytes: u32,
    pub max_transfer_slots: u16,
}
```

握手流程：

```text
Host                         Engine
 │──── Hello(host) ──────────►│
 │◄─── Hello(engine) ─────────│
 │──── HelloAccept ───────────►│
 │◄─── Ready(worker_id) ──────│
```

- major 不同：拒绝连接。
- major 相同且对端 minor 满足 mandatory capability：允许连接。
- schema hash 相同：启用精确 schema 快路径。
- schema hash 不同：只能按同 major 的兼容规则继续；unknown mandatory capability 时拒绝。
- 协商完成前只允许握手、关闭和协议错误消息。
- 每次 Worker 启动生成新的 `WorkerId`/epoch。

`EndpointCapabilities` 只描述 transport/host 能力，例如 OffscreenCanvas、transferable ImageBitmap、SharedArrayBuffer、shared memory、GPU handle；不表示 PDF feature 支持。PDF feature 由 CapabilityProfile/CapabilityDecision 表达。

## 6. Envelope

逻辑 envelope 至少包含：

```rust
pub struct EnvelopeHeader {
    pub major: u16,
    pub minor: u16,
    pub message_type: u16,
    pub flags: u16,
    pub payload_len: u32,
    pub sequence: u64,
}

pub struct Correlation {
    pub worker: WorkerId,
    pub session: Option<SessionId>,
    pub request: Option<RequestId>,
    pub generation: Option<u64>,
}
```

验证顺序：

1. 固定 header 长度和 transport frame 长度；
2. major/minor、message type、flags；
3. `payload_len` 与实际长度、消息级上限；
4. sequence/worker/session/request/generation 形状；
5. transfer slot 数量和类型；
6. payload schema、枚举、字符串、数组和 handle；
7. command 的状态机前置条件。

任何乘加、offset/len 和 allocation 都使用 checked arithmetic。未知 mandatory 字段/variant 返回协议错误；未知 optional 字段按 minor 兼容规则忽略并保留诊断计数。

## 7. ID 与 sequence

- `WorkerId` 在 Worker epoch 内固定，重启后改变。
- `SessionId` 在同一 Worker epoch 内不重用。
- `RequestId` 由 command 发起方生成，在 session 内唯一。
- `DataTicket`、`SurfaceId` 和 ChangeSet revision 由 owner 生成。
- 每个发送方向维护单调递增 `sequence`；它用于检测重复/倒退和调试，不保证跨方向全序。
- transport 可以乱序传输时，接收端仍按 command/event 语义处理；不得仅按 sequence 排序后假装依赖成立。
- ID/epoch 不匹配的迟到消息丢弃，并记录脱敏协议指标。

## 8. Command 集

| Command | Session 状态 | Request | 说明 |
| --- | --- | --- | --- |
| `Open` | 无 | 必须 | 绑定不可变 source descriptor，创建 session |
| `ProvideData` | Opening/Ready | 不创建；关联 ticket | 提供 Range bytes 或 source error |
| `SubmitPassword` | WaitingForPassword | 不创建；关联 open request/challenge | 提交短生命周期 secret |
| `SetViewport` | Ready | 不创建；使用 generation | 更新 generation 和可视页请求 |
| `RequestText` | Ready | 必须 | 请求指定 page/revision 文本 |
| `Search` | Ready | 必须 | 发起/继续/结束搜索 |
| `ApplyChanges` | Ready | 必须 | 应用带 base revision 的 ChangeSet |
| `Save` | Ready | 必须 | 保存固定 revision snapshot |
| `Cancel` | 非终态 Request | 不创建；指定目标 Request | 幂等取消 |
| `ReleaseSurface` | Surface 存活 | 不创建 | 幂等释放/ack transfer |
| `CloseSession` | 非 Closed | 不创建 | 幂等关闭 session |
| `Shutdown` | Worker Ready | 不创建 | 有界 drain 后关闭 Worker |

每个 command schema 必须声明：最大 payload、是否可重放、所有权、状态前置条件、预算、敏感字段和唯一终态。

## 9. Event 集

| Event | 是否终态 | 说明 |
| --- | --- | --- |
| `CommandAccepted` | 否 | 可选 ack；不表示完成 |
| `NeedData` | 否 | Range ticket 与缺失区间 |
| `PasswordRequired` | 否 | 需要密码，不回传 secret |
| `Progress` | 否 | 可丢弃/合并的进度 |
| `DocumentReady` | 是（Open） | open 成功，包含 revision/metadata 摘要 |
| `CapabilityReported` | 否/随请求 | CapabilityDecision 与缺失 requirement |
| `SurfaceReady` | 否 | Native Surface 与 generation；属于 session/generation stream |
| `TextReady` | 是（RequestText） | 文本结果或 handle |
| `SearchBatch` | 否 | 可多批，包含 batch sequence |
| `SearchComplete` | 是（Search） | 搜索结束 |
| `ChangesApplied` | 是（ApplyChanges） | 新 working revision |
| `SaveComplete` | 是（Save） | 输出 identity/hash/bytes 摘要 |
| `RequestCancelled` | 是 | request 唯一取消终态 |
| `RequestFailed` | 是 | 稳定 EngineError |
| `SurfaceReclaimed` | 否 | 超时/close 后 handle 失效通知 |
| `SessionClosed` | 是（Session） | session 不再产生新事件 |
| `WorkerStopped` | 是（Worker） | 正常 shutdown 后 Worker epoch 终止 |
| `WorkerFault` | Worker 终态 | Worker epoch 失效 |

同一 Request 只能出现一个终态。`SetViewport` 是 generation 更新，不创建 Request；其 `SurfaceReady` 属于 session/generation stream，Host 不得依赖“最后一张图”的隐式概念。当前 generation 是否达到目标质量由显式 viewport progress/coverage 字段表达。

## 10. Open 与 Range 流程

```text
Host                                      Engine
 │── Open(req=1, source descriptor) ──────►│
 │◄─ NeedData(req=1, ticket=A, ranges) ────│
 │── ProvideData(ticket=A, snapshot, bytes)►│
 │◄─ NeedData(req=1, ticket=B, ranges) ────│
 │── ProvideData(ticket=B, ...) ───────────►│
 │◄─ DocumentReady(req=1, session, rev) ───│
```

- `Open` source descriptor 不授予 Core 任意网络/文件权限；Host/source service 负责读取。
- `NeedData` 包含 `SourceIdentity`、ticket、ranges、priority 和 checkpoint opaque identity。
- `ProvideData` 必须引用相同 source snapshot，并验证 bytes 恰好覆盖声明范围。
- ticket 只完成一次；重复 completion 返回 `DuplicateTicketCompletion`。
- source validator 改变时 Host 发送 source error，Engine 以 `SourceChanged` 终止 session。
- Data arrival 只重新入队 job，不在 command 解码栈内执行 parser。

## 11. Password 流程

```text
Engine ── PasswordRequired(open_req, challenge, attempt, policy) ──► Host
Engine ◄─ SubmitPassword(open_req, challenge, session, secret) ──── Host
```

- Secret 通过 transport 的敏感字段机制传递，不可 Debug、trace 或持久化。
- `SubmitPassword` 是 pending Open 的 continuation，不创建新 Request；challenge 只能消费一次，并受有界 attempt policy 控制。
- PasswordRequired 不包含可用于离线泄露的内部派生数据。
- close/cancel 后晚到 secret 必须立即丢弃并清理。

## 12. Viewport 与 Surface 流程

`SetViewport` 必须包含完整 generation、visible pages、geometry、quality、output profile、DPR、rotation 和 optional-content identity。

```text
Host ── SetViewport(gen=42) ──────────────────────► Engine
Host ── SetViewport(gen=43) ──────────────────────► Engine
Host ◄─ SurfaceReady(gen=42, surface=S1) ───────── Engine  # stale
Host ── ReleaseSurface(S1) ───────────────────────► Engine
Host ◄─ SurfaceReady(gen=43, surface=S2) ───────── Engine  # display
Host ── ReleaseSurface(S2) ───────────────────────► Engine
```

- Engine 和 Host 都必须检查 generation。
- Stale Surface 不显示，但仍必须 release/ack。
- 所有已发布 Surface 来自同一 Native Scene/RenderConfig；不允许拼接外部输出。
- GPU 失败可以复用 Native Scene，以新的 RenderConfigHash/RendererEpoch 生成 Fast CPU Surface，并明确 backend/epoch。

## 13. Surface transport

```rust
pub struct SurfaceMetadata {
    pub id: SurfaceId,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
    pub alpha: AlphaMode,
    pub render_config: RenderConfigHash,
    pub renderer_epoch: u32,
}

pub enum SurfaceTransport {
    OffscreenCanvasCommit { canvas: CanvasId },
    BrowserTransfer { slot: TransferSlot, kind: BrowserTransferKind },
    SharedMemory { handle: PlatformHandle, offset: u64, len: u64 },
    GpuTexture { handle: PlatformGpuHandle, backend: GpuBackend },
}
```

规则：

- `BrowserTransfer.slot` 必须指向同一 `postMessage` 的实际 transfer list 项。
- 裸 Wasm pointer 不得跨 Worker/realm；同 Worker local surface 额外验证 `memory_epoch`。
- SharedArrayBuffer 只在握手协商且浏览器跨源隔离满足时使用。
- Desktop handle 必须验证权限、长度、backend 和 session owner。
- `stride * height`、`offset + len` 和 format 在消费端重验。
- acquire/transfer/release/reclaim/close 状态遵守生命周期规范。

## 14. Text 与 Search

- Text 结果绑定 source identity、document revision、page、text schema 和 extraction profile。
- 大结果可以使用 chunk/handle，但必须有总量上限、chunk sequence、完成和 release。
- Search query 是敏感字段，默认不写日志。
- SearchBatch 可乱序到达不同 page，但同一 query 内提供 batch/page 序号和稳定 range identity。
- Cancel 后不得再发布新 batch；已在 transport 中的 batch 由 Host 按 Request 状态丢弃。
- Text/Search 的 Unicode 和几何 schema 版本独立于 wire protocol minor，改变语义时必须升级 schema/epoch。

## 15. ChangeSet 与 Save

- `ApplyChanges` 包含 base revision；不匹配返回 `RevisionConflict`。
- `ChangesApplied` 返回新的 working revision 和受影响 page/resource 摘要。
- `Save` 固定开始时的 revision snapshot；之后的变更不混入本次保存。
- Save target 是宿主授予的 opaque sink/capability，不是 PDF 可控路径。
- `SaveComplete` 返回写入字节数、输出 hash/identity 和签名状态摘要；不返回秘密或路径。
- Save 失败保持未保存 ChangeSet，可由宿主重试或导出 sidecar。

## 16. Cancel、Close 与 Shutdown

### 16.1 Cancel

- Cancel 幂等；未知或已终态 Request 返回稳定 ack/状态，不创建新错误风暴。
- Completion 与 cancel 由原子终态提交裁决，只能发布一个终态。
- 取消一个 Request 不必取消仍被其他 Request 共享的 Range/cache work。

### 16.2 CloseSession

- 收到后拒绝新 command，取消未终态 request，使 Surface 进入回收/失效。
- `SessionClosed` 之后不得再发布该 session 的事件。
- 晚到的 command/event/handle 按 stale 处理。

### 16.3 Shutdown

- Worker 进入 draining，拒绝新 Open。
- 在协商 deadline 内完成或取消 session，然后发送 `WorkerStopped`。
- 超时后 Host 可以终止 worker；新 Worker 必须使用新 epoch。

## 17. CapabilityDecision 与 EngineError

`CapabilityDecision` 描述产品能力范围；`EngineError` 描述本次操作为何未完成。二者可以同时出现，但不能混为一条自由文本。

```rust
pub enum SupportStatus {
    Supported,
    Unsupported,
    Rejected,
}

pub struct CapabilityDecision {
    pub status: SupportStatus,
    pub profile: CapabilityProfileId,
    pub policy_version: u32,
    pub missing: Vec<CapabilityRequirement>,
    pub scope: CapabilityScope,
}
```

```rust
pub struct EngineError {
    pub code: ErrorCode,
    pub category: ErrorCategory,
    pub severity: Severity,
    pub recoverability: Recoverability,
    pub location: Option<ErrorLocation>,
    pub diagnostic_id: DiagnosticId,
}
```

- `Unsupported` 不调用 baseline。
- `ResourceLimit` 不重新分配第二份外部引擎预算。
- `Cancelled` 是正常终态。
- `RetryNativeRenderer` 只允许 Native GPU → Native Fast CPU 等内部后端变化。
- Host 只能按 code/category/recoverability 决策，不解析错误字符串。

## 18. 反压与消息优先级

Queue 必须有容量和水位。建议优先级：

| 优先级 | 消息 |
| --- | --- |
| Critical | Close、Shutdown、Cancel、RequestFailed、WorkerFault、ReleaseSurface |
| High | NeedData、PasswordRequired、completion、CapabilityReported |
| Normal | SurfaceReady、TextReady、SearchBatch |
| Low | Progress、prefetch hint、telemetry |

- Progress、连续 viewport command 和低价值预取可以合并/替换。
- 终态、error、close、cancel、release 不得静默丢失。
- 队列满必须产生可观察 backpressure，不允许无限内存增长。
- 大 payload 使用 transfer/shared memory/chunk，不堵塞 control channel。
- Host 长期不 release Surface 时，Engine 按 lease 回收并发出 `SurfaceReclaimed`。

## 19. Browser 映射

- 每个 structured clone 对象通过生成 validator。
- transfer list 与 schema slot 一一对应；重复 transfer 视为协议错误。
- `messageerror`、Worker error 和 termination 映射为 transport/WorkerFault。
- CSP、service worker cache 和 network manifest 只包含产品 Native 资源。
- TS host 不实现 PDF 语义，只负责 DOM/Canvas/事件/a11y/transport。
- Event listener、MessagePort、Worker、observer 和 Surface 在 close/unmount 时释放。

## 20. Desktop 映射

- IPC 连接认证对端，frame 有长度上限和版本。
- OS handle 使用带外传输时，schema 中的 slot 与实际 handle 表一致。
- Worker 只获得宿主授予的最小文件/共享内存/GPU capability。
- 断连使 Worker/session 进入明确状态；不得无限等待 orphan handle。
- IPC codec panic/exception 不得跨进程边界，转换为协议故障并隔离 worker。

## 21. 版本演进

Minor 版本可以：

- 增加 optional 字段；
- 增加通过 capability 协商启用的消息/variant；
- 放宽接收端但不改变旧字段含义。

Major 版本必须用于：

- 删除/重解释 mandatory 字段；
- 改变 ownership、终态或错误语义；
- 改变 ID 唯一范围；
- 改变无法通过能力协商兼容的 wire encoding。

规则：

- 发送端不得在未协商时发送新 mandatory variant。
- 接收端忽略 unknown optional 字段，但保留大小和资源上限。
- 每次 schema 变化生成兼容矩阵和 old/new round-trip vectors。
- 支持窗口和废弃日期由 release policy 定义；不得永久保留无测试旧协议。

## 22. 可重放与诊断 trace

协议 trace 默认记录 envelope metadata、message type、ID、sequence、大小、耗时、错误/能力 code 和脱敏环境。不得记录 PDF bytes、文本、密码、query、注释或平台 handle。

测试环境可以生成内容寻址的 replay bundle；包含敏感 payload 时必须使用授权、加密和保留策略。Replay 必须能注入乱序、重复、丢包、断连和延迟。

## 23. 必测协议场景

- 握手 major mismatch、schema mismatch、unknown mandatory capability。
- frame 截断、超长、未知 message type、非法枚举和 transfer slot。
- Open/NeedData/ProvideData 的乱序、重复、SourceChanged。
- PasswordRequired 期间 cancel/close 和晚到 secret。
- viewport generation 快速变化与 Surface 乱序。
- completion/cancel 竞争和唯一终态。
- Surface 重复 release、lease reclaim、Wasm memory epoch、桌面 stale handle。
- queue 满时 critical 消息仍可送达。
- Worker crash/restart 后旧 epoch 消息到达。
- old/new minor 的双向兼容和生成 validator 一致性。
- 敏感字段不进入 trace、日志或错误字符串。

## 24. 评审清单

- [ ] schema 是 Rust/TS/Desktop codec 的单一事实来源。
- [ ] 新 command/event 定义了状态前置条件、owner、预算和唯一终态。
- [ ] 消息长度、枚举、ID、handle、slot 和 payload 均被验证。
- [ ] 乱序、取消、close、重启和 stale generation 有明确结果。
- [ ] Surface 和大 payload 的 acquire/transfer/release/反压完整。
- [ ] CapabilityDecision、EngineError 和取消没有混淆。
- [ ] 版本变化符合 major/minor 和 capability 协商规则。
- [ ] Browser/Desktop adapter 不承载 PDF 语义。
- [ ] 协议不包含外部 PDF 引擎路径。
- [ ] 兼容、fuzz、生命周期和隐私测试已更新。
