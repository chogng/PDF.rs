# PDF.rs 测试规范

- 文档编号：RPE-STD-003
- 版本：0.1
- 状态：稳定态开发基线（持续迭代）
- 适用范围：单元、规范、属性、差分、浏览器/桌面 E2E、fuzz、corpus、性能与发布测试

## 1. 测试目标

测试系统与 parser、Scene、renderer 同为核心产品资产。测试必须能够回答：

- 行为由哪条规范、勘误或项目契约定义；
- 输入如何生成、授权和复现；
- 预期结果由谁、以什么权威等级推导；
- 失败是 Native 实现、测试 oracle、环境还是输入的问题；
- 资源、取消、并发和平台边界是否满足契约；
- 某项能力是否具备进入 ReleaseProfile 的证据。

单纯提高行覆盖率、让 PDFium 与 Native 像素相近或让一次手工文件成功打开，都不等于正确性成立。

## 2. 基本原则

- 新 feature 先建立测试与 oracle，再扩大实现。
- 测试必须确定性；随机测试记录 seed、生成器版本和 shrink 结果。
- 测试失败必须可在本地或固定 runner 中复现。
- 外部引擎只作 O4 observation，不是规范真值或产品后端。
- 不得自动接受首次 Reference 输出为 golden。
- 不得用扩大像素容差、增加重试或移除复杂样例掩盖回归。
- 测试代码遵守与产品代码相同的安全、来源和隐私要求。

## 3. 测试层级

| 层级 | 目标 | 默认触发 |
| --- | --- | --- |
| Unit | 纯函数、边界、错误分支、不变量 | 每次提交 |
| Component | parser、resolver、content、text、raster、cache 等模块契约 | 每次提交/PR |
| Atomic spec | 单一条款、勘误或行为差异 | 每次提交/PR |
| Property/model | 状态机、对象图、代数和 round-trip 不变量 | PR/nightly |
| Metamorphic | 等价输入变换后语义不变 | PR/nightly |
| Differential | Reference/Fast/GPU/外部 baseline 分歧发现 | merge/nightly |
| Lifecycle/concurrency | 取消、close、Range、乱序、崩溃恢复 | PR/nightly |
| Browser/desktop E2E | 真实 transport、Canvas、IPC、平台资源 | merge/nightly |
| Fuzz | 安全、状态空间、最小化 | 持续/nightly |
| Corpus | 真实世界支持率与回归 | nightly/weekly/release |
| Performance | 组件和用户路径分布 | PR/固定硬件池 |
| Release | 固定 profile 的完整准入 | 每次 release |

低层测试定位根因，高层测试证明集成结果。E2E 不替代单元/规范测试，像素测试不替代 Scene/Text diff。

## 4. 目录和命名

```text
tests/
├── cases/<feature>/<case-id>/
│   ├── input.pdf
│   ├── case.toml
│   ├── source.dsl
│   └── expected/
├── models/
├── properties/
├── metamorphic/
├── lifecycle/
├── browser/
├── desktop/
├── fuzz/
├── corpus/manifests/
└── performance/
```

Case ID 使用稳定的层级式名称：

```text
<domain>/<feature>-<specific-behavior>-<nnn>
```

例如 `transparency/soft-mask-luminosity-004`。ID 一经进入主干不得因移动目录而改变；重命名必须保留 alias/history。

回归用例使用 `regression/<issue-id>-<short-name>`，并链接最初失败、最小样例和修复提交。

## 5. Case manifest

每个非平凡 fixture 必须有机器可读 `case.toml`，至少包含：

| 字段组 | 必填内容 |
| --- | --- |
| Identity | id、title、owner、status、introduced_in |
| Specification | 规范版本、条款、勘误或项目契约 |
| Provenance | 自建/外部、来源、许可证、hash、可再分发性 |
| Features | Feature ID 和参数化 requirement |
| Validity | valid、invalid、ambiguous、real-world-tolerated |
| Expected | parse、Scene、Text、Pixel、diagnostic、CapabilityDecision/error |
| Oracle | O0-O4、derivation、reviewers、golden 生成权限 |
| Budget | 输入、对象、递归、解码、路径、像素、fuel、watchdog |
| Render | 尺寸、DPR、颜色 profile、alpha、AA、renderer epoch |
| Tolerance | exact、edge-aware、color-aware、manual-review |
| Runners | 必跑 Native runner 和可选外部 baseline |
| History | golden 变化、已知差异、issue、最后审核版本 |

Manifest validator 必须阻断缺少许可证、hash、oracle 或预算的 fixture。

## 6. Oracle 权威等级

| 等级 | 来源 | 可以证明什么 |
| --- | --- | --- |
| O0 Normative | 规范文本可直接推导的对象、状态、错误或 Scene | 精确语法/语义契约 |
| O1 Analytic | 人工可计算的小样例、几何/颜色推导、代数不变量 | Reference 和边界算法正确性 |
| O2 Adjudicated | 多实现分歧后由 reviewer 基于规范裁决 | 复杂语义与真实文件预期 |
| O3 Reference Regression | 已通过 O0/O1/O2 审核的 Reference 输出 | Fast/GPU/跨平台回归 |
| O4 Observational | PDFium/其他黑盒、视觉检查、启发式 | 发现问题和进入人工裁决 |

规则：

- O4 不得单独阻断规范符合性，也不得直接生成 O3。
- Reference 的正确性必须由 O0/O1/O2 覆盖；Reference 不能自证。
- O2 裁决必须记录规范条款、分歧结果、最小样例、reviewer 和理由。
- 一个 case 可以对不同 expected artifact 使用不同 oracle 等级。
- Oracle 变化是规范变更，必须经过独立于实现作者的评审。

## 7. Golden 管理

- Golden 只能由固定版本生成器生成或由 reviewer 审核后提交。
- CI 不得自动覆盖 golden。
- Golden 文件必须 canonical：稳定排序、稳定 ID、固定浮点/颜色/字体环境，不含路径、时间和线程顺序。
- Golden PR 必须附旧/新差异、oracle 等级、推导说明、受影响 feature/corpus 和两名 reviewer。
- 修改 Reference 与更新其 O3 golden 不得由同一未经独立裁决的步骤自动完成。
- 大型二进制 golden 使用内容寻址存储时，仓库仍保存 hash、schema、生成器和许可元数据。
- 删除 golden 必须说明能力移除、case 替代或 oracle 无效原因，不能为让 CI 通过而删除。

## 8. 解析、Scene、Text 与 Pixel 断言

断言顺序为：

1. parse/object/diagnostic；
2. canonical Scene；
3. geometry/coverage；
4. Text/structure；
5. final pixel。

### 8.1 Scene

比较 command、顺序、transform、clip、group、glyph、image、颜色和资源语义 hash。不得比较运行时指针、分配顺序或环境路径。

### 8.2 Text

比较 Unicode、原始 code/CID、glyph、quad、baseline、writing mode、source/visual/logical/structure order、选择、搜索和可访问性字段。

### 8.3 Pixel

| 模式 | 适用场景 | 要求 |
| --- | --- | --- |
| Exact | 自建 reference fixture | 逐通道一致 |
| Edge-aware | Fast/GPU 对 Reference | 边缘 coverage 容差，内部区域严格 |
| Color-aware | ICC/DeviceN/外部观察 | 联合评估颜色差、alpha、区域面积 |
| Semantic alerts | corpus | 文字、图片、高对比内容缺失直接升级 |
| Manual review | 规范歧义 | 保存完整差异包后裁决 |

禁止只使用整页 SSIM/平均误差。差分工具必须识别连通区域、文字区域、透明边界和高对比缺失。

## 9. Valid、Invalid、Unsupported 与 Recovery 测试

每项 feature 至少覆盖：

- 最小合法输入；
- 最大/边界合法输入；
- 每个必填字段缺失或类型错误；
- 溢出、截断、递归、循环引用和超预算；
- 参数组合和依赖 feature；
- strict 与明确允许的 tolerant recovery 差异；
- CapabilityProfile 支持、拒绝和 unexpected-unsupported；
- 失败后的 session/request/资源状态。

不得把 invalid 输入预期写成“没有崩溃即可”。必须断言稳定错误分类、位置、预算消耗边界和是否允许继续使用 session。

## 10. 属性、模型与 Metamorphic 测试

优先建立以下模型：

- Range/DataTicket/Request/close 状态机；
- xref/revision/object resolver；
- graphics-state stack 和 content operand stack；
- page/resource graph 循环检测；
- Surface acquire/transfer/release；
- ChangeSet/apply/save/reopen；
- scheduler priority、generation 和 cancellation。

Metamorphic 变换必须声明预期保持的语义，例如：

- 对象重编号但引用等价；
- 字典 key 排序、合法空白和注释变化；
- xref table/stream 表示切换；
- 内容流分段但 token 序列等价；
- 几何上等价的矩阵组合；
- 合法增量修订与 canonical 重写的可观察语义一致。

随机失败必须保存 seed，并优先 shrink 为结构化最小样例。

## 11. 生命周期与并发测试

测试必须使用可控 scheduler、fake source 和 fault injection 覆盖：

- open/NeedData/password/close 的所有关键顺序；
- 完成与取消竞争；
- Range 数据乱序、重复、失败和 source revision 改变；
- 多个 viewport generation 乱序完成；
- session close 后晚到的 codec/GPU/IPC 结果；
- queue 满、worker crash、GPU context lost；
- Surface 重复 release、超时、Wasm memory grow 和 stale handle；
- session 关闭后 live resource count 回到基线。

只依赖 wall-clock sleep 的并发测试不允许进入主干；必须使用可观察事件、虚拟时钟或确定性屏障。

## 12. 预算与取消测试

- 每个 budget 字段至少有 `< limit`、`= limit`、`> limit` 三类用例。
- 确定性 FuelBudget 在不同平台对同一输入必须得到相同结果。
- Wall-clock watchdog 只测试失控保护，不作为规范 oracle。
- 取消测试必须测量检查点和实际 p95/p99，不只断言最终返回 Cancelled。
- 超预算不得产生 panic、未标记部分页面或外部引擎重试。
- codec/FFI 的预算和取消必须有集成测试，不能只 mock adapter。

## 13. Fuzz 规范

每个 fuzz target 必须定义：输入模型、最大资源、seed corpus、dictionary、超时、期望的不变量、minimizer 和 owner。

最低目标包括：

- lexer、number、string、name、array/dictionary；
- xref/object stream/revision/repair；
- filters、字体、CMap、图片 codec adapter；
- content VM、graphics state、inline image、Form；
- path/clip/blend/mask/color function；
- page/name/structure tree、annotation/form；
- writer reopen/round-trip；
- Range/request/surface 状态机；
- GPU command/shader 输入边界。

Fuzz 发现的 crash、hang、预算异常或语义分歧必须产生最小化回归。安全问题样例按安全流程隔离，不直接公开提交原始敏感文件。

## 14. 外部 baseline 差分

PDFium 或其他完整处理器只能由 `tools/baseline` 进程级 runner 使用：

- 输入为固定 hash 的 corpus object；
- 记录 runner、revision、build flags、字体、颜色和平台指纹；
- 输出仅作为 O4 observation；
- 不共享产品 RangeStore，不参与用户请求；
- 不进入产品构建层或 release artifact；
- 不能因 baseline 不可用阻止 Native 产品构建本身，但可使独立差分 job 失败。

分歧必须分类为 Native 错误、baseline 错误、规范允许差异、损坏输入、规范歧义或未裁决。未裁决差异不能自动更新 golden。

## 15. Browser 与桌面 E2E

Browser 至少覆盖：Wasm/Worker 启动、Range/ETag、DPR/zoom/rotation、快速滚动、选择/搜索/a11y、消息乱序、OffscreenCanvas 有无、SharedArrayBuffer 协商、Worker 重启、GPU context lost 和资源清单无外部 PDF 引擎。

桌面至少覆盖：IPC schema、共享内存/texture handle 权限、worker sandbox/crash、文件 revision 改变、窗口/缩放、保存原子性和句柄回收。

E2E runner 固定浏览器/OS/driver image digest。失败 artifact 包含截图、协议 trace、环境和诊断 ID，但默认不包含用户文档内容。

## 16. Corpus 治理

| Tier | 运行频率 | 内容 |
| --- | --- | --- |
| T0 | 每次提交 | 自建原子用例、关键回归、快速变体 |
| T1 | PR/合并队列 | 高价值模块 corpus |
| T2 | Nightly | 大规模真实文件抽样、差分、性能 |
| T3 | Weekly/离线 | 全量设备、长 fuzz、广泛兼容 |

- 每个外部文件记录来源、许可、hash、可再分发性和访问策略。
- 调试/训练 corpus 与 release holdout 分开。
- 指标同时按文件、页面、feature 和去重页面访问统计。
- 私有文件默认不上传；只保存不可逆 hash、结构化 feature 和经授权最小样例。
- corpus 更新生成新的 manifest hash，发布结果必须绑定该 hash。

## 17. 性能测试

性能结果至少记录：commit、构建 profile、feature flags、编译器、OS、CPU/GPU、内存、浏览器、corpus hash、renderer/font/color epoch、cache 状态、样本数和原始样本。

- 区分 cold/warm、组件/用户路径、引擎时间/网络总时间。
- 报告 median、p95、p99 和置信区间，不只报告最快值。
- 优化必须同时运行 correctness diff、峰值内存和支持范围检查。
- 不得通过降低质量、扩大 unsupported 或改变输入集合获得性能通过。
- Native/PDFium 比较必须固定输入、输出、颜色、字体、缓存和可见区域。
- PR 噪声门槛和 release 门槛分开；release 只使用固定硬件池。

## 18. Flaky 与 quarantine

- 第一次失败必须保留；重试成功不能把原失败隐藏为绿色。
- 测试最多按 CI policy 有界重试，结果中单独标记 flaky。
- Quarantine 必须包含 owner、issue、原因、影响范围、到期日期和修复计划。
- Quarantine 默认不超过一个短发布周期；涉及安全、数据损坏、critical correctness 的测试不得 quarantine 后发布。
- 通过扩大 timeout/容差修复 flaky 必须提供环境或统计证据。

## 19. Failure bundle

失败包按适用范围包含：

```text
failure/
├── manifest.toml
├── minimized.pdf
├── feature-report.json
├── diagnostics.json
├── scene-native.json.zst
├── scene-reference.json.zst
├── native.png / baseline.png / diff.png
├── text-native.json / text-baseline.json
├── protocol-trace.json.zst
└── environment.json
```

Bundle 必须可内容寻址、可重放、可最小化并遵守隐私/许可。用户文件未经授权不得进入普通 CI artifact。

## 20. CI 准入

| Lane | 必跑内容 | 结果 |
| --- | --- | --- |
| Local | format、lint、受影响 unit/T0 | 提交前修复 |
| PR | 全 unit、T0、定向 T1、Reference diff、快速 perf | 阻断合并 |
| Merge | 跨模块 T1、browser/desktop smoke、外部差分 | 阻断主干 |
| Nightly | T2、长 fuzz、完整 E2E、性能池 | 自动 issue/owner |
| Weekly | T3、设备矩阵、provenance/license | 质量评审 |
| Release | 固定 ReleaseProfile、holdout、依赖纯度、回滚 | 阻断发布 |

CI 必须输出测试选择理由，避免“测试未运行”被误判为“通过”。

## 21. Feature Definition of Done

- [ ] 规范条款、Feature ID、research/provenance 已登记。
- [ ] 最小合法、非法、边界、参数组合和 recovery 测试齐全。
- [ ] Budget、取消、生命周期和错误路径已覆盖。
- [ ] Oracle 等级与推导可审核；golden 更新满足双人评审。
- [ ] Reference/Fast 及适用 GPU 差分通过。
- [ ] Fuzz target/seed/minimizer 已接入。
- [ ] Browser/desktop 适用 E2E 通过。
- [ ] 性能、内存和支持范围无未解释回归。
- [ ] failure bundle 可重放且不违反隐私/许可。
- [ ] CapabilityProfile 证据满足目标成熟状态门槛。
