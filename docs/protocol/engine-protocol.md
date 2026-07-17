# PDF.rs Engine 交互协议规范

- 文档编号：RPE-PROTO-001
- 版本：0.2
- 协议版本：`0.2`
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
- 桌面 binary codec 字段注册表；
- 消息 ID 注册表；
- schema hash 和兼容性测试向量；

禁止手工维护彼此独立的 Rust/TypeScript/IPC 字段定义。协议文档示例必须依据生成的桌面字段注册表复核；Rust/TypeScript 常量、桌面字段注册表、hash registry 和 JSON 向量都必须携带同一生成器版本，生成器版本和 schema hash 必须进入构建与发布证据。

当前 canonical source 是 `protocol/engine.protocol`。在仓库根目录使用以下命令生成或校验全部派生文件：

```sh
cargo run --quiet --package pdf-rs-protocol-codegen -- generate .
cargo run --quiet --package pdf-rs-protocol-codegen -- --check .
```

生成命令原子更新以下受控输出；`--check` 只比较内容，不重写文件，CI 必须执行后者：

- `runtime/protocol/src/generated.rs`
- `platform/browser/generated/engine-protocol.ts`
- `platform/desktop/generated/engine-protocol.registry`
- `protocol/generated/schema-hash.txt`
- `protocol/generated/compatibility-vectors.json`
- `protocol/generated/invalid-vectors.json`

生成器对 canonical schema 的完整字节计算 SHA-256。完整 32 字节 digest 是构建、审计、兼容向量和发布证据中的 schema identity；握手中的 `schema_hash: [u8; 16]` 固定为该 digest 的前 16 字节，策略名固定为 `sha256-first-16-bytes`。它不是另一种 128 位 hash，也不得从非 canonical 输入单独计算。`SceneHash`、`RenderPlanHash`、`RenderConfigHash` 和 Surface 中的 `decision_hash` 始终保留完整 32 字节，不采用 wire 截断策略。

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

pub struct EndpointCapabilities {
    pub supported: u64,
    pub mandatory: u64,
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
- `supported` 是端点实际实现的 capability bit set；`mandatory` 是该端点要求对端必须支持的 bit set，且本端 `mandatory` 不得包含本端未支持的 bit；违反后者以 `InvalidEndpointCapabilities` 拒绝连接。
- 任一端点的 `mandatory` 含未知 bit：以 `UnknownMandatoryCapability` 拒绝连接。
- `local.mandatory & !peer.supported != 0` 或 `peer.mandatory & !local.supported != 0`：以 `MissingMandatoryCapability` 拒绝连接。
- 协商结果只能使用 `local.supported & peer.supported & known_capabilities`；未知但非 mandatory 的 supported bit 可以忽略，不得据此访问新 payload、handle 或 transport。
- major 相同、minor 位于生成的 compatibility window 且 capability 规则兼容时才允许连接。仓库没有可认证的 0.1 canonical schema/hash，因此协议 0.2 的当前生成窗口只包含 minor 2；旧 minor、未来 minor 和未注册组合都以 `UnsupportedMinor` 拒绝。只有未来把旧 schema 的精确 wire hash 纳入 canonical 生成注册表并加入双向重放后，窗口才可扩展。当前端点固定声明当前 minor，协商 minor 取 peer minor。
- wire schema hash 相同：启用精确 schema 快路径；它仍不能绕过 mandatory capability、endpoint role、消息大小或 transfer-slot 校验。
- wire schema hash 不同但 minor 不同：只能按同 major、精确旧 hash 的生成兼容注册继续；当前注册为空，因此拒绝。wire schema hash 不同且双方声明同一 minor 时同样拒绝。
- 协商完成前只允许握手、关闭和协议错误消息。
- 每次 Worker 启动生成新的 `WorkerId`/epoch。

`EndpointCapabilities` 只描述 transport/host 能力。协议 0.2 注册的 bit 是 OffscreenCanvas、transferable ArrayBuffer、transferable ImageBitmap、SharedArrayBuffer、shared memory 和同 Worker local memory；不表示 PDF feature 支持。PDF feature 只由 CapabilityProfile/CapabilityDecision 表达。

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

Desktop binary transport 的 `payload_len` 是 header 后实际编码 payload 字节数。Browser
structured-clone transport 不使用浏览器实现私有的 clone 大小；它对 schema payload 使用确定的
logical typed-tree 计数：null 为 1 字节，boolean 为 2 字节，number/bigint 为 9 字节，
string/`Uint8Array` 为 5 字节加 UTF-8/内容长度，array/record 为 5 字节加子项，record key
按排序顺序各计 4 字节加 UTF-8 长度。共享引用按每次 tree occurrence 计数；cycle、accessor、
symbol key、exotic prototype、超过 65,536 个节点或超过消息上限都拒绝。transfer list 的实际
资源字节不计入 logical payload，必须通过 segment/Surface 的显式长度和 slot 规则另行逐项绑定。

任何乘加、offset/len 和 allocation 都使用 checked arithmetic。未知 mandatory 字段/variant 返回协议错误；未知 optional 字段按 minor 兼容规则忽略并保留诊断计数。

## 7. ID 与 sequence

- `WorkerId` 在 Worker epoch 内固定，重启后改变。
- `SessionId` 在同一 Worker epoch 内不重用。
- `RequestId` 由 command 发起方生成，在 session 内唯一。
- `DataTicket`、`SurfaceId` 和 ChangeSet revision 由 owner 生成。
- 每个发送方向维护从 1 开始的单调递增 `sequence`；0、重复和倒退都以 `NonMonotonicSequence` 拒绝。它用于检测重放和调试，不保证跨方向全序。
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

每个 command schema 必须声明：最大 payload、是否可重放、所有权、状态前置条件、预算、敏感字段和可能产生的 outcome events。生成 descriptor 将它们命名为 `outcome_events`；stream outcome 不得被误称为终态。创建 Request 的 command 另行遵守唯一终态规则。

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
| `SurfaceReclaimed` | 否 | ReleaseSurface ack 或超时/close 后 handle 失效通知，reason 区分原因 |
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
- `ProvideData` 必须引用相同 source snapshot；每个 `ByteRange` 非空且 exclusive end
  使用 checked arithmetic，`DataSegment.byte_length == range.len`，slot 按 segment
  顺序唯一覆盖实际 transfer table，并验证每个实际 transfer 的 byte length 恰好覆盖声明范围。
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

`SetViewport` 必须携带完整且可重放的 viewport identity，不得依赖上一条 viewport 的隐式字段：

```rust
pub struct PageGeometry {
    pub identity: [u8; 32],
    pub media_box_x_milli_points: i32,
    pub media_box_y_milli_points: i32,
    pub media_box_width_milli_points: u32,
    pub media_box_height_milli_points: u32,
    pub crop_box_x_milli_points: i32,
    pub crop_box_y_milli_points: i32,
    pub crop_box_width_milli_points: u32,
    pub crop_box_height_milli_points: u32,
    pub intrinsic_rotation: PageRotation,
}

pub struct PageViewport {
    pub page_index: u32,
    pub coordinate_space: PageCoordinateSpace,
    pub geometry: PageGeometry,
    pub clip_x_milli_points: i32,
    pub clip_y_milli_points: i32,
    pub clip_width_milli_points: u32,
    pub clip_height_milli_points: u32,
}

pub struct ViewportRequest {
    pub generation: u64,
    pub document_revision: u64,
    pub annotation_revision: u64,
    pub zoom_numerator: u32,
    pub zoom_denominator: u32,
    pub visible_pages: Vec<PageViewport>, // schema maximum: 64
    pub quality: QualityPolicy,
    pub output_profile: u32,
    pub device_scale_milli: u32,
    pub rotation: PageRotation,
    pub optional_content_id: u64,
}
```

- `generation`、`document_revision`、`annotation_revision`、zoom、DPR、rotation、quality、output profile 或 optional-content identity 的任何可见语义变化都生成新的完整 generation。
- zoom 以非零、约分后的 `numerator/denominator` 表示；接收端拒绝零分子、零分母和非 canonical 比例。DPR 使用非零整数 milli-scale，禁止跨 wire 传递 `f32`/`f64`。
- `PageCoordinateSpace::PdfPointsBottomLeft` 明确 page box 和 clip 的坐标空间；所有坐标使用千分之一 PDF point，宽高必须非零，乘加和坐标转换使用 checked arithmetic。
- `PageGeometry.identity` 是完整 page geometry 的私有 32 字节 identity；接收端同时验证显式 media/crop box、intrinsic rotation 与该 identity 所绑定的 Native page。
- `visible_pages` 最多 64 项，page identity 不得重复；每项必须绑定同一 `document_revision`，且 clip、geometry 和 page index 必须与当前 Native document snapshot 一致。
- command correlation 中的 generation 必须等于 `ViewportRequest.generation`。缺少任何 identity 字段、复用旧 revision 的 geometry 或混入其他 page identity 都是协议错误。

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
- 所有已发布 Surface 都绑定一个完整 Native RenderPlan、Native Scene、CapabilityDecision 和 RenderConfig；不允许拼接外部输出。
- Native backend retry 只能在已声明的 `ReferenceCpu`/`FastCpu` 之间进行，并必须产生新的 RenderConfigHash 或 RendererEpoch；协议 0.2 不表示外部或 GPU backend。

## 13. Surface transport

```rust
pub struct SurfaceRegion {
    pub page_index: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub coordinate_space: SurfaceCoordinateSpace,
}

pub struct SurfaceMetadata {
    pub id: SurfaceId,
    pub owner: SurfaceOwner,
    pub generation: u64,
    pub region: SurfaceRegion,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
    pub alpha: AlphaMode,
    pub byte_offset: u64,
    pub byte_length: u64,
    pub render_config: RenderConfigHash,
    pub renderer_epoch: RendererEpoch,
    pub plan_id: RenderPlanId,
    pub plan_hash: RenderPlanHash,
    pub scene_hash: SceneHash,
    pub decision_hash: CapabilityDecisionHash,
    pub backend: NativeBackend,
}

pub enum SurfaceTransport {
    OffscreenCanvasCommit { canvas: CanvasId, region_length: u64 },
    BrowserTransfer {
        slot: u16,
        transfer_kind: BrowserTransferKind,
        transfer_length: u64,
    },
    SharedMemory {
        handle: PlatformHandle,
        region_length: u64,
        release_token: u64,
    },
    LocalMemory {
        region_length: u64,
        memory_epoch: MemoryEpoch,
    },
}
```

规则：

- `region` 是 Surface 在 `DevicePixelsTopLeft` page space 中的最终 placement，不是 backing-memory byte range。它的 page、坐标、尺寸和 coordinate space 必须逐项匹配 `plan_id + plan_hash` 指定的 tile；Host 不得根据到达顺序自行推导 placement。
- `width`、`height`、`stride`、format 和 alpha 描述 pixel buffer；`byte_offset` 与 `byte_length` 描述该 buffer 在 transport backing region 内的精确 byte window。
- 接收端先以 checked arithmetic 计算 `layout_bytes = stride * height`，验证 `stride >= width * bytes_per_pixel`、format alignment、`byte_length == layout_bytes`，再验证 `byte_offset + byte_length <= region_length/transfer_length`。任何溢出、越界或长度不一致都在访问 backing storage 前拒绝。
- owner、generation、renderer epoch、render config、plan ID/hash、scene hash、decision hash、Native backend 和 region 必须与当前 Worker、Session、Viewport 和已接受 RenderPlan 全部一致；仅 hash 相等不能替代 owner/epoch/generation 校验。
- `BrowserTransfer.slot` 必须指向同一 `postMessage` 的实际 transfer list 项。
- `OffscreenCanvasCommit.canvas` 必须是通过 `RegisterCanvas` 成功登记且 owner/transfer slot 匹配的 canvas。
- 裸 Wasm pointer 不得跨 Worker/realm；同 Worker local surface 额外验证 `memory_epoch`。
- SharedArrayBuffer 只在握手协商且浏览器跨源隔离满足时使用。
- Desktop shared-memory handle 必须验证权限、完整 region length、非零 release token、backend 和 session owner。
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

pub enum CollectionCompleteness {
    Complete,
    Truncated,
}

pub struct CapabilityLocation {
    pub page_index: Option<u32>,
    pub object_number: Option<u32>,
    pub object_generation: Option<u16>,
    pub source_offset: Option<u64>,
    pub command_index: Option<u32>,
    pub resource_id: Option<u32>,
}

pub struct CapabilityContext {
    pub code: u32,
    pub value: u64,
    pub location: Option<CapabilityLocation>,
}

pub struct CapabilityRequirement {
    pub id: u32,
    pub capability: u16,
    pub parameter: u64,
    pub context: CapabilityContext,
    pub dependencies: Vec<u32>,     // schema maximum: 32
    pub scope: CapabilityScope,
    pub contributor_ids: Vec<u32>, // schema maximum: 16
    pub location: Option<CapabilityLocation>,
}

pub struct CapabilityContributor {
    pub id: u32,
    pub kind: CapabilityContributorKind,
    pub code: u32,
    pub location: Option<CapabilityLocation>,
}

pub struct CapabilitySubject {
    pub source: SourceIdentity,
    pub document_revision: u64,
    pub revision_startxref: u64,
    pub page_index: u32,
    pub page_object_number: u32,
    pub page_object_generation: u16,
    pub scene_schema_major: u16,
    pub scene_schema_minor: u16,
    pub scene_hash: SceneHash,
}

pub struct CapabilityDecision {
    pub decision_schema_version: u16,
    pub status: SupportStatus,
    pub profile: CapabilityProfileId,
    pub profile_version: u32,
    pub policy_version: u32,
    pub subject: CapabilitySubject,
    pub missing: Vec<CapabilityRequirement>, // schema maximum: 16
    pub missing_total: u32,
    pub missing_completeness: CollectionCompleteness,
    pub contributors: Vec<CapabilityContributor>, // schema maximum: 32
    pub contributors_total: u32,
    pub contributors_completeness: CollectionCompleteness,
    pub scope: CapabilityScope,
    pub location: Option<CapabilityLocation>,
    pub rejection_code: Option<u32>,
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

- Capability evaluator 必须在高分辨率 raster allocation 前检查完整 Scene requirement/dependency graph、command 和 resource；Scene producer 的 Supported/Unsupported 摘要不能替代产品 decision。
- requirement、dependency、contributor 和 location 使用 canonical ID/order；重复、悬空、前向/循环依赖、非法 scope/context/location 或 totals/completeness 不一致必须在发布 decision 前拒绝。
- `missing_total`/`contributors_total` 是完整求值后的总数，不是 retained list 长度。`Complete` 要求 retained length 等于 total；`Truncated` 要求 retained length 小于 total。超过 16 个 missing 或 32 个 contributor 时保留 canonical prefix、给出准确 total 并显式标记 `Truncated`，禁止静默截断。
- `CapabilitySubject` 把 decision 绑定到完整 source、document revision、xref anchor、page object、Scene schema 和完整 SceneHash；其他 Scene、revision 或 page 不得复用。
- location 是内容脱敏的可选结构；字段组合必须与 scope/context 一致。`source`、`scene_hash`、`source_offset` 和嵌套 location 按 schema privacy metadata 处理，不进入默认 trace 或用户可见错误字符串。
- `Supported` 必须满足 `missing_total == 0`、missing 为空、`missing_completeness == Complete`，且没有顶层 location 或 rejection code；contributors 仍按通用 totals/completeness 规则计账。`Unsupported` 不得携带 rejection code；`Rejected` 必须携带稳定 `rejection_code`。结构有效但不在 profile 的能力是 `Unsupported`；malformed graph 或明确 policy prohibition 才是 `Rejected`。
- `Unsupported` 不调用 baseline，也不创建部分 tile 或空白成功 Surface。
- `ResourceLimit` 不重新分配第二份外部引擎预算。
- `Cancelled` 是正常终态。
- ResourceLimit、Cancelled、SourceChanged 和 Internal 是 EngineError/终态，不得伪装成 Unsupported 或 Rejected。
- `RetryNativeRenderer` 只允许注册的 Native backend 变化，并必须更新 RenderConfigHash 或 RendererEpoch。
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
- 完整 schema SHA-256 变化必须更新全部生成输出和证据；wire 前 16 字节只用于握手字段，不得作为构建证据中的完整 digest。
- wire schema hash 相同也不能绕过 `supported`/`mandatory` capability 交集；wire hash 不同的兼容只接受生成向量明确覆盖的同 major、不同 minor 组合。
- 支持窗口和废弃日期由 release policy 定义；不得永久保留无测试旧协议。

## 22. 可重放与诊断 trace

协议 trace 默认记录 envelope metadata、message type、ID、sequence、大小、耗时、错误/能力 code 和脱敏环境。不得记录 PDF bytes、文本、密码、query、注释或平台 handle。

测试环境可以生成内容寻址的 replay bundle；包含敏感 payload 时必须使用授权、加密和保留策略。Replay 必须能注入乱序、重复、丢包、断连和延迟。

## 23. 必测协议场景

- 握手 major mismatch、完整/wire schema mismatch、unknown mandatory capability、missing mandatory capability 和 unknown optional supported bit。
- frame 截断、超长、未知 message type、非法枚举和 transfer slot。
- Open/NeedData/ProvideData 的乱序、重复、SourceChanged。
- PasswordRequired 期间 cancel/close 和晚到 secret。
- viewport generation 快速变化、revision/geometry/zoom/DPR/OCG 单字段变化与 Surface 乱序。
- completion/cancel 竞争和唯一终态。
- Surface 单字段 plan/scene/decision/backend/region/range/epoch 错配、checked range overflow、重复 release、lease reclaim、Wasm memory epoch 和桌面 stale handle。
- queue 满时 critical 消息仍可送达。
- Worker crash/restart 后旧 epoch 消息到达。
- old/new minor 的双向兼容、完整 SHA-256 与 wire 前 16 字节向量、生成 validator 一致性和干净 checkout 下 `--check`。
- 敏感字段不进入 trace、日志或错误字符串。

## 24. 评审清单

- [ ] schema 是 Rust/TS/Desktop codec 的单一事实来源。
- [ ] generator 写模式和 `--check` 模式已执行，完整 SHA-256、wire 前 16 字节和全部生成输出一致。
- [ ] 新 command/event 定义了状态前置条件、owner、预算和唯一终态。
- [ ] 消息长度、枚举、ID、handle、slot 和 payload 均被验证。
- [ ] 乱序、取消、close、重启和 stale generation 有明确结果。
- [ ] Surface 的 owner、plan、Scene、decision、backend、placement、byte range 和 acquire/transfer/release/反压完整。
- [ ] CapabilityDecision 的 subject、bounded totals/completeness、contributors、location、rejection 与 EngineError/取消没有混淆。
- [ ] 版本变化符合 major/minor 和 capability 协商规则。
- [ ] Browser/Desktop adapter 不承载 PDF 语义。
- [ ] 协议不包含外部 PDF 引擎路径。
- [ ] 兼容、fuzz、生命周期和隐私测试已更新。
