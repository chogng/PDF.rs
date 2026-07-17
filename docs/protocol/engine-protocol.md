# PDF.rs Engine Protocol

状态：M4 canonical contract
Canonical source：`protocol/engine.protocol`

本文定义 Host 与 Native Engine Worker 之间唯一受支持的控制协议。Rust、Browser
TypeScript 和 Desktop registry 都由 canonical source 生成；本文解释不变量，不是第二份
schema。任何与生成产物不一致之处，以 canonical source 和生成检查为准。

## 1. 版本、限制与生成产物

当前唯一注册版本是 major `0`、minor `2`：

- `max_message_bytes = 16,777,216`
- `max_transfer_slots = 64`
- `max_data_segment_bytes = 4,194,304`
- `max_data_ticket_bytes = 16,777,216`
- payload codec：`fixed_le_v1`

握手必须使用当前 major/minor 和当前 wire identity 的 16-byte hash。该 identity 不是裸
schema 文本的 SHA-256；其预像绑定
`PDF.rs/EngineProtocol/WireIdentity/v1`、`payload_codec_abi_version = 1`、codec 名称和
canonical schema 全部 bytes。完整 32-byte digest 进入构建证据，握手使用其前 16 bytes。
因此同一 schema 文本不能在不同 codec ABI 下冒充同一 wire layout。

仓库没有已认证的旧
minor decoder；旧 minor、未来 minor、major 不同，以及同 minor 不同 schema hash 都拒绝。
未知 optional capability bit 可以忽略，未知 mandatory bit 必须拒绝。

生成器一次产生并校验：

- `runtime/protocol/src/generated.rs`
- `platform/browser/generated/engine-protocol.ts`
- `platform/desktop/generated/engine-protocol.registry`
- `protocol/generated/schema-hash.txt`
- `protocol/generated/compatibility-vectors.json`
- `protocol/generated/invalid-vectors.json`
- `protocol/generated/payload-codec-vectors.json`

`pdf-rs-protocol-codegen --check` 必须证明这些文件来自同一 canonical source、generator
version 和 schema hash；手工修改生成文件无效。

首次 `Hello` 也不猜测 payload layout：接收端先只读固定 header，要求 exact current
major/minor、握手 message ID、固定 flags/长度上限，再由编译进产品的
`(major, minor, payload codec ABI)` bootstrap registry 选择唯一 Hello decoder。Hello 内的
wire hash 随后确认双方选择的是同一完整 identity；当前没有 fallback decoder。

## 2. 20-byte control header

每条 control frame 都以固定 20 bytes 开始。整数均为 little-endian，无 padding：

| Offset | Width | Field |
|---:|---:|---|
| 0 | 2 | `major: u16` |
| 2 | 2 | `minor: u16` |
| 4 | 2 | `message_type: u16` |
| 6 | 2 | `flags: u16` |
| 8 | 4 | `payload_len: u32` |
| 12 | 8 | `sequence: u64` |

`payload_len` 必须精确等于 frame 在 byte 20 后的剩余长度，同时不超过全局限制、握手协商
限制、消息 descriptor 的 `max_payload_bytes` 和生成器证明的最大编码长度。当前消息 flags
均只允许 `0`。message type 未注册、sequence 为 `0`、整数溢出或 frame 有多余/缺失 bytes
都拒绝。

Control payload 的规范形式是：

```text
fixed_le_v1(Correlation) || fixed_le_v1(message payload record)
```

message type 决定 command/event kind 和 payload record；wire 不再携带另一个字符串 variant
名。Correlation 与 record 必须各自完整解码，整个 payload 必须恰好消费完。

## 3. `fixed_le_v1`

`fixed_le_v1` 是无 padding、无隐式默认值的 schema-order codec：

- `u8/u16/u32/u64/i32` 使用其固定位宽 little-endian；`i32` 使用二进制补码。
- `bool` 使用一个 byte，只有 `0` 和 `1` 合法。
- enum 使用声明的底层整数宽度；未注册 tag 拒绝。
- `bytes16`、`bytes32` 直接编码固定数量的 bytes，不带长度。
- bounded `bytes<N>` 编码 `u32 byte_length` 后接原始 bytes。
- `optional<T>` 编码一个 marker byte：`0` 表示 absent，`1` 后接 `T`；其他 marker 拒绝。
- `list<T,N>` 编码 `u32 count` 后按顺序编码元素；count 必须不超过 schema 上限。
- record 按 canonical 字段顺序编码，既无 field tag，也无 padding。
- tagged union 先编码其声明宽度的 tag，再按该 variant 的 canonical 字段顺序编码。

Decoder 在读取 container 内容前验证 `u32` 长度、schema 上限、累计 item/byte 上限和剩余
输入。递归深度、checked arithmetic、truncation、非 canonical marker、未知 tag 和 trailing
bytes 都以稳定 codec error 拒绝。Browser fixed bytes 必须复制为 owned snapshot，并拒绝
SharedArrayBuffer-backed view，避免验证后的并发突变。

## 4. 握手与 capability

握手顺序固定：

```text
Host -- Hello(host ProtocolHello) --> Engine
Host <-- EngineHello(engine ProtocolHello, execution capabilities) -- Engine
Host -- HelloAccept(exact minor/hash) --> Engine
Host <-- Ready(worker, exact minor/hash, profiles, outputs, execution capabilities) -- Engine
```

`ProtocolHello` 校验 endpoint role、exact version/hash、非零且有界的 message/resource
limits，以及 `EndpointCapabilities`：

- 本端 `mandatory` 必须是本端 `supported` 的子集。
- mandatory 只能使用已注册 bit。
- `local.mandatory & !peer.supported` 和反方向都必须为零。
- 协商结果只包含双方 supported 的已知交集，并取双方 limits 的较小值。
- Browser 协商先把顶层 hello 和 nested capabilities 捕获为 exact own data-descriptor
  snapshot，再对 snapshot 完整校验；accessor、exotic prototype、共享可变 schema-hash view
  一律拒绝，避免校验与 WeakSet 认证之间发生 TOCTOU。
- 导出的 `SCHEMA_HASH` 只是 caller-owned convenience copy；握手/Transcript 使用模块私有
  canonical bytes。所有被 validator 或 admission 信任的 message/correlation/outcome、
  EngineError policy、enum/capability registry 都在导出前递归冻结。
- Browser sequence tracker 只接受 exact constructor 产生的 private-branded frozen instance；
  subclass、prototype/own override 和 prototype-forged object 一律拒绝。Envelope validator
  调用模块捕获的原始 pending operation，业务校验后 commit 时再次检查 private high-watermark。

Endpoint capability 只表示 transport：transferable ArrayBuffer、transferable ImageBitmap、
SharedArrayBuffer、Desktop shared memory 和同 Worker local memory。PDF feature 支持只由
CapabilityDecision 表示。

`EngineExecutionCapabilities` 是另一命名空间。当前仅注册 Worker-private
`OffscreenCanvasStaging`；它可以优化 Worker 内部绘制，但不是 OOB Surface transport，也
不能直接改变 DOM-bound presentation。Ready 的 capability profile 和 output profile 列表
必须非空、严格递增、无重复；当前注册 `BaselineNative` 和 `Srgb`。

## 5. Correlation 与联合验证

Correlation shape 由 message descriptor 生成：

| Shape | worker | session | request | generation |
|---|---|---|---|---|
| `Worker` | required | forbidden | forbidden | forbidden |
| `Session` | required | required | forbidden | forbidden |
| `Request` | required | optional | required | forbidden |
| `OpenRequest` | required | forbidden | required | forbidden |
| `SessionRequest` | required | required | required | forbidden |
| `Generation` | required | required | forbidden | required |

Shape-only 校验不能授权 dispatch。Adapter 必须把 header message type、command/event kind、
payload 内重复 identity、当前 Worker/session/request/generation registry、状态前置条件和实际
OOB 资源作为一次联合验证。例如 SetViewport generation、NeedData 的 session/request、
RequestCancelled target、Surface owner/generation 和 release lease 都必须与 Correlation 及
当前 registry 相等。

每个方向各自维护单调 sequence。允许 gap，不允许 `0`、重复或倒退。Sequence 只能在以下
步骤全部成功后事务提交：

1. header、exact frame length、版本、message descriptor 和 negotiated limits；
2. canonical codec、record invariant 和无 trailing bytes；
3. correlation shape、identity、Worker/session/request/generation 与 state；
4. capability、logical OOB slot、实际资源 type/extent/rights 和生命周期前置条件。

任一步失败都不得提交 sequence、采用资源、推进状态或调用 Native parser/render 逻辑。

## 6. OOB resource table

Control payload 不内嵌大 bytes、ImageBitmap、共享内存或平台资源。Browser `postMessage`
使用独立 OOB resource table：

```text
physical table index 0 = transferred control ArrayBuffer
logical protocol slot n = physical table index n + 1
```

物理 index `0` 始终保留，且不计入 `max_transfer_slots`。即使消息没有 logical OOB，
control 仍占 index `0`；payload slot 永远不能引用 control。Logical slots 必须从 `0` 连续、
唯一并与 descriptor 数量一致。Browser adapter 逐项验证实际对象类型、transfer ownership、
byte length 或 dimensions；Desktop adapter 验证 authenticated handle table、rights、region
length 和 owner。裸 Wasm pointer、`WebAssembly.Memory`、任意文件描述符或未注册 executable
resource 不得跨边界。

## 7. Immutable Range bytes

Engine 不拥有任意文件或网络访问。Open 只提交不可变 `SourceDescriptor`。需要 bytes 时，
Engine 发 `NeedData`：

- Correlation 是 `SessionRequest`，把 ticket 绑定到 Worker、session 和原 Open/request。
- `ticket`、source revision 和 checkpoint 必须非零且匹配当前 snapshot/job。
- ranges 为 1..16 个 checked、非空、严格递增且彼此分离的 `ByteRange`。
- 单 range 不超过 4 MiB，ticket 聚合不超过 16 MiB。
- priority 使用注册的 `DataPriority`，不是隐式整数策略。

Host 成功时发 `ProvideData`。每个 `DataSegment`：

- `role = ImmutableRangeBytes`；
- `byte_length == range.len`；
- `slot == segment index`，segments 非空且最多 16；
- ranges 维持相同 snapshot、严格顺序和聚合上限。

Browser logical slot 映射到 transferred ArrayBuffer，实际 `byteLength` 必须精确相等；
Desktop logical slot 映射到 authenticated、只读、不可变的 byte attachment，并验证相同
长度和最小 rights。源文件本身仍由 Host 持有，不授予 Worker。

无法提供 bytes 时发零 OOB 的 `FailData(ticket, expected, observed?, code, retryable)`。
`SourceChanged` 必须携带与 expected 不同的 observed identity，且不可标记 retryable；其他
failure code 不得携带 observed。Foreign、duplicate、late 或 snapshot 不匹配的 ticket 都
拒绝。Engine 可用 `DataFailed(ticket, EngineError)` 结束对应数据操作。

## 8. 文档、page metrics 与 viewport

Open 的终态成功是 `DocumentReady`，它发布 session、document revision、精确 page count、
profile 和 policy version。Page geometry 不从 Browser 猜测：

- `GetPageMetrics(document_revision, start_index, max_count)` 使用 `SessionRequest`；
- revision 必须匹配当前文档，`max_count` 为 1..64；
- `PageMetrics` 回显 revision/start/total，并返回最多 64 个连续 page index；
- batch 不得越过 `total_pages`；geometry identity 和 media/crop dimensions 必须非零有效。

SetViewport 提交完整 generation：document/annotation revision、约分后的 zoom、visible page
geometry/clip、quality、`Srgb` output profile、DPR、rotation 和 optional-content identity。
generation 非零且单调；visible page index 和 geometry identity 不得重复。新 generation
替代旧 generation 后，旧 Surface 不显示，但仍必须完成 release/reclaim。

## 9. CapabilityDecision 与 RenderPlan

CapabilityDecision 是唯一 PDF feature 支持判定。它绑定 source/document/page/Scene
subject、profile/policy version、scope、missing requirements、contributors、结构化 location
总数/完整性和完整 evaluated requirement/dependency/parameter/command/resource 审计计数：

- missing 最多 16、contributors 最多 32，非零 ID 严格递增且唯一；
- dependency/contributor ID 列表严格递增并引用当前集合；
- `Complete` 要求实际 count 等于 total；`Truncated` 要求实际 count 小于 total；
- location total/completeness 与全部 evaluated 计数都是 canonical wire/hash 字段，不得只保留在
  本地对象；顶层 summary location 的 retained count 只能是 0 或 1，`Complete` 要求它精确等于
  total，`Truncated` 要求它严格小于 total；
- `Supported` 不得有 missing、location 或 rejection code；
- `Unsupported` 不得有 rejection code；`Rejected` 必须有 rejection code。

`CapabilityReported` 同时携带 decision 和 `decision_hash`。Hash 必须由生成的 domain-separated
framing 对 exact `fixed_le_v1(CapabilityDecision)` 计算，不得 hash JSON、内存布局或
redacted projection。Domain 常量是：

```text
PDF.rs/EngineProtocol/CapabilityDecision/fixed_le_v1/v1
```

`GenerationPlanned` 携带 `RenderPlanManifest` 与 `plan_hash`。Manifest 绑定 plan schema version、
document revision、render config、renderer epoch、plan ID/generation、Scene/decision/geometry
hash、完整 viewport clip/zoom/DPR/rotation/optional-content/annotation identity、Native backend、
output profile、quality、唯一非空 row-major regions，以及与 regions 一一对应的非零
`TileContentHash` 列表。逐 tile hash 又绑定完整 source/page/content/config/epoch 身份，因此
manifest hash 覆盖完整计划身份。`plan_hash` 对 exact manifest 使用独立 domain：

```text
PDF.rs/EngineProtocol/RenderPlanManifest/fixed_le_v1/v1
```

生成 binding 和 byte-exact vectors 定义 domain framing；实现必须使用该 framing与
`fixed_le_v1` bytes，不得自定义拼接。SurfaceMetadata 的 render config、renderer epoch、
plan ID/hash、Scene hash、decision hash、backend、generation 和 region 必须属于同一已接受
manifest，禁止跨 plan 拼接。

两种 hash 的精确预像均为：

```text
UTF-8(domain) || 0x00 || u64LE(payload_byte_length) || fixed_le_v1(payload)
```

生成的 Rust/TypeScript preimage helper 和 `payload-codec-vectors.json` 中的 payload、
preimage、SHA-256 KAT 是唯一允许的 framing；policy 层只对 helper 返回的 bytes 做 SHA-256。

## 10. Surface transport 与 Host-mediated presentation

所有 Surface 共用 `SurfaceMetadata`：

- 非零 `SurfaceId`、`lease_token`、owner Worker/session、generation、renderer epoch 和 plan ID；
- 非零 config/plan/Scene/decision hashes；
- RGBA8；`stride >= width * 4` 且四字节对齐；
- `byte_length == stride * height`，`byte_offset + byte_length` checked 且位于实际 region；
- region 使用 device-pixels/top-left，并与 accepted RenderPlan region 相等。

Browser 只允许 Host-mediated presentation：

| Variant | OOB | 关键约束 |
|---|---|---|
| `BrowserArrayBuffer` | transferable slot | actual length 等于 `buffer_length`；Straight alpha；metadata byte range 在 buffer 内 |
| `BrowserImageBitmap` | transferable slot | actual width/height 等于 transport 与 metadata；Premultiplied alpha；offset `0`，stride=`width*4` |
| `BrowserSharedArrayBuffer` | attachment slot | negotiated capability + cross-origin isolation；固定不可增长长度；Straight alpha；fence/epoch 规则 |

Browser 不接受 Desktop variants。Worker-private Offscreen staging 如存在，也必须先产出上述
Host-mediated resource；Host 在改变 DOM 前再次核对当前 generation，因此 generation `42`
的迟到结果不能覆盖已提交的 generation `43`。

SharedArrayBuffer 额外规则：

- actual/current/max 与声明 `buffer_length` 相等，buffer 不可 grow；
- `fence_byte_offset` 四字节对齐，`fence+4` checked、在 buffer 内且不与 pixel range 重叠；
- `publication_epoch` 非零；
- publisher 完成 pixel writes 后以 atomic release 发布 epoch；
- Host 在使用 pixels 前后各做 atomic acquire load，两次都必须等于声明 epoch；
- epoch 变化、stale、零值或 fence 错误都拒绝并进入 release/reclaim；
- 同一 Surface lease 终态前不得 republish 或复用 backing。

Desktop 使用同一 SurfaceMetadata/plan identity：

- `SharedMemory` logical slot 映射 authenticated shared-memory handle，校验最小 rights、owner 和
  完整 `region_length`；
- `LocalMemory` 只在同一 Worker 和匹配 `MemoryEpoch` 内解释，memory grow 后旧 view 失效。

`ReleaseSurface` 必须回显 exact SurfaceId 与 `lease_token`。成功、重复、已终态或未知目标都
通过稳定 ack status 表达；token 不匹配不能释放别的 lease。`SurfaceReclaimed` 回显相同
identity/token 和 reason。Ack、reclaim、session close 或 Worker terminal 前，资源不可复用。

## 11. 消息与 outcome

### Commands

| ID | Command | Correlation | State | Outcomes |
|---:|---|---|---|---|
| 1 | Hello | Worker | Starting | EngineHello terminal；ProtocolFault terminal |
| 2 | HelloAccept | Worker | Starting | Ready terminal；ProtocolFault terminal |
| 3 | Open | OpenRequest | Ready | NeedData stream；DocumentReady/RequestFailed/RequestCancelled terminal |
| 4 | ProvideData | Session | OpeningOrReady | DataFailed terminal |
| 7 | SetViewport | Generation | Ready | GenerationPlanned/CapabilityReported/SurfaceReady stream；GenerationCompleted terminal |
| 8 | Cancel | Request | ActiveOrTerminalRequest | CancelAcknowledged terminal；RequestCancelled stream |
| 9 | ReleaseSurface | Session | SurfaceAliveOrReclaimed | SurfaceReleaseAcknowledged terminal；SurfaceReclaimed stream |
| 10 | CloseSession | Session | NonClosedOrClosed | CloseSessionAcknowledged terminal；SurfaceReclaimed/SessionClosed stream |
| 11 | Shutdown | Worker | ReadyOrDrainingOrStopped | ShutdownAcknowledged terminal；WorkerStopped/WorkerFault stream |
| 12 | FailData | Session | OpeningOrReady | DataFailed terminal |
| 13 | GetPageMetrics | SessionRequest | Ready | PageMetrics/RequestFailed/RequestCancelled terminal |

### Events

| ID | Event | Correlation |
|---:|---|---|
| 101 | Ready | Worker |
| 102 | NeedData | SessionRequest |
| 103 | DocumentReady | SessionRequest |
| 105 | CapabilityReported | Generation |
| 106 | SurfaceReady | Generation |
| 107 | RequestCancelled | Request |
| 108 | RequestFailed | Request |
| 109 | SessionClosed | Session |
| 110 | WorkerStopped | Worker |
| 111 | WorkerFault | Worker |
| 112 | ProtocolFault | Worker |
| 113 | SurfaceReclaimed | Session |
| 114 | EngineHello | Worker |
| 115 | DataFailed | Session |
| 116 | PageMetrics | SessionRequest |
| 117 | GenerationPlanned | Generation |
| 118 | GenerationCompleted | Generation |
| 121 | CancelAcknowledged | Request |
| 123 | SurfaceReleaseAcknowledged | Session |
| 124 | CloseSessionAcknowledged | Session |
| 125 | ShutdownAcknowledged | Worker |

`stream` outcome 可以在同一操作内出现多次；`terminal` outcome 或 terminal ack 必须唯一地结束
该 command ownership。Cancel、Surface release、session close 和 shutdown 是幂等命令。
Runtime 为已完成 identity 保留有界 tombstone，使重复命令返回 `AlreadyApplied`、
`AlreadyTerminal` 或 `UnknownTarget`，而不是重做副作用或静默丢弃。Critical ack、fault、
close、cancel 和 release traffic 不得因普通 progress/backpressure 被丢弃。

`GenerationCompleted` 的 `Failed` 状态必须携带 EngineError；其他状态不得携带 error。
produced region count 必须与该 generation 已发布且接受的 Surface accounting 一致。

## 12. EngineError、privacy 与 redaction

EngineError 只使用生成的稳定 code registry。每个 code 的 category、severity 和
recoverability 组合必须与 descriptor 完全一致，并带非零 `DiagnosticId`。Adapter 不解析
自由文本决定恢复策略，也不得把 protocol failure 改写为成功。

Schema field privacy 分为：

- `public`：可以进入有界结构化 trace；
- `private`：默认 redact，例如 source identity、Scene/decision hash、source offset；
- `sensitive`：始终 redact，例如 lease token 和数据 segment metadata。

生成的 Rust Debug 与 Browser diagnostics 必须递归执行 privacy policy。Codec error 只报告
稳定 code 和有界 offset/context，不 dump payload bytes、document 内容、validator、handle、
token、完整 identity 或 OOB resource。错误、metrics、trace、截图和 failure bundle 都遵守
同一规则。

## 13. 必测不变量

至少覆盖：

- exact schema handshake、role、双向 mandatory、未知 mandatory、旧/未来 minor 与 schema fork；
- header truncation、长度不等、oversize、flags、未知 message、sequence `0`/重复/倒退；
- `fixed_le_v1` 的 bool/optional 非 canonical marker、enum/union unknown tag、u32 container
  truncation/overflow、depth/item/byte limit 与 trailing bytes；
- Correlation 每个 required/optional/forbidden 组合及 payload identity mismatch；
- Browser control reserved index `0`、logical slot offset、duplicate/missing/extra/wrong-type OOB；
- ImmutableRangeBytes 的 zero/overflow/overlap/order、segment/ticket bounds、actual attachment
  length、FailData identity/code；
- page metrics 连续性、viewport geometry/generation、CapabilityDecision completeness 和两个 hash
  domain；
- 每种 Surface transport 的 capability、actual extent、alpha、layout、range、plan identity、
  generation、lease、duplicate release 和 reclaim；
- SharedArrayBuffer isolation、fixed length、fence alignment/overlap、epoch before/after mutation；
- codec/correlation/resource/state 失败后 sequence、state 和 ownership 均不提交；
- privacy/redaction 与 product dependency purity。

Compatibility、invalid 和 payload-codec vectors 必须在 Rust 与 Node 中逐条执行并断言预期结果；
只检查 vector 名称不构成 replay。
