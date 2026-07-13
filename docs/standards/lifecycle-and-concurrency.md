# PDF.rs 生命周期与并发规范

- 文档编号：RPE-STD-002
- 版本：0.1
- 状态：稳定态开发基线（持续迭代）
- 适用范围：Engine、Worker、Session、Source、Request、Job、Scene、Surface、Cache、ChangeSet

## 1. 目标

本规范定义跨线程、Wasm、IPC 和异步 Range 场景中的对象所有权、状态转换、取消、关闭和回收行为。所有实现必须满足：

- 没有隐式 owner；
- 没有依赖回调时序的正确性；
- close、取消和 worker 故障后不会发布 stale 结果；
- 资源在有限时间内可回收；
- 同一输入 snapshot 不会混入其他 revision 的字节或缓存；
- 生命周期错误返回稳定结果，不产生 use-after-close 或部分输出。

本规范与[代码规范](coding-standard.md)、[安全与资源预算规范](security-and-resource-budget.md)及[Engine 交互协议](../protocol/engine-protocol.md)共同约束实现。

## 2. 基本身份模型

以下 identity 不得互换或复用：

| Identity | 唯一性范围 | 失效条件 |
| --- | --- | --- |
| `WorkerId` | 宿主进程 | Worker 重启 |
| `SessionId` | Worker epoch | close、open 失败或 Worker 重启 |
| `SourceIdentity` | 不可变 source snapshot | validator/revision 变化 |
| `DocumentRevision` | Session | ChangeSet commit 或重新打开 |
| `RequestId` | Session | 请求进入终态并超过保留窗口 |
| `JobId` | Runtime | job 进入终态 |
| `ViewportGeneration` | Session viewport | 缩放、旋转、跳页、OCG 或 revision 改变 |
| `SurfaceId` | Worker epoch | release、reclaim、session close 或 Worker 重启 |
| `RendererEpoch` | 构建/算法配置 | renderer、字体、颜色或输出契约改变 |

跨边界 handle 使用 64 位 opaque 值，并能验证 generation。内部可以使用 slab/index，但 index 重用时必须增加 generation；旧 handle 必须稳定返回 `StaleHandle` 或 `SessionClosed`。

## 3. 所有权模型

| 对象 | 唯一 owner | 允许的共享方式 |
| --- | --- | --- |
| Engine worker | Host | 只通过 EnginePort/IPC 交互 |
| Session mutable state | Document actor | Worker 读取不可变 snapshot |
| SourceSnapshot | Session | `Arc`/共享不可变值 |
| RangeStore backing data | Source service | 带 SourceIdentity 的稳定 slice |
| Scene | Scene build result/cache | 不可变共享 |
| Viewport state | Session actor | 以 generation snapshot 传给 job |
| Surface | 创建它的 worker/session | 显式 acquire/transfer/release |
| ChangeSet | Session actor 或宿主 sidecar | 版本化不可变快照/命令 |
| Cache entry | 对应 cache shard | 通过完整 key 共享 |

禁止共享可变 `Document` 大对象。Document actor 是逻辑单写者；worker job 接收明确输入并返回不可变结果或状态化事件。

## 4. Worker 生命周期

```text
NotStarted → Starting → Ready → Draining → Stopped
                  └──────────────→ Failed
Ready ──crash/context fatal──────→ Failed
Failed ──host restart────────────→ Starting(new WorkerId/epoch)
```

- `Starting` 必须完成协议握手、schema 校验和 capability 协商后才能进入 `Ready`。
- `Draining` 拒绝新 session，允许有界时间内完成或取消已有操作。
- `Stopped` 和 `Failed` 是当前 Worker epoch 的终态。
- Worker 重启必须生成新的 `WorkerId`/epoch；旧 Session、Request、Surface 和平台 handle 全部失效。
- Host 可以根据持久化的 source descriptor、viewport 和 ChangeSet 重建 session，但不得假设 worker 内缓存仍存在。
- Worker crash 不得导致宿主自动上传原始 PDF；crash bundle 遵守隐私规范。

## 5. Session 生命周期

```text
Created → Opening ⇄ WaitingForData
               ⇄ WaitingForPassword
               → Ready → Closing → Closed
               └───────────────→ Failed
Ready ──source changed/internal fatal────→ Failed
```

### 5.1 创建与打开

- `Open` 被接受后立即分配 `SessionId` 和 `RequestId`，但不表示文档已可用。
- `SourceSnapshot` 在 session 生命周期内不可变；open 时绑定 identity、length 和 validator。
- 缺数据发出 `NeedData`，缺密码发出 `PasswordRequired`；两者都不是 open 失败。
- 每次密码提交属于独立 request；密码只在最小作用域存在，不进入日志或可重放事件。
- `Ready` 事件必须包含 document revision、page count 的已知状态、CapabilityProfile/policy 版本和可用宿主能力。
- open 失败后 session 进入 `Failed`，只允许查询安全诊断摘要和执行 close。

### 5.2 Close

- Close 幂等；重复 close 返回相同可观察结果，不创建新 session 状态。
- 收到 close 后立即拒绝新请求，并使所有未终态 request 进入取消流程。
- close 不等待 UI release 所有 surface 才能开始；实现必须按协议回收或使其失效。
- `SessionClosed` 事件必须在该 session 的资源不再可能产生新事件后发布。
- close 后到达的网络、codec、GPU 和 worker 结果只能用于释放内部资源，不得更新缓存或发给 UI。
- session ID 不得在同一 Worker epoch 内重用。

## 6. Request 与 Job 生命周期

Request 是协议可见操作；Job 是 runtime 内部可调度单元。一个 Request 可以产生多个 Job，但每个 Job 只属于一个 Request 或明确标记为可共享基础设施任务。

```text
Accepted → Queued → Running ⇄ WaitingForData
                     │  ⇄ WaitingForResource
                     ├──→ Completing → Completed
                     ├──→ CancelRequested → Cancelled
                     └──→ Failed
```

### 6.1 一般规则

- 每个 Request 恰好有一个协议终态：`Completed`、`Cancelled` 或 `Failed`。
- 进度、NeedData、Capability 和 partial metadata 事件不是终态。
- Job 可以多次暂停和重新入队，但不得重复发布同一 completion。
- runtime 必须持有 completion guard 或等价机制，解决完成/取消/close 竞态。
- 取消 token 与资源预算必须传递给所有子 Job、codec 和 renderer。
- 共享 Range 下载不因一个订阅者取消而必然取消；最后一个订阅者离开后按 source policy 决定是否保留。

### 6.2 取消优先级

如果完成与取消并发：

1. completion 已原子提交时，Request 可以完成，但 UI 仍按 generation 判断是否采用结果；
2. cancel 已原子提交时，后续结果只能释放，不得发布成功；
3. session close 或 Worker epoch 改变始终使未提交 completion 失效；
4. 不允许同一 Request 同时发出成功和取消终态。

取消是正常结果，不转换为 `Internal`，也不进入错误率分母。

## 7. Range、DataTicket 与 ResumeCheckpoint

```text
Created → Pending → Ready
                  → Failed
                  → SourceChanged
                  → Abandoned
```

- `DataTicket` 只能完成一次。
- ticket 与 `SourceIdentity`、缺失 ranges 和订阅 job 绑定。
- 数据到达只唤醒 runtime；不得在网络、JS 或 FFI 回调栈中继续 parser。
- `ResumeCheckpoint` 必须拥有恢复所需状态，或指向已经声明幂等的阶段边界。
- source validator 改变时，相关 ticket 进入 `SourceChanged`，整个 session 终止；不得清缓存后把新旧字节拼接。
- Job 取消只解除订阅；共享 ticket 的其他订阅者不受影响。
- RangeStore 中返回的 `ByteSlice` 必须引用稳定 backing storage，并携带 snapshot identity。

必须测试数据、取消和 close 的所有到达顺序。

## 8. Viewport generation 与 stale work

以下操作必须递增 `ViewportGeneration`：

- zoom、DPR、rotation 或 viewport geometry 改变；
- 跳页或滚动预测策略产生新的可视集合；
- optional-content 状态改变；
- annotation/document revision 影响可见结果；
- 输出 profile 或质量策略改变。

每个 viewport Job 和 Surface 都携带 generation。结果发布前和 UI 采用前必须各检查一次 generation。旧 generation：

- 应尽快取消；
- 即使已完成也不得覆盖当前画面；
- 可以进入不改变语义的低层内容缓存，但不得进入用旧 generation 键控的产品 tile 命中路径；
- 不计为渲染失败，但计入 `stale_work` 性能指标。

## 9. DocumentRevision、ChangeSet 与保存

```text
BaseRevision(n) + ChangeSet(k)
        │ validate/apply
        ▼
WorkingRevision(n,k)
        │ successful save/commit
        ▼
BaseRevision(n+1)
```

- ChangeSet 必须引用预期 base revision；不匹配返回 `RevisionConflict`。
- ChangeSet 应作为宿主可持久化的版本化数据，不只存在于 worker 内存。
- apply 成功后，受影响的 Scene、Text、Tile 和 writer cache 必须通过 revision/key 自然失效。
- save 使用 snapshot：开始保存后，新 ChangeSet 不得静默混入本次输出。
- save 失败不得修改 base revision 或丢失未保存 ChangeSet。
- close 前若存在未保存变更，由宿主决定提示、保存 sidecar 或放弃；engine 不自行弹 UI。

## 10. Scene 与缓存生命周期

- Scene 构建完成后不可变；增量变化生成新 Scene/revision，不原地修改被 renderer 使用的 Scene。
- 缓存条目必须有 owner、作用域、完整 key、字节计费和淘汰策略。
- job/page/session/process/persistent cache 必须显式区分。
- `ResourceLimit`、损坏、取消和 Internal 结果不得缓存为成功值。
- 可以短期缓存稳定失败，但 key 必须包含 source revision、policy 和 renderer/codec epoch，并具有 TTL 或明确失效条件。
- session close 必须释放 session-only cache 引用；跨 session cache 只能保留不含秘密且 identity 完整的不可变数据。
- GPU context lost 时，所有绑定该 context/epoch 的资源整体失效。

缓存驱逐可以改变性能，不得改变 CapabilityDecision、错误分类或最终语义。

## 11. Surface 生命周期

```text
Allocated → Published → Acquired/Transferred → Released
     │           │              │
     └───────────┴──────────────→ Reclaimed
Any state ──session close/worker restart────→ Invalid
```

### 11.1 通用规则

- Surface owner 是创建它的 worker/session。
- metadata 必须包含 `SurfaceId`、尺寸、stride、format、alpha、generation 和必要 epoch。
- `stride * height`、offset/len 和平台 handle 权限在生产端与消费端都验证。
- 发布后修改像素内容必须有独占 ownership；默认 Surface 视为不可变。
- 协议必须规定 acquire、一次性 transfer、ack/release、超时回收和 close 行为。
- release 幂等；访问已 release/reclaimed/invalid 的 Surface 返回 `StaleSurface`。

### 11.2 Wasm 与浏览器

- Wasm local pointer 只能由同一 Worker、同一 Wasm Memory 和匹配 `memory_epoch` 的 JS glue 访问。
- Wasm memory grow 后，旧 local view 必须重建；epoch 不匹配时拒绝访问。
- 跨 realm 使用实际 transfer list 中的 `ArrayBuffer`/`ImageBitmap`，不得发送裸 Wasm pointer。
- SharedArrayBuffer 只在跨源隔离和协议协商成功后使用。
- `ImageBitmap`、OffscreenCanvas 和 transfer slot 必须有明确一次性所有权。

### 11.3 桌面

- 共享内存/GPU texture handle 必须附权限、offset、len、backend 和 release token。
- handle 不得被未经授权的 session 导入。
- 宿主进程异常退出时，worker 必须能通过 lease/进程监控回收资源。

## 12. 锁、队列与反压

- 每个锁必须保护一组明确字段，禁止跨层大锁。
- 如果存在多锁获取，模块文档必须给出全序；运行时不得反向获取。
- 持锁期间不得进入 codec、GPU driver、FFI、IPC send、宿主回调或阻塞等待。
- command、event、render 和 telemetry 队列必须有独立容量与优先级。
- 反压时优先合并/丢弃可替代的 viewport 更新和 progress 事件；不得丢失 close、cancel、error、completion、release。
- 高优先级任务不得永久饿死后台任务；调度器应记录等待时间和优先级提升。
- 队列关闭必须唤醒所有等待方并返回稳定关闭结果。

## 13. 竞态裁决表

| 竞态 | 规定结果 |
| --- | --- |
| Result vs viewport change | 结果可完成，但旧 generation 不进入 UI |
| Completion vs cancel | 原子提交先到者决定唯一终态 |
| NeedData vs cancel | 取消订阅；ticket 可继续服务其他 job |
| Data arrival vs source change | source change 优先，数据丢弃，session 失败 |
| Surface publish vs close | 未提交 publish 取消；已发布 surface 立即失效/回收 |
| Save vs new ChangeSet | save 固定旧 snapshot，新变更留待下次保存 |
| GPU completion vs context lost | 未确认 completion 丢弃，使用新 epoch 重建 |
| Worker restart vs late IPC | WorkerId/epoch 不匹配，消息丢弃并记录协议指标 |

## 14. 超时、租约与泄漏检测

- Surface、共享内存、GPU handle、DataTicket 和外部 process runner 必须有可观测的最长存活策略。
- 超时用于回收和 watchdog，不替代确定性 FuelBudget。
- Debug/CI 构建应维护每类资源的 live count、owner 和创建位置摘要。
- session close 测试必须等待到资源计数回到基线。
- Worker shutdown 后不得存在可继续访问宿主 handle 的线程或任务。
- 泄漏检测输出不得包含 PDF 内容或秘密。

## 15. 必测生命周期场景

- open 后立即 close；close 重复调用；失败 session close。
- NeedData 前后取消、close、source validator 改变。
- 密码请求期间 close；错误密码重试；密码成功后旧请求到达。
- 快速缩放/滚动产生至少三个 generation，旧结果乱序完成。
- Surface transfer 前后 close、超时、重复 release、Wasm memory grow。
- GPU context lost 与 CPU 重试；旧 GPU completion 到达。
- ChangeSet/apply/save/close 的所有关键交错。
- Worker crash 后 session 重建，旧 IPC/handle 到达。
- 队列满时 close/cancel/completion 仍可送达。
- session 关闭后资源计数、内存和句柄回到允许基线。

状态机测试应同时包含模型测试、确定性调度测试、fuzz 和真实浏览器/桌面 E2E。

## 16. 评审清单

- [ ] 新对象有唯一 owner、状态机和终态。
- [ ] ID/generation/epoch 的唯一范围与失效条件明确。
- [ ] close、cancel、失败和 Worker restart 均可回收资源。
- [ ] 结果发布前验证 session/request/generation。
- [ ] 锁不跨 await、回调、FFI、IPC 或阻塞调用。
- [ ] 队列有容量、反压和关闭策略。
- [ ] 缓存 key 覆盖全部结果 identity，失败不会伪装成功。
- [ ] Surface acquire/transfer/release/timeout 已定义并测试。
- [ ] 关键竞态具备确定性或模型测试。
