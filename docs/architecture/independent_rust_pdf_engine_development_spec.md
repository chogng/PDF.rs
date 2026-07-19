# 独立 Rust PDF 引擎开发设计文档

**规范驱动、自主实现、浏览器/桌面双平台与独立验证**

- 文档编号：RPE-ARCH-001
- 版本：0.3
- 日期：2026-07-13
- 状态：稳定态架构基线 / 可执行开发规范（持续迭代）
- 核心约束：自主实现；产品运行时不依赖任何外部 PDF/2D 引擎；目录不使用 pdf- 前缀；PDFium 仅作开发/CI 黑盒基线

## 目录

- [1. 文档控制与核心决策](#1-文档控制与核心决策)
- [2. 产品目标、范围与成功标准](#2-产品目标范围与成功标准)
- [3. 自主实现、许可证与来源治理](#3-自主实现许可证与来源治理)
- [4. 总体架构与代码组织](#4-总体架构与代码组织)
- [5. 核心引擎：字节、语法、对象与文档](#5-核心引擎字节语法对象与文档)
- [6. 内容流虚拟机与 Scene 中间表示](#6-内容流虚拟机与-scene-中间表示)
- [7. 字体、文本、图片与颜色](#7-字体文本图片与颜色)
- [8. 渲染器：Reference CPU、Fast CPU 与 GPU](#8-渲染器reference-cpufast-cpu-与-gpu)
- [9. Runtime、调度、缓存与平台集成](#9-runtime调度缓存与平台集成)
- [10. 注释、表单与增量写入](#10-注释表单与增量写入)
- [11. 外部基线与 Native 能力成熟度](#11-外部基线与-native-能力成熟度)
- [12. 测试与质量工程体系](#12-测试与质量工程体系)
- [13. 安全、资源预算与可观测性](#13-安全资源预算与可观测性)
- [14. 公共 API、IPC 与错误模型](#14-公共-apiipc-与错误模型)
- [15. 开发流程、里程碑与发布治理](#15-开发流程里程碑与发布治理)
- [16. 风险、开放决策与验收标准](#16-风险开放决策与验收标准)
- [附录 A-F](#附录-a-操作符覆盖清单)

## 1. 文档控制与核心决策

### 1.1 文档目的

本文档把“独立 Rust PDF 引擎”从概念方案细化为可执行的工程规范。它定义产品稳定态边界、模块职责、核心数据结构、浏览器与桌面运行方式、测试与性能门槛、外部黑盒基线治理以及 Native 能力成熟机制。团队应把本文档作为架构评审、任务拆分、代码评审和发布准入的共同基线。

本文档描述目标稳定架构，而不是把阶段性脚手架固化为产品设计。开发过程中可以通过 ADR、实验分支和 roadmap 迭代局部算法与交付顺序；凡是会影响跨模块契约、可测性、许可证边界或产品运行时依赖的决策，均属于规范性要求，不得破坏本文档定义的架构不变量。

### 1.2 已确认的架构决策

**表 1-1 核心架构决策**

| 编号 | 决策 | 约束 |
| --- | --- | --- |
| AD-001 | 目录按职责命名，不使用 `pdf-` 前缀 | 例如 `core/syntax`、`runtime/cache`；发布包名可使用项目命名空间。 |
| AD-002 | 核心实现自主开发 | 不把 Hayro、Vello、PDF.js 或其他完整 PDF/2D 引擎作为核心生产依赖。 |
| AD-003 | 生产依赖按项目许可证兼容性审查 | 不预设 Apache-2.0 blanket deny；所有直接/传递依赖仍须进入 allowlist、SBOM 和再分发检查。 |
| AD-004 | 先建立测试系统，再扩大功能覆盖 | 任何新特性必须先有规范映射、最小样例、预期结果和回归策略。 |
| AD-005 | 先完成确定性 Reference CPU 渲染器 | Fast CPU 与 GPU 均以经 O0-O2 审核的 reference 输出和规范语义为基线。 |
| AD-006 | 浏览器使用 Rust/WASM + 薄 TypeScript 宿主 | TS 负责 DOM、Worker、Canvas、交互和可访问性；PDF 语义与渲染在 Rust。 |
| AD-007 | PDFium 仅为开发/CI 黑盒基线 | 不链接、不 vendoring、不随产品打包或下载；只允许由 `tools/baseline` 的进程级 runner 调用。 |
| AD-008 | 产品运行时只有 Native PDF 语义路径 | Unsupported 必须显式返回；不得在用户路径调用外部 PDF 引擎补齐能力。 |
| AD-009 | 发布范围由机器可读 ReleaseProfile 冻结 | P0/P1/P2 只表示 backlog 优先级；profile 固定 feature 组合、corpus、平台和数字门槛。 |
| AD-010 | Unsupported、预算、完整性和内部错误分类稳定 | 这些结果不得触发外部引擎；UI 按稳定错误/能力报告降级或提示。 |
| AD-011 | 浏览器产品包只包含 Native engine.wasm | surface 按同 Worker ABI、浏览器 transfer 和桌面 IPC 分层；只构建一套 Native Worker/Wasm 产品路径。 |
| AD-012 | Reference 是经审核实现基线，不是自动规范 oracle | Golden 声明 O0-O4 authority；Reference 输出只有经审核后才能成为 O3 regression golden。 |

> **不可破坏的不变量：** 产品构建和运行时依赖图不得包含 PDFium 或其他完整 PDF/2D 引擎；核心模块不得暴露浏览器对象或具体渲染后端类型；`Scene` 必须保留 PDF 透明度、颜色空间和字体语义；测试 golden 不得由外部基线或 Reference 首次输出自动生成后直接视为正确答案。

### 1.3 规范性术语

| 术语 | 含义 |
| --- | --- |
| MUST / 必须 | 违反即视为架构或质量缺陷，不得在正常评审中忽略。 |
| SHOULD / 应当 | 默认执行；偏离时需要在 ADR 或 PR 中说明理由和补偿措施。 |
| MAY / 可以 | 可选实现，不影响架构一致性。 |
| Native | 本项目自主实现的解析、文本、Scene 或渲染路径。 |
| Baseline | 仅在开发/CI 中运行的外部黑盒对照，不属于产品依赖或用户请求路径。 |
| Reference | 确定性、可解释、优先正确性的 CPU 实现基线；自身仍需 oracle 验证。 |
| Fast | 面向产品吞吐与交互延迟优化的 CPU 实现。 |
| Corpus | 用于回归、差分、性能和稳定性测试的 PDF 文件集合。 |

### 1.4 需求优先级

需求使用 P0、P1、P2 表示交付顺序。P0 是阅读器 MVP 与架构闭环所需能力；P1 是广泛兼容与生产成熟所需能力；P2 是专业工作流或后续产品能力。优先级不代表规范重要性，而代表产品切入顺序。

P0/P1/P2 是 backlog 排序，不等同于某次发布承诺。每次发布必须绑定一个机器可读的 `ReleaseProfile`；只有其中列出的 feature、组合、平台、corpus 和阈值才构成该版本的验收边界。文档中的“达到门槛”“目标 corpus”“目标设备”等表述，均以绑定的 `ReleaseProfile` 为准，不允许在发布评审时临时解释。

### 1.5 v0.3 修订摘要

- 主文档改为描述目标稳定态；阶段性迁移、实验和一次性脚手架进入 roadmap/ADR。
- 明确 PDFium 只属于开发/CI 黑盒 baseline；产品构建、发布与用户请求始终走 Native 路径。
- 生产许可证治理改为项目许可证兼容性 allowlist，不再预设 Apache-2.0 blanket deny。
- Unsupported、invalid、resource limit 与 internal fault 使用稳定分类，由 UI 明确处理，不调用外部引擎。
- 保留 ReleaseProfile、Range 状态机、多级 oracle、Surface 分层、缓存 identity 和确定性 fuel 等 v0.2 改进。

#### 1.5.1 2026-07-18 字体/文本阶段归属澄清

- M3 已验收的文字范围仅是嵌入简单字体的确定性水平字形绘制；后续出现的 WinAnsi、Type1C 和 Identity-H/CIDFontType2 实现原语，不自动扩大 M3，也不构成 M4 能力晋级。
- M4/M5 只交付 Fast CPU 与桌面/浏览器像素阅读闭环，不新增 CMap、ToUnicode、Unicode 语义、文本协议或文字交互能力。
- M6/R0 负责常见水平 CMap、ToUnicode/声明 encoding、水平字体闭环、TextAtom、选择、复制、搜索和链接；其可执行计划为 [`plan/m6.toml`](../../plan/m6.toml)。
- RTL、竖排 CIDFont、ActualText、Tagged PDF 及结构化可访问性属于独立的 Post-R0 Font/Text 里程碑 FT1；其可执行计划为 [`plan/post-r0-font-text.toml`](../../plan/post-r0-font-text.toml)。

### 1.6 配套工程规范

本架构文档定义稳定产品边界；具体编码、生命周期、测试、来源、安全预算和 Host/Engine 协议由以下配套规范执行：

| 规范 | 约束范围 |
| --- | --- |
| [代码规范](../standards/coding-standard.md) | Rust/TypeScript、错误、并发、unsafe、依赖和评审 |
| [生命周期与并发规范](../standards/lifecycle-and-concurrency.md) | Worker、Session、Request、Range、Surface、Cache 和 Save |
| [测试规范](../standards/testing-standard.md) | Oracle、golden、差分、fuzz、corpus、性能和 CI |
| [可溯源与来源治理规范](../standards/traceability-and-provenance.md) | 规范、feature、实现、测试、依赖、数据与发布证据 |
| [安全与资源预算规范](../standards/security-and-resource-budget.md) | 不可信输入、FuelBudget、沙箱、隐私和供应链 |
| [Engine 交互协议规范](../protocol/engine-protocol.md) | Browser/Desktop Host 与 Native Engine 的命令、事件和资源所有权 |

配套规范不得放宽本文档的不变量。任何冲突必须通过 ADR 和版本化修订解决，不能由局部实现静默选择。文档索引和变更治理见 [`docs/README.md`](../README.md)。

## 2. 产品目标、范围与成功标准

### 2.1 产品目标

- 构建一个**内存安全、可测试、可增量演进**的 Rust-first PDF 引擎，同时支持桌面和现代浏览器。
- 在目标阅读场景中，通过 tile、任务取消、Scene 缓存、按需解析和低复制输出，取得优于 PDFium 的产品级延迟与内存表现。
- 把正确性从“看起来能显示”提升为规范条款、语义 Scene、文本结构、像素输出和真实语料均可追溯验证。
- 让 PDFium 等外部引擎从第一天起只存在于测试工具边界，不进入产品架构、公共 API 或发布产物。
- 形成自主的测试生成器、reference renderer、差分工具和 corpus 管理体系，作为长期技术壁垒。

### 2.2 非目标

- 首个生产版本不承诺完整 Acrobat 级编辑、预检、印前、动态 XFA、3D、多媒体或任意 PDF JavaScript。
- 不以“逐行翻译 PDFium”为实施方法；外部实现可用于行为观察、性能基线和问题分类，但不得形成代码级依赖。
- 不把 GPU 作为首版正确性前提；GPU 是经过 reference/fast CPU 验证后的加速后端。
- 不追求所有微基准均优于 PDFium；目标是首屏、滚动、缩放、随机跳页、文本体验、内存与稳定性综合胜出。
- 不在产品运行时调用、嵌入、下载或拼接 PDFium/其他外部引擎输出；外部输出仅保存在差分测试 artifact 中。

### 2.3 功能范围

| 领域 | P0 | P1 | P2 / 明确延后 |
| --- | --- | --- | --- |
| 文件与语法 | 接受 PDF 1.0-2.0 header 与核心对象语法；xref table/stream、object stream、增量修订、常见修复 | Linearization、加密、更多损坏文件恢复 | 专业校验器、完整预检 |
| 显示 | 路径、文字、图片、裁剪、基础透明、基础渐变、旋转和缩放 | 复杂透明组、soft mask、Pattern、DeviceN、ICC、overprint | 高端印前仿真 |
| 文本 | 提取、选择、复制、搜索、链接 | RTL、竖排 CJK、ActualText、Tagged PDF、可访问性 | 高级重排与编辑 |
| 交互 | 目录、页标签、基本注释显示 | 高亮、下划线、墨迹、文本框、基础 AcroForm | XFA、PDF JavaScript、多媒体 |
| 保存 | 注释 sidecar、变更集 | 增量保存、表单值保存 | 任意内容编辑、完整重写优化 |
| 平台 | 桌面 Native worker；浏览器 Rust/WASM worker | WebGPU、共享纹理、移动端优化 | 服务端批处理和打印栈 |

### 2.4 产品成功与外部基线比较

产品成功必须按用户结果定义，而不是按语言、代码行数或单个整页 raster benchmark 定义。PDFium 只是其中一个可复现的外部性能/行为基线，不是产品依赖或规范真值。项目应同时维护正确性、稳定性、性能、文本与可访问性五类记分板。

| 维度 | 核心指标 | 成熟阶段目标 |
| --- | --- | --- |
| 正确性 | 严重视觉缺陷率、文本映射准确率、规范用例通过率 | 目标产品范围内无已知 P0 规范缺陷；Native 页面覆盖率持续提升 |
| 稳定性 | panic、hang、worker crash、预算超限 | 发布 corpus 上为 0；恶意输入稳定返回可分类错误 |
| 交互性能 | 首个可视 preview、全质量 tile、滚动 p95、缩放重栅格化 | 按 ReleaseProfile：至少两个主要路径 p95/PDFium ≤ 0.85，其他主要路径 ≤ 1.05（成熟阶段 ≤ 1.00） |
| 资源效率 | 峰值内存、像素复制、网络请求数、缓存命中 | 在目标设备类上优于或等于 PDFium，并遵守硬预算 |
| 文本与无障碍 | 选择几何、复制结果、阅读顺序、结构树 | 在自有高价值 corpus 上达到发布门槛，并可解释低置信度结果 |

### 2.5 非功能性要求

| 类别 | 要求 |
| --- | --- |
| 安全 | 解析不可信 PDF 必须位于资源预算内；桌面运行在沙箱 worker；浏览器重计算不得阻塞主线程。 |
| 可移植 | 核心模块不得依赖文件系统、窗口系统或 JS 类型；必须能编译到桌面目标与 `wasm32`。 |
| 可观测 | 每个 unsupported、预算终止、恢复路径和性能阶段都必须有结构化原因和指标。 |
| 可复现 | Reference 输出、规范用例和性能环境必须可版本化；测试结果必须包含构建、硬件与 corpus 指纹。 |
| 产品依赖纯度 | 产品依赖图、构建产物和运行时网络请求中不得包含外部完整 PDF/2D 引擎。 |
| 许可证治理 | 生产依赖、测试工具、语料和生成数据分别审查；核心禁止来源不清的翻译或常量表。 |

### 2.6 ReleaseProfile R0：首个可用版本的固定边界

“接受 PDF 1.0-2.0”只表示能够识别对应 header、版本覆盖规则和核心对象语法，不表示 Native 已实现 PDF 2.0 的全部 feature。Native 能力判定必须基于实际所需能力及其组合，不能只看文件版本号。

实现仓库 MUST 保存 `release/profiles/r0.toml`；下面的配置是 v0.2 的规范默认值。修改范围或阈值必须通过 ADR，并同时更新 profile schema、corpus manifest 和发布说明。

**表 2-1 R0 Native 能力边界**

| 领域 | R0 Native MUST | R0 明确 Unsupported/延后 |
| --- | --- | --- |
| 文件 | xref table/stream、hybrid、object stream、读取最新增量修订、R0/R1 repair | 加密、R2 repair、需要执行动作的文档 |
| Filter/图片 | ASCIIHex、ASCII85、RunLength、Flate + predictor、LZW；经批准叶子 codec 提供 DCT/JPEG | CCITT、JPX、JBIG2 及未知 filter |
| 图形 | 路径 fill/stroke、dash、clip、Image/Form XObject、DeviceGray/RGB/CMYK、alpha、Normal/Multiply/Screen blend、axial/radial shading | soft mask、Pattern、DeviceN、ICC、overprint、knockout、复杂 transparency group |
| 字体 | 嵌入 Type 1、TrueType/OpenType glyf、CFF、常见水平 CIDFont；Type 3 仅在 capability profile 明确开放 | 非嵌入字体的不可控替代、竖排 CIDFont、未验证 Type 3 组合 |
| 文本 | ToUnicode/声明 encoding、水平 LTR、quad、选择、复制、搜索、链接、outline | RTL、竖排、ActualText、Tagged PDF 阅读顺序和低置信度未知映射 |
| 注释/表单/保存 | 基本注释 appearance 显示；sidecar ChangeSet | AcroForm 交互、JavaScript/XFA、增量保存 |
| 平台 | 桌面隔离 worker；浏览器 Dedicated Native Worker；Range/无 Range；CPU surface | GPU 不是 R0 准入条件；SharedArrayBuffer 仅为可选优化 |

R0 对超出 Native 能力的输入返回结构化 `UnsupportedCapability`；UI 可以显示已支持的文档级信息、问题页面占位和具体缺失能力，但不得调用外部 PDF 引擎。Capability 判定不得把超出上表的页面误判为可安全处理。Feature 组合由 `CapabilityProfile` 表达，例如 `soft-mask(luminosity) AND knockout AND ICCBased`，不得只用三个互不关联的布尔标签晋级。

**表 2-2 R0 固定验收阈值**

| 指标 | R0 门槛 |
| --- | --- |
| Release corpus | `release-r0-v1`，至少 1,000 个文件、10,000 页；20% holdout 从未用于调试；manifest 与内容哈希固定 |
| 正确性 | 原子规范用例 critical/major 缺陷为 0；eligible holdout 页面 critical 为 0、major 页面率 ≤ 0.10% |
| 稳定性 | T0/T1/release corpus 的 panic、hang、worker crash 为 0；所有恶意样例在预算或 watchdog 内终止 |
| 能力判定 | release corpus 中危险 capability false-positive 为 0；in-profile 页面运行时 unexpected-unsupported 加权率 ≤ 0.50% |
| 产品支持覆盖 | 全部 release corpus 按页面访问加权的 Native supported rate ≥ 75%；其余页面 100% 返回结构化 unsupported，不崩溃、不输出未标记残缺结果 |
| 性能 | 同设备、同输出、同缓存状态下，所有主要路径 Native p95/PDFium p95 ≤ 1.05；至少两个路径 ≤ 0.85 |
| 内存 | Native 峰值内存/PDFium baseline 峰值内存 ≤ 1.00；比较仅在离线 benchmark runner 中执行 |
| 取消 | 取消请求到停止消耗 CPU 的 p95 ≤ 50 ms、p99 ≤ 100 ms；旧 generation 显示次数为 0 |
| 浏览器 | 固定 CI 镜像中的 Chromium、Firefox、WebKit 三引擎通过；具体版本和镜像 digest 写入 profile |

**接口 2-1 `release/profiles/r0.toml` 最小 schema**

```text
schema = 1
id = "release-r0-v1"
spec_version = "0.2"
status = "candidate"
capability_profiles = [
  "r0.syntax.v1",
  "r0.graphics.basic.v1",
  "r0.font.horizontal.v1",
  "r0.text.horizontal-ltr.v1",
]

[corpus]
manifest = "corpus/release-r0-v1/manifest.toml"
hash = "REQUIRED_BEFORE_RC"
min_files = 1000
min_pages = 10000
holdout_ratio = 0.20

[gates]
critical_page_rate = 0.0
major_eligible_page_rate = 0.001
dangerous_capability_false_positive = 0
in_profile_unexpected_unsupported_rate = 0.005
weighted_native_supported_rate_min = 0.75
native_p95_ratio_max = 1.05
winning_path_p95_ratio_max = 0.85
winning_path_count_min = 2
native_peak_memory_ratio_max = 1.00
cancellation_p95_ms = 50
cancellation_p99_ms = 100

[benchmark]
hardware_pool = "REQUIRED_BEFORE_RC"
primary_paths = [
  "cold-first-preview",
  "first-full-quality-viewport",
  "continuous-scroll",
  "pinch-zoom",
  "random-jump",
  "search-first-result",
]

[platform.browser]
engines = ["chromium", "firefox", "webkit"]
image_digests = [] # release blocking until pinned
```

Profile loader 必须拒绝未知 mandatory 字段、缺失 hash/硬件池、空 release browser digest、重复 capability profile 和超出 schema 支持范围的版本。`status = "candidate"` 的 profile 不得生成正式 release artifact。

阈值按固定硬件池测量，网络场景同时报告“引擎耗时”和“包含网络耗时”。样本不足时不得用单个百分比判定，必须报告置信区间和原始样本数。

严重度和分母固定如下：`critical` 包括安全边界突破、panic/hang/crash、整页或关键内容缺失、错误执行动作、数据损坏保存；`major` 包括影响阅读/选择/搜索的成块文字或图片缺失、显著几何/透明/颜色错误、错误阅读顺序。页面率按“至少一个对应缺陷的唯一页面数 / eligible 唯一页面数”计算，不按差异区域数量重复计数。“危险 capability false-positive”指能力误判后产生 critical、ResourceLimit、InternalFault 或发布未标记的错误像素；普通静态漏判按 unexpected-unsupported 统计。页面访问加权使用去重 session-page view，并在 profile 中固定抽样窗口和匿名化规则。

## 3. 自主实现、许可证与来源治理

### 3.1 核心政策

项目采用“规范与测试驱动的独立实现”策略。Hayro、Vello、PDF.js、MuPDF、PDFium 等项目可以作为外部行为样本、性能对照或研究对象，但核心生产代码不得直接依赖其完整引擎，也不得逐行翻译其实现。

> **许可证策略：** 项目自身源码使用由项目所有者批准的开源许可证。第三方生产依赖按与该许可证、产品分发方式和组织政策的兼容性逐项加入 allowlist，不对 Apache-2.0 设置 blanket deny，也不因“仅为传递依赖”跳过审查。许可证合规、来源治理和架构可替换性分别评估。

开发/测试工具与产品依赖分开建账。PDFium 仅由开发者本机或 CI 的外部 runner 使用，不进入产品源码依赖、发布包或产品 SBOM；`tools/baseline/pdfium` 只保存调用适配、版本/构建指纹和下载说明，不 vendoring PDFium 源码或二进制。若团队分发包含 PDFium 的 CI image、SDK 或测试工具包，该分发物仍须附带 PDFium 及其实际构建闭包要求的许可证材料 [R3][R4]。

#### 3.1.1 自主实现的 ownership 边界

“自主 PDF 引擎”要求项目拥有 PDF 语义和后端决策，不要求重复实现所有通用密码学、压缩和图像算法。边界如下：

| 类别 | 默认策略 | 约束 |
| --- | --- | --- |
| PDF 语义核心 | 必须自主实现 | syntax、xref/object、修订链、document、Content VM、graphics state、Scene、文本映射、PDF 合成语义和 capability/error policy |
| Reference/Fast raster | 必须自主实现 | 几何、coverage、clip、group/mask/compositing 的语义和测试 oracle 归本项目所有 |
| 密码学原语 | 优先使用经审查叶子库 | 不自行设计 AES/哈希/随机数；PDF Security Handler 的参数派生、对象边界和权限语义由本项目实现 |
| 压缩与图片 codec | 可以使用经审查叶子库 | Flate、JPEG、JPX、JBIG2、CCITT 等必须 trait 隔离、受 budget/cancellation 控制，且不得携带 PDF 对象模型或渲染器 |
| 字体与 ICC 叶子能力 | 条件允许 | 可使用 table/outline/profile 级库；PDF font/CMap/positioning/color-space 语义仍由本项目控制 |
| 完整文档/2D 引擎 | 产品禁止 | Hayro、PDF.js、MuPDF、PDFium、Vello 等不得进入产品依赖图；经批准的外部引擎只可作为进程级开发/CI baseline |

每个候选依赖必须在 ADR 中标记 `semantic_owner`、`failure_isolation`、`budget_hook`、`cancellation_hook`、`license_decision`、`replacement_plan` 和 Wasm/Native 支持状态。缺少 budget 或 cancellation 接口的 codec 不得直接处理不可信输入；必须置于可终止的隔离 worker，或由 adapter 分块调用并执行硬上限。

### 3.2 允许和禁止的参考方式

| 活动 | 允许性 | 要求 |
| --- | --- | --- |
| 阅读 ISO 规范、勘误、公开论文和标准算法 | 允许 | 记录具体条款、版本和链接。 |
| 运行外部引擎观察输出 | 允许 | 作为差分样本，不把单一引擎当作规范真值。 |
| 阅读外部项目架构并写研究笔记 | 允许 | 笔记描述问题、约束和思路，不粘贴实现代码。 |
| 复制或机械翻译函数、状态机、常量表 | 禁止 | 除非来源属于明确允许的标准数据并由生成器重建。 |
| 使用外部测试 PDF | 条件允许 | 逐文件记录来源、许可、哈希和可再分发条件。 |
| 使用外部完整渲染器作为产品后端 | 禁止 | PDFium 等仅可由 `tools/baseline` 进程级 runner 在开发/CI 中调用。 |
| 使用叶子级通用库 | 条件允许 | 通过许可证、供应链和可替换性审查；不得泄漏类型到公共 API。 |

### 3.3 Provenance 记录

每个核心模块必须维护 `PROVENANCE.md`，并在重要实现 PR 中引用。最少包含：规范条款、采用算法、外部行为观察、生成数据来源、测试用例编号、许可证结论和已知偏差。

**示例：模块来源记录**

```text
core/content/PROVENANCE.md

# Scope
Content stream parser and graphics-state interpreter.

# Normative sources
- ISO 32000-2:2020, clauses 8, 9, 10, 11.
- Errata snapshot: 2026-06 / collection 3.

# Algorithms
- Operand stack: independently designed.
- Matrix operations: standard affine math.

# External behavior observations
- PDFium and two other processors were used only for differential runs.
- No implementation source was copied or translated.

# Tests
- spec/content/q-q-balance-001
- spec/text/tj-array-spacing-004
- regression/issue-00317
```

### 3.4 依赖审查与供应链

- 生产、开发、测试、工具链和语料依赖必须分开生成 SBOM；不能用“仅开发依赖”掩盖测试数据的再分发义务。
- Cargo lockfile 变更必须经过自动许可证检查；不在项目 allowlist、GPL/AGPL、未知许可证和自定义许可证默认阻断。Apache-2.0 是否允许由项目许可证和组织政策决定，不在本架构文档中 blanket deny。
- 允许的叶子依赖必须由本项目 trait 隔离，并提供替换计划、版本锁定、漏洞扫描和最小 feature 配置。
- 字体映射、CMap、颜色配置和编码表等数据必须由生成脚本从已批准来源生成；仓库保存生成器、输入哈希和输出哈希。
- 外部 corpus 大文件宜存放在对象存储，仓库只保存 manifest、哈希和下载策略，避免无意再分发。

## 4. 总体架构与代码组织

### 4.1 系统上下文

**图 4-1 系统上下文**

```text
┌──────────────────────────────────────────────────────────────┐
│                       Product UI                             │
│  browser: TypeScript/DOM     desktop: Web UI or native UI   │
└───────────────────────────┬──────────────────────────────────┘
                            │ versioned protocol
                            ▼
┌──────────────────────────────────────────────────────────────┐
│                         Runtime                              │
│ session | scheduler | cache | budget | capability policy    │
└───────────────────────────┬──────────────────────────────────┘
                            ▼
┌──────────────────────────────────────────────────────────────┐
│                        Native Core                           │
│ bytes → syntax → object → document → content → scene        │
│ text/structure → Reference/Fast CPU/GPU                     │
└──────────────────────────────────────────────────────────────┘

Development / CI boundary only (never linked to Product Runtime):
tests/corpus ─► tools/baseline protocol ─► PDFium/other process
                     │
                     └─► observations/diffs, never product pixels
```

### 4.2 Workspace 目录

**图 4-2 推荐目录结构**

```text
/
├── core/
│   ├── bytes/
│   ├── syntax/
│   ├── xref/
│   ├── object/
│   ├── filters/
│   ├── security/
│   ├── document/
│   ├── content/
│   ├── graphics/
│   ├── font/
│   ├── image/
│   ├── color/
│   ├── text/
│   ├── structure/
│   ├── annotation/
│   ├── form/
│   ├── scene/
│   ├── raster/
│   │   ├── reference/
│   │   └── fast/
│   ├── gpu/
│   └── write/
├── runtime/
│   ├── engine/
│   ├── session/
│   ├── schedule/
│   ├── cache/
│   ├── budget/
│   └── policy/
├── platform/
│   ├── browser/
│   ├── desktop/
│   ├── ipc/
│   └── surface/
├── tools/
│   ├── inspect/
│   ├── generate/
│   ├── compare/
│   ├── baseline/
│   │   ├── protocol/
│   │   └── pdfium/       # adapter/config only; no vendored engine
│   ├── minimize/
│   ├── benchmark/
│   ├── trace/
│   └── corpus/
└── tests/
    ├── cases/
    ├── generated/
    ├── corpus/
    ├── browser/
    ├── fuzz/
    ├── performance/
    ├── recovery/
    └── expected/
```

目录不使用 `pdf-` 前缀。若需要发布多个 Cargo package，可使用项目命名空间，例如 `<project>-syntax`、`<project>-scene`，但物理目录仍保持简洁职责名。

### 4.3 模块职责

| 模块 | 主要职责 | 禁止事项 |
| --- | --- | --- |
| bytes | 统一随机读取、RangeStore、数据完整性与来源版本 | 不得解析 PDF 对象 |
| syntax/xref/object | 词法、对象、xref 修订链、对象解析与恢复 | 不得依赖页面、字体或渲染 |
| filters/security | 流过滤器、predictor、解密和预算 | 不得直接访问 UI 或全局网络 |
| document | Catalog、page tree、资源继承、name tree、outline | 不得生成像素 |
| content/graphics | 操作符 VM、图形状态、XObject、标记内容 | 不得绑定具体 raster backend |
| scene | 不可变、可序列化、保留 PDF 语义的中间表示 | 不得包含外部引擎/Vello/浏览器类型 |
| font/text | 字形与 Unicode 双链、选择、搜索、结构语义 | 不得依赖系统排版决定 PDF 中已定位字形 |
| raster/reference | 确定性、解释性正确性基线 | 不得使用近似快速路径 |
| raster/fast | tile、SIMD、多线程、缓存和交互优化 | 不得改变 Scene 语义 |
| gpu | WebGPU/原生 GPU 加速 | 不得成为唯一正确性实现 |
| runtime | session、调度、缓存、预算、capability 与 Native renderer 选择 | 不得解析具体 PDF 语法或调用外部 PDF 引擎 |
| tools/baseline | 进程级外部引擎启动、输入/输出标准化、版本指纹和差分采集 | 不得成为 product feature、runtime dependency 或 golden oracle |

### 4.4 依赖方向

```text
bytes
  └─ syntax
      ├─ xref
      └─ object ── filters/security
                    └─ document
                        ├─ font/image/color
                        ├─ content/graphics
                        │   └─ scene
                        │       ├─ raster/reference
                        │       ├─ raster/fast
                        │       └─ gpu
                        ├─ text/structure
                        └─ annotation/form/write

runtime depends on stable core interfaces.
platform depends on runtime and protocol.
tools/baseline depends on exported test/protocol schemas only; product crates never depend on tools.
```

### 4.5 架构边界规则

- 核心 crate MUST 不直接依赖 async runtime；异步网络和事件循环在 platform/runtime 层收敛。
- 核心 crate MUST 不调用 `std::fs`、DOM、Canvas、WebGPU 或平台窗口 API；本地文件通过 `ByteSource` 注入。
- 除明确批准的 FFI/surface/GPU 边界外，核心 crate SHOULD 使用 `#![forbid(unsafe_code)]`。
- 公共 API 只暴露本项目定义的 ID、值对象、错误和 trait，不暴露第三方依赖类型。
- Scene 创建后不可变；渲染 worker 只读取 Scene 和资源快照，不持有 document 全局锁。
- 所有跨进程、跨线程和跨 Wasm 边界的数据结构必须有版本号、大小上限和未知字段处理规则。
- Release CI 必须从产品构建产物、Cargo/包管理依赖图、Wasm imports、动态库列表和网络清单中验证不存在 PDFium 或其他完整外部 PDF/2D 引擎。

## 5. 核心引擎：字节、语法、对象与文档

### 5.1 ByteSource 与 RangeStore

核心解析器不区分本地文件和远程文件。它只通过可暂停的同步读取语义访问已可用区间；网络层负责异步下载、验证 source snapshot 并填充 `RangeStore`。Parser 不依赖 async runtime，但任何可能缺数据的 job 都必须能在显式 checkpoint 暂停，由 runtime 在 ticket 完成后重新调度。

**接口 5-1 字节读取契约**

```text
pub trait ByteSource: Send + Sync {
    fn snapshot(&self) -> SourceSnapshot;
    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice>;
}

pub enum ReadPoll<T> {
    Ready(T),
    Pending {
        ticket: DataTicket,
        missing: SmallRanges,
    },
    EndOfFile,
    Failed(SourceError),
}

pub struct SourceSnapshot {
    pub identity: SourceIdentity,
    pub len: Option<u64>,
    pub validator: SourceValidator,
}

pub struct SourceIdentity {
    pub stable_id: [u8; 32],
    pub revision: u64,
}

pub struct ReadRequest {
    pub range: ByteRange,
    pub priority: RequestPriority,
    pub job: JobId,
    pub checkpoint: ResumeCheckpoint,
}
```

`SourceSnapshot` 在 session 生命周期内不可变。HTTP source 使用 strong ETag/`If-Range` 或等价强验证器；本地文件使用宿主提供的稳定 file identity、长度和 revision。若只能获得弱验证器，宿主必须把已下载响应绑定为不可变 snapshot；检测到内容变化时终止旧 session，并以 `SourceChanged` 重新打开。禁止仅清缓存后继续旧 job，也禁止把不同 revision 的区间拼成同一文档。

`ByteSlice` 必须拥有或引用带 snapshot identity 的稳定 backing storage；它不得借用会在回调返回后移动的临时 JS/FFI buffer。所有 `start + len`、slice offset 和 platform size 转换执行溢出检查。

**状态机 5-1 Range 暂停与恢复**

```text
job calls ByteSource.poll(request, checkpoint)
  ├─ Ready(bytes) ───────────────► continue from current state
  ├─ EndOfFile / Failed ─────────► stable classified error
  └─ Pending(ticket, ranges)
         │
         ├─ job serializes/owns ResumeCheckpoint
         ├─ runtime deduplicates and schedules ranges
         ├─ source validates response against SourceSnapshot
         ├─ ticket becomes Ready / Failed / SourceChanged
         └─ runtime requeues job(checkpoint); never resumes inline
```

每个 ticket 只能完成一次；取消 job 会解除订阅，但不必取消仍被其他 job 共享的 Range 请求。数据到达、取消和 close 竞态由 runtime 按 `session + job + generation` 仲裁。Parser 可以从 checkpoint 恢复，也可以从已声明的幂等阶段边界重试；不得隐式从任意深层调用栈重新开始整个文档解析。

### 5.2 Range 合并与下载策略

- 区间请求必须经过合并器；重叠或间距小于阈值的请求合并，阈值按网络 RTT 和文件大小配置。
- 优先级依次为：当前可视页关键对象、首屏字体/图片、相邻页、目录/搜索、后台预取。
- 线性化 hint 只作为优化提示，必须验证边界、对象引用和长度，不能信任其安全性或正确性。
- 不支持 HTTP Range 时允许回退为完整下载，但必须记录原因和首屏字节成本。
- ByteRange 使用 64 位无符号偏移，并对 `start + len` 做显式溢出检查。
- Range 响应若与 snapshot validator 不一致，丢弃该响应并以 `SourceChanged` 终止 session；不得把错误解释为普通网络重试。
- `DataTicket` 完成只负责唤醒 runtime；解析工作不得在网络、JS 或 FFI 回调栈中执行。

外部 baseline runner 接收完整、已固定 hash 的测试文件，不共享产品 `RangeStore`，也不参与用户 Range 请求。Range/渐进读取正确性由 Native 的生成式测试和网络 E2E 独立验证，禁止用外部引擎的请求序列定义 Native 行为。

### 5.3 词法与对象模型

语法层负责 header、空白、注释、数字、name、literal string、hex string、array、dictionary、indirect reference 和 stream 边界。对象模型必须同时支持语义访问和原始字节追踪，以服务错误诊断与增量写入。

**接口 5-2 对象模型核心类型**

```text
pub enum Object {
    Null,
    Bool(bool),
    Int(i64),
    Real(Real),
    Name(NameId),
    String(StringObject),
    Array(ArrayId),
    Dict(DictId),
    Stream(StreamId),
    Ref(ObjectRef),
}

pub struct ObjectRef {
    pub number: u32,
    pub generation: u16,
}

pub struct Located<T> {
    pub value: T,
    pub span: ByteSpan,
    pub revision: RevisionId,
}
```

`Real` 不直接等同于 `f64`。解析层必须保存原值是否为整数、指数格式和异常范围；数学层可转为经过范围检查的浮点或定点表示。增量 writer 如未修改对象，应尽可能保留原始字节而不是重新格式化。

### 5.4 xref 与增量修订

- 必须支持传统 xref table、xref stream、hybrid-reference、object stream 和多次增量更新。
- 解析顺序从最后一个 `startxref` 反向建立修订链；同一对象号采用最新有效定义。
- xref 条目在使用前验证 offset、generation、object number 和对象头，不允许盲目信任。
- 对象 resolver 使用 `Unresolved / Resolving / Ready / Failed` 状态检测循环引用；错误中保留引用链。
- 修复模式和严格模式分离。严格模式服务规范测试；宽容模式服务真实文件，但任何修复必须生成 diagnostic。

```text
pub enum ResolveState<T> {
    Unresolved,
    Resolving { stack_id: u32 },
    Ready(Arc<T>),
    Failed(Arc<ResolveError>),
}

pub struct Revision {
    pub id: RevisionId,
    pub startxref: u64,
    pub trailer: DictId,
    pub previous: Option<RevisionId>,
}
```

### 5.5 修复策略

| 级别 | 行为 | 边界 |
| --- | --- | --- |
| R0 严格 | xref 或对象结构不符合规范即失败 | 用于规范测试和 writer 验证 |
| R1 局部修复 | 校正空白、长度、轻微 offset 偏差 | 必须有限扫描并记录修复 |
| R2 对象扫描 | 在限定范围扫描 `obj` 头并重建候选表 | 受字节数、对象数和时间预算限制 |
| R3 拒绝 | Native 无法在预算内可靠恢复 | 返回结构化 `RecoveryFailed`；不得输出未标记的部分文档 |

### 5.6 Stream filters 与解码预算

过滤器编排、参数语义、budget、取消和 PDF 对象边界由本项目自主实现。ASCIIHex、ASCII85、RunLength、predictor 和 LZW 优先自主实现；Flate、DCT、CCITT、JPX、JBIG2 可按第 3.1.1 节使用经批准叶子 codec。尚未实现或未经批准的过滤器触发 `UnsupportedFilter`，不引入完整外部 PDF 引擎。

| 过滤器/编码 | 阶段 | 必须测试的风险 |
| --- | --- | --- |
| ASCIIHex / ASCII85 / RunLength | P0 | 终止符、奇数字节、过长输入、非法字符 |
| Flate + PNG/TIFF predictor | P0 | 解压炸弹、行宽溢出、错误 predictor 参数 |
| LZW | P0/P1 | EarlyChange、字典重置、非法码、输出预算 |
| DCT/JPEG | P0/P1 | 色彩分量、CMYK/YCCK、截断、巨幅尺寸 |
| CCITT | P1 | K 参数、黑白极性、行边界和损坏码流 |
| JPX/JPEG 2000 | P1 | tile、alpha、色彩空间、巨量分辨率 |
| JBIG2 | P1 | 全局段、引用循环、symbol 数量、历史安全风险 |

### 5.7 Security Handler

安全模块只负责 PDF Security Handler、密码派生、对象级解密和权限信息，不负责 UI 策略。密码由 platform 传入 session 的短生命周期安全缓冲区，不进入日志、panic、trace 或持久缓存。

Security Handler 的 PDF 参数派生和对象边界由本项目实现；AES、哈希、随机数等密码学原语必须来自经审查的密码库，除非独立密码学评审明确批准自行实现。测试向量必须覆盖规范算法组合和错误参数，但不得把测试通过表述为密码学安全审计。

- 支持能力必须按具体 Revision、Version、CF 和算法组合建模，不能只有“encrypted=true”。
- 解密发生在对象流/字符串消费边界，并纳入字节预算和取消检查。
- 未知或不允许的密码算法返回 `UnsupportedSecurityFeature`；UI 可以提示能力限制或密码策略，但不得调用外部引擎。
- 数字签名验证与加密解密是独立子系统；不能因为能打开加密文档就声称支持签名。

### 5.8 文档模型

`document` 层提供稳定、惰性的逻辑视图：Catalog、page tree、page labels、outline、destination、name tree、attachments、metadata、optional content 和结构树入口。对象只在需求触发时解析，不在打开文档时构建完整对象图。

```text
pub struct DocumentSession {
    source: Arc<dyn ByteSource>,
    revisions: RevisionChain,
    resolver: ObjectResolver,
    catalog: OnceResult<Catalog>,
    page_index: PageIndex,
    diagnostics: DiagnosticSink,
}

pub struct PageHandle {
    pub index: u32,
    pub object: ObjectRef,
    pub inherited: InheritedResources,
    pub boxes: PageBoxes,
    pub rotation: Rotation,
}
```

### 5.9 Page tree 与资源继承

- Page tree 遍历必须防止循环、重复 kid、错误 Count、过深层级和非 Page/Pages 对象。
- 页索引使用惰性分段结构；只解析用户需要的区间，并缓存已验证的节点摘要。
- MediaBox、CropBox、Rotate、Resources 等继承值在 `PageHandle` 中物化，避免每次渲染重复向上查找。
- 资源字典查找保留作用域链和来源对象，便于错误报告与 Scene resource 去重。

## 6. 内容流虚拟机与 Scene 中间表示

### 6.1 Content VM

内容流不是直接画像素，而是由操作符 VM 解释为保留语义的 Scene。VM 持有 operand stack、graphics-state stack、text state、resource scopes、marked-content stack 和 budget context。

```text
pub struct Interpreter<'a> {
    operands: OperandStack,
    graphics: GraphicsStack,
    text: TextState,
    resources: ResourceScope<'a>,
    marked: MarkedContentStack,
    builder: SceneBuilder,
    budget: &'a mut Budget,
    diagnostics: &'a dyn DiagnosticSink,
}
```

### 6.2 操作符执行规则

- 操作符分发表必须显式定义 operand 个数、类型、允许上下文、预算成本和错误恢复方式。
- `q/Q`、`BT/ET`、marked content 和 compatibility sections 必须维护结构平衡；宽容模式可在流结束时恢复，但要记录。
- Form XObject 递归执行时创建资源作用域和变换快照，并限制递归深度、总操作数和重复引用。
- Inline image 的 `EI` 识别不能只做简单字符串搜索；必须结合过滤器、长度候选和语法验证。
- 未知操作符在 `BX/EX` 内可忽略；其他位置按严格/宽容策略处理。

### 6.3 图形状态

| 状态组 | 主要字段 |
| --- | --- |
| 几何 | CTM、current path、clip stack、flatness、stroke adjustment |
| 线条 | width、cap、join、miter、dash |
| 颜色 | stroking/nonstroking color space 与 components、rendering intent、overprint |
| 透明 | alpha、blend mode、soft mask、alpha-is-shape、text knockout |
| 文本 | font、size、char/word spacing、horizontal scale、leading、rise、render mode、text matrices |
| 外部状态 | ExtGState 来源、transfer/halftone 等当前阶段支持状态 |

### 6.4 Scene 设计目标

- 不可变：生成后只读，可跨 worker 安全共享。
- 保留语义：透明组、soft mask、Pattern、Type 3、颜色空间、optional content 不得过早扁平化。
- 可裁剪：每条 command 和 group 有保守 bounds，支持 tile 跳过与局部重放。
- 可序列化：提供稳定 canonical 格式用于 scene diff、缓存、trace 和跨进程传输。
- 后端无关：CPU、GPU、SVG/debug exporter 使用相同 Scene，不泄漏具体 renderer 类型。
- 可诊断：command 能追溯到内容流、对象引用、操作符序号和 marked-content 信息。

**接口 6-1 Scene 核心结构**

```text
pub struct Scene {
    pub version: SceneVersion,
    pub page_size: Size,
    pub commands: Arc<[Command]>,
    pub resources: Arc<ResourceStore>,
    pub spatial: SpatialIndex,
    pub features: FeatureReport,
    pub provenance: Arc<[CommandSource]>,
}

pub enum Command {
    Fill(FillCommand),
    Stroke(StrokeCommand),
    GlyphRun(GlyphRunCommand),
    Image(ImageCommand),
    Shading(ShadingCommand),
    PushClip(ClipCommand),
    PopClip,
    BeginGroup(GroupCommand),
    EndGroup,
    BeginOptionalContent(OptionalContentId),
    EndOptionalContent,
}
```

### 6.5 ResourceStore

路径、字体、图片、颜色空间、函数、shading、pattern 和 mask 使用稳定 ID 引用。ID 在单个 Scene 中按 canonical 遍历分配；跨页缓存使用内容哈希和解析上下文键，避免把只看字节相同但语义环境不同的资源错误合并。

```text
pub struct ResourceKey {
    pub source: SourceIdentity,
    pub object: ObjectRef,
    pub revision: RevisionId,
    pub decode_context: DecodeContextHash,
}

pub enum Resource {
    Path(PathData),
    Font(FontResource),
    Image(ImageResource),
    ColorSpace(ColorSpace),
    Function(FunctionResource),
    Pattern(PatternResource),
    Mask(MaskResource),
}
```

### 6.6 FeatureReport 与能力判定

**接口 6-2 能力报告**

```text
pub struct FeatureReport {
    pub tags: FeatureSet,
    pub requirements: Vec<CapabilityRequirement>,
    pub decision: CapabilityDecision,
    pub unsupported: Vec<UnsupportedFeature>,
    pub warnings: Vec<CompatibilityWarning>,
    pub complexity: SceneComplexity,
    pub text_confidence: ConfidenceSummary,
}

pub struct CapabilityRequirement {
    pub id: RequirementId,
    pub feature: FeatureId,
    pub parameters: CanonicalParameters,
    pub context: RequirementContext,
    pub depends_on: SmallVec<RequirementId>,
}

pub enum UnsupportedFeature {
    Filter { kind: FilterKind, location: ObjectLocation },
    Font { kind: FontKind, location: ObjectLocation },
    ColorSpace { kind: ColorSpaceKind, context: ColorContext },
    BlendMode { mode: BlendMode, group: Option<GroupId> },
    KnockoutGroup,
    Javascript,
    Xfa,
    RichMedia,
}
```

`FeatureSet` 仅用于搜索和统计；`CapabilityRequirement` 保留参数、依赖和上下文，`CapabilityDecision` 使用版本化 CapabilityProfile 对完整 requirement graph 求值。Capability 判定应在高分辨率 raster 前尽早完成。静态检测优先；运行时 unexpected-unsupported 是分类缺陷，必须进入回归和 profile 修正。不得先执行昂贵渲染再以未标记空白或残缺页面代替错误。

### 6.7 Canonical Scene

- 浮点值在 canonical 输出中使用规范化十进制或定点表示，统一 `-0`、NaN 和无穷值处理。
- 字典、资源和 feature 列表使用确定性排序；ID 按首次规范遍历顺序分配。
- 可忽略的诊断信息与语义 Scene 分离，避免环境路径或线程顺序污染 golden。
- Scene schema 有 major/minor 版本；major 不兼容，minor 必须允许跳过未知可选字段。

## 7. 字体、文本、图片与颜色

### 7.1 字体总体模型

PDF 字体系统必须把“画出字形”和“理解文字”分成两条链。PDF 内容流通常已经选择 character code/glyph 并给出 advance、text matrix 和 `TJ` 调整，因此读取/渲染链不得再做 shaping、bidi 重排或 Unicode 规范化来替换 glyph、advance 或位置。Shaping 只属于将新 Unicode 内容写入 PDF 的 authoring/editing/reflow 链，或未来单独注册的生成能力；它不属于现有 PDF 已定位 glyph 的重放链。

```text
绘制链：character code → CID/glyph id → outline/bitmap → PDF positioning
语义链：character code → ToUnicode/Encoding/CMap → Unicode → confidence
```

R0 对缺失或非嵌入字体继续 fail-closed，并返回结构化 `UnsupportedCapability`。未来若研究受控 fallback，必须使用独立 CapabilityProfile，固定字体集合、字体文件哈希、选择规则、metrics 兼容策略、平台矩阵和缓存 epoch；仅依赖当前机器的系统字体数据库不满足可复现性，也不得改变 R0 的字形或排版结果。

### 7.2 字体模块划分

```text
core/font/
├── cmap/
├── encoding/
├── unicode/
├── truetype/
├── opentype/
├── type1/
├── cff/
├── cid/
├── type3/
├── hint/
├── outline/
├── metrics/
└── fallback/
```

| 字体类型 | Native 策略 | 关键测试 |
| --- | --- | --- |
| Type 1 / MMType1 | 自主拥有 encoding/charstring/metrics 语义；可使用经批准 table/outline 叶子库 | subr、seac、异常 charstring、缺失 glyph |
| TrueType/OpenType | 自主拥有 PDF 映射和 metrics 语义；table/glyf/CFF 可经 trait 接入批准叶子库 | 复合 glyph、变体、坏 offset、hint 边界 |
| CFF/CFF2 | 自主拥有 PDF/CID 语义；INDEX/DICT/charstring VM 自研或使用经批准叶子库 | subroutine bias、stack、blend、异常递归 |
| CIDFont | CIDSystemInfo、CMap、CIDToGIDMap、vertical metrics | Identity-H/V、预定义 CMap、竖排 CJK |
| Type 3 | glyph procedure 解释为嵌套 Scene | 资源作用域、颜色化/非颜色化、递归和缓存 |
| 非嵌入字体 | R0 fail-closed；Post-R0 仅可通过独立 profile 研究固定字体包或受控 platform adapter | 跨平台替代差异、字体文件/规则哈希、宽度一致性、可诊断性、cache epoch |

### 7.3 TextAtom 与文本语义

**接口 7-1 文本原子**

```text
pub struct TextAtom {
    pub unicode: SmallUnicode,
    pub confidence: MappingConfidence,
    pub code: CharCode,
    pub cid: Option<u32>,
    pub glyph_id: Option<u32>,
    pub quad: Quad,
    pub baseline: Vector,
    pub writing_mode: WritingMode,
    pub font: FontId,
    pub mcid: Option<u32>,
    pub source_order: u32,
}
```

`MappingConfidence` 至少区分 `ExactToUnicode`、`DeclaredEncoding`、`EmbeddedCMap`、`GlyphNameInference`、`Heuristic` 和 `Unknown`。UI 在复制或搜索低置信度文本时应提示、降级或禁用不可靠操作，但不得调用外部 PDF 引擎补齐文本。

### 7.4 选择、搜索与阅读顺序

M6/R0 只验收水平 LTR：字符边界由 CMap/encoding 决定，Unicode 由 ToUnicode/声明 encoding 与显式置信度产生，几何由 PDF text state 和 glyph quad 产生。R0 可以保留原始 marked-content/MCID 信息，但不得把未实现的 ActualText、structure order、RTL logical order 或竖排语义伪装成可靠结果；这些能力由 FT1 单独晋级。

- 选择命中以 glyph quad 和 baseline 为基础，不能只使用 axis-aligned bounding box。
- 单词/行聚类结合 writing mode、baseline 角度、间距、font size 和 marked-content；Tagged PDF 结构优先于几何推断。
- 搜索索引保存规范化 Unicode 与 TextAtom 映射，支持大小写、组合字符、连字和可选断词规则。
- 复制结果必须区分视觉顺序、逻辑顺序和 ActualText；RTL 与竖排文档需要独立测试。
- 文本层、链接层和可访问性树由结构化数据生成，不从最终 bitmap 反推。

### 7.5 图片模型

图片资源保存原始 stream、decode 参数、颜色空间、mask、soft mask、interpolate 和尺寸。解码必须按 tile/缩放需求惰性执行；巨大扫描件不得默认解码为全分辨率整图。

```text
pub struct ImageResource {
    pub width: u32,
    pub height: u32,
    pub bits_per_component: u8,
    pub color_space: ColorSpaceId,
    pub decode: DecodeArray,
    pub image_mask: bool,
    pub mask: Option<ImageMask>,
    pub soft_mask: Option<ImageId>,
    pub interpolate: bool,
    pub stream: StreamRef,
}
```

### 7.6 颜色系统

| 层次 | 要求 |
| --- | --- |
| 基础空间 | DeviceGray、DeviceRGB、DeviceCMYK、CalGray、CalRGB、Lab |
| 索引与专色 | Indexed、Separation、DeviceN、NChannel 属性 |
| 配置文件 | ICCBased，缓存 profile 解析与 transform |
| 函数 | Type 0/2/3/4 函数，用于 tint、shading、transfer 等 |
| 合成 | blend color space、rendering intent、overprint、black point 等按阶段实现 |
| 输出 | 默认屏幕 sRGB；打印/专业 profile 作为独立模式，不污染快速阅读路径 |

Scene 不应把所有颜色过早转换为 RGBA。复杂透明组、DeviceN 和 overprint 需要保留颜色空间与通道语义。首版不支持的组合必须被 `FeatureReport` 捕获并返回页级 `UnsupportedCapability`。

## 8. 渲染器：Reference CPU、Fast CPU 与 GPU

### 8.1 为什么先做 Reference CPU

Reference renderer 是项目自有、确定性的实现基线。它应牺牲吞吐换取可解释性和跨平台一致性。Fast CPU 与 GPU 的测试目标首先是与已审核 Reference 的语义和覆盖结果一致，其次才是与 PDFium 等外部实现比较。Reference 自身不是自动成立的规范 oracle；它必须通过解析预期、几何不变量、人工可推导样例和多实现裁决验证。

> **核心原则：** 没有自主 reference renderer，就无法稳定判断 Fast/GPU 的差异来自优化错误、外部引擎差异还是规范歧义；但把 Reference 的首次输出直接固化为 golden，同样会把实现错误固化为项目真值。

### 8.2 Reference renderer 架构

```text
Scene
  ↓ canonical traversal
Geometry preparation
  ↓ fixed-point flattening
Scalar coverage raster
  ↓ deterministic clip masks
Spec-defined compositing
  ↓ deterministic color conversion
Canonical pixel surface
```

| 领域 | Reference 规则 |
| --- | --- |
| 坐标 | 使用有界定点表示；所有转换、裁剪和舍入规则固定并测试。 |
| 曲线 | 自适应 De Casteljau 展平，容差由设备空间和固定算法决定。 |
| 抗锯齿 | 采用固定采样或确定性面积覆盖；不调用平台字体/图形栈。 |
| 字体 | 测试字体必须嵌入；reference 默认不使用 OS hinting 和系统抗锯齿。 |
| 合成 | 内部使用预乘 alpha；blend、group、mask 按明确颜色空间处理。 |
| 并行 | 默认单线程或确定性分区；线程调度不能改变输出。 |
| 输出 | 固定像素格式、输出 profile 和 alpha 规则；输出经 oracle 审核后才可成为 regression golden。 |

PDF 语义符合性与项目 canonical raster 一致性必须分别报告。PDF 规范决定对象、图形状态、透明和颜色等语义；曲线展平、coverage 采样和抗锯齿的项目级确定性规则记录在 `reference-raster-vN` 中。Fast/GPU 偏离 canonical raster 是实现差异，但只有在违反规范语义、项目已发布视觉契约或容差门槛时才自动定性为规范缺陷。

### 8.3 Reference 实现分层

```text
core/raster/reference/
├── geometry/
├── flatten/
├── edge/
├── coverage/
├── clip/
├── blend/
├── group/
├── mask/
├── image/
├── glyph/
├── color/
└── surface/
```

### 8.4 Fast CPU renderer

Fast renderer 复用 Scene，但拥有独立实现。初始 tile 建议为 32×32 或 64×64 的内部 raster tile；产品输出 tile 可为 256×256 或 512×512。内部 tile 用于 binning、clip mask 和透明组局部表面。

```text
Scene commands
   ↓ bounds / spatial index
Command binning per internal tile
   ↓
Parallel tile jobs
   ├─ path coverage
   ├─ glyph/image sampling
   ├─ clip-mask cache
   └─ local transparency groups
   ↓
Product tile surface
```

### 8.5 Fast CPU 优化规则

- 先 profile，再优化；每项优化必须包含 microbenchmark、场景 benchmark 和输出差分。
- 禁止在 hot path 中使用文档级全局锁；字体、图片、clip 和 Scene cache 使用分片或单写多读结构。
- SIMD 后端通过统一 kernel 接口实现 scalar/SSE/AVX/NEON 等版本，scalar 作为语义基线。
- 透明组只分配其 tile 相交区域；超大 group 使用分块或降级策略，受中间表面预算约束。
- glyph coverage、path flattening、图片 mip/缩放结果分别缓存，键必须包含 scale、transform 分类和渲染质量。
- 所有长循环按 `FuelSchedule` 的固定最大间隔检查 cancellation；wall-clock watchdog 只由 runtime 外层执行。

### 8.6 产品 tile 与调度键

```text
pub struct TileKey {
    pub source: SourceIdentity,
    pub document_revision: u64,
    pub page: u32,
    pub scene_hash: SceneHash,
    pub zoom: ZoomBucket,
    pub device_scale: ScaleKey,
    pub rotation: Rotation,
    pub x: i32,
    pub y: i32,
    pub optional_content: OcgStateId,
    pub annotation_revision: u64,
    pub render_config: RenderConfigHash,
    pub renderer_epoch: u32,
}

RenderConfigHash includes:
- backend class and quality/AA policy
- output profile, alpha mode and pixel format
- image interpolation and font-fallback environment hash
- clip/tile halo policy and color-management version
```

所有缓存必须声明作用域：job、page、session、process 或 persistent。若 Tile cache 严格限制在单 session，可省略已由 session 隔离的字段，但持久化或跨 session cache 必须使用完整 `SourceIdentity + SceneHash + RenderConfigHash + RendererEpoch`。引擎升级、Reference raster 版本变化、系统字体数据库变化或颜色配置变化时，旧 epoch 不得命中。

### 8.7 GPU 后端

GPU 后端不依赖 Vello。首个目标是浏览器 WebGPU；Native GPU 可复用 shader 和中间数据格式，再分别接入平台接口或经审核的低层绑定。GPU 后端只在 reference/fast CPU 已覆盖的特性上开放，并固定实现所依据的 WebGPU 规范快照 [R7]。

```text
core/gpu/
├── encode/
├── binning/
├── path/
├── coverage/
├── clip/
├── compose/
├── shader/
├── resource/
└── backend/
    ├── webgpu/
    ├── metal/      # later
    ├── vulkan/     # later
    └── d3d12/      # later
```

### 8.8 GPU 退化与故障恢复

- 设备不支持、context lost、内存不足或某 feature 未实现时，按页面或 tile 切换 Fast CPU；不得显示半完成 surface。
- GPU shader 与 CPU reference 使用同一组生成式测试向量，特别覆盖边界、奇异矩阵、clip、blend 和 alpha。
- GPU 输出允许受控浮点差异，但不能只使用整页 SSIM；必须同时比较 coverage、结构区域和关键语义对象。
- GPU pipeline cache、纹理和 buffer 均受 session/全局预算控制；页面滚出视口后可快速回收。

## 9. Runtime、调度、缓存与平台集成

### 9.1 Document actor 与 worker pool

```text
Document Actor (single logical writer)
├── object/cache metadata
├── page/scene request coordination
├── change-set state
└── publishes immutable snapshots

Worker Pool
├── parse/decode jobs
├── scene build jobs
├── text indexing jobs
└── raster jobs consuming immutable Scene
```

不使用 `Arc<Mutex<EntireDocument>>`。Document actor 管理可变元数据和请求去重，worker 只处理明确输入并返回不可变结果。跨页字体/图片缓存采用分片，避免每个 glyph 触发文档级同步。

### 9.2 调度优先级

| 优先级 | 工作 |
| --- | --- |
| P0 | 当前视口中心 tile、用户正在交互的注释/表单 |
| P1 | 当前视口边缘 tile、当前页文本选择所需数据 |
| P2 | 滚动方向下一屏、相邻页 preview |
| P3 | 缩略图、目录目标页、搜索命中附近页 |
| P4 | 全文索引、后台预解码、低优先级 corpus/诊断任务 |

### 9.3 Viewport generation 与取消

每次缩放、旋转、跳页、文档修订或 optional-content 改变都会产生新的 generation。旧 generation 的任务应尽快取消；若无法中断，完成结果也不得进入 UI。

```text
pub struct ViewportRequest {
    pub generation: u64,
    pub visible_pages: Vec<PageViewport>,
    pub predicted_direction: ScrollDirection,
    pub quality: QualityPolicy,
}

if job.generation != session.current_generation() {
    return Err(RenderError::Cancelled);
}
```

### 9.4 多级缓存

| 缓存 | 键 | 淘汰与预算 |
| --- | --- | --- |
| 对象缓存 | SourceIdentity + object ref | 小对象优先保留；失败结果按 code/短 TTL 缓存 |
| 解码流缓存 | ResourceKey + decode params + codec epoch | 按输出字节计费；压缩炸弹和 ResourceLimit 不可缓存为成功结果 |
| 字体/字形 | font key + glyph + scale class + font environment | 分离程序、outline、coverage；分片 LRU |
| 图片 | image key + decode scale + region + color epoch | 优先可视 region；大图分块 |
| Scene | SourceIdentity + page + document revision + OCG + scene-builder epoch | 按 command/resource 内存计费 |
| 产品 tile | TileKey | 视口保护段 + 最近使用段 |
| GPU 资源 | backend-specific key | context 失效时整体清空 |

初始预算可按 tile 45%、decoded image 25%、Scene 15%、font/glyph 10%、object/other 5% 分配。该比例只是启动值，必须由真实 corpus 的峰值和命中率调整。

### 9.5 浏览器是否需要 TypeScript

> **明确结论：** TypeScript 不是 PDF 解析或 raster 的必要条件，但浏览器必须通过 JavaScript API 实例化 WebAssembly、驱动 Worker、DOM、Canvas 和 WebGPU。工程上应使用薄 TypeScript 宿主；TypeScript 编译后实际运行的是 JavaScript。

Rust/WASM 可以通过生成绑定调用 Web API，但仍会存在 JS glue。核心原则不是“零 TypeScript”，而是让 TS 只负责浏览器宿主和交互，不承载 PDF 语义、字体、内容解释或 raster 算法。WebAssembly 的标准接口本身通过 JavaScript API 与网页环境交互 [R6]。

### 9.6 浏览器线程模型

**图 9-1 浏览器部署**

```text
Main thread
├── viewer.ts          # viewport virtualization
├── interaction.ts     # mouse/touch/keyboard
├── text-layer.ts      # DOM text and selection
├── a11y.ts            # semantic DOM / focus order
└── worker-client.ts
          │ postMessage / transferable handles
          ▼
    Dedicated Engine Worker
    ├── native-worker.ts
    └── engine.wasm
        ├── parser / scene / text
        ├── scheduler / cache
        ├── CPU renderer
        └── WebGPU renderer
```

产品页面只创建 Native Engine Worker。主线程负责 UI 和消息路由，不执行 PDF 解析；Worker 返回 `CapabilityDecision`、稳定错误和 Native surface。PDFium baseline 仅由 CI/开发工具在浏览器产品进程之外运行，不出现在网页资源、Worker graph、CSP/network manifest 或 service-worker cache 中。

### 9.7 浏览器显示路径

| 路径 | 数据流 | 使用条件 |
| --- | --- | --- |
| Worker-private OffscreenCanvas staging | Wasm raster → Worker-private staging → Host-mediated ImageBitmap / ArrayBuffer / fenced SharedArrayBuffer | 可选优化；永不 transfer DOM-bound canvas，也不是 wire Surface [R5] |
| Worker CPU + ImageBitmap | Wasm raster → transferable ImageBitmap → Host canvas | 已协商 ImageBitmap 能力时的 Host-mediated Surface |
| Worker CPU + ArrayBuffer | Wasm buffer → transferable buffer → main canvas | 最广兼容，但复制和上传成本更高 |
| Worker WebGPU | Scene → GPU commands → WebGPU canvas | 能力检测、差分和稳定性门槛通过后 |

### 9.8 Canvas、文本层与可访问性

Canvas 只提供像素，不自然提供浏览器文本选择、链接焦点和屏幕阅读器语义。页面容器必须分层；HTML 标准也要求 Canvas 提供等价功能或目的的替代内容 [R5]。

```text
<div class="page">
  <canvas class="surface"></canvas>
  <div class="text-layer" aria-hidden="true"></div>
  <div class="link-layer"></div>
  <div class="annotation-layer"></div>
  <div class="accessibility-tree"></div>
</div>
```

- 视觉 text layer 可用透明 DOM span 提供选择；必须与 TextAtom quad 和 viewport transform 同步。
- R0 视觉 text layer 和可聚焦 link layer 不得声称完整可访问性；Post-R0 屏幕阅读器树不应简单复用视觉 span，应优先使用 structure tree、ActualText、Alt 和逻辑顺序。
- 链接与表单控件使用真实可聚焦 DOM 元素，并与页面坐标建立一一映射。
- 页面虚拟化销毁 DOM 前应保留焦点和选择状态，重新挂载时恢复。

### 9.9 浏览器共享内存与复制策略

`SharedArrayBuffer` 可用于 worker 与主线程共享控制环、surface 或队列，但只有在满足跨源隔离条件时才启用；标准在非隔离环境中禁止序列化共享缓冲区 [R8]。因此它必须是可选优化。

| 能力 | 首选 | 降级路径 |
| --- | --- | --- |
| 控制消息 | 结构化小消息 | 相同 |
| 大型像素 | Host-mediated ImageBitmap / Worker-private OffscreenCanvas staging | transferable ArrayBuffer |
| 共享环形队列 | SharedArrayBuffer + Atomics | 普通 postMessage |
| 远程字节 | Worker 侧 fetch 或宿主 RangeSource | 主线程下载后 transfer |
| GPU | WebGPU | Fast CPU |

### 9.10 浏览器协议示例

```text
// TypeScript host types; generated from the protocol schema.
export type Command =
  | { type: "open"; requestId: bigint; source: SourceDescriptor }
  | { type: "viewport"; session: bigint; generation: bigint; pages: PageViewport[] }
  | { type: "search"; session: bigint; query: string; options: SearchOptions }
  | { type: "supplyData"; session: bigint; ticket: bigint;
      snapshot: SourceIdentity; segments: TransferSlot[] }
  | { type: "failData"; session: bigint; ticket: bigint; error: SourceError }
  | { type: "close"; session: bigint };

export type Event =
  | { type: "opened"; requestId: bigint; session: bigint; info: DocumentInfo }
  | { type: "tileReady"; session: bigint; generation: bigint; tile: TileDescriptor }
  | { type: "needData"; session: bigint; ticket: bigint;
      snapshot: SourceIdentity; ranges: ByteRange[] }
  | { type: "unsupported"; session: bigint; generation: bigint; page?: number;
      decision: CapabilityDecision }
  | { type: "sourceChanged"; session: bigint; expected: SourceIdentity;
      observed: SourceIdentity }
  | { type: "error"; requestId?: bigint; error: EngineError };
```

`supplyData` 的每个 transfer slot 对应同一消息 transfer list 中的不可变 ArrayBuffer segment，并带有 range metadata；接收端验证 ticket、snapshot、范围、长度和重叠规则。若 Worker 自行 fetch，同一状态机仍在 Worker 内执行，只是不经过主线程消息。

### 9.11 浏览器包体与懒加载

- `engine.wasm` 只包含 P0 常用解析、文本和 CPU 路径；JPX、JBIG2、高级颜色等可按独立模块或延迟段加载。
- 字体/图片 codec 的拆分必须避免破坏单次 session 缓存和取消；模块版本必须与 Scene/protocol 兼容。
- 每个 release 记录压缩前、传输后、编译时间、实例化时间和首个调用成本。
- Release CI 必须检查资源清单、source map、Wasm imports、service-worker precache 和网络请求，证明产品不包含或下载外部 PDF 引擎。

### 9.12 桌面部署

```text
UI process
   │ versioned IPC
   ▼
sandboxed engine worker
   ├── native core
   ├── CPU/GPU renderer
   └── shared-memory surfaces
```

- 若桌面 UI 使用 WebView/Electron 风格技术，可复用浏览器 TypeScript viewer，但引擎通过 native IPC 而不是在 UI 中跑完整 Wasm。
- 若 UI 为原生框架，TypeScript 完全不是必需；协议和引擎保持相同。
- 像素首阶段通过共享内存 BGRA surface 返回；成熟后可增加 DXGI/IOSurface/dma-buf 等共享纹理适配。
- 发布包的动态库、进程启动清单和安装 manifest 不得包含 PDFium 或其他外部 PDF 引擎。

## 10. 注释、表单与增量写入

### 10.1 分层显示模型

```text
Final page composition
├── Original Scene
├── Annotation Scene
├── Form Widget Scene
├── Search/Selection Overlay
└── UI Handles / Caret
```

注释和表单不直接修改原页面 Scene。原文档、变更集和 UI 临时状态分离，可降低重渲染范围并支持 undo/redo。

### 10.2 ChangeSet

```text
pub enum Change {
    CreateAnnotation(Annotation),
    UpdateAnnotation { id: AnnotationId, patch: AnnotationPatch },
    DeleteAnnotation(AnnotationId),
    SetFormValue { field: FieldId, value: FormValue },
    SetOptionalContent { group: OcgId, visible: bool },
}

pub struct ChangeSet {
    pub base_revision: RevisionId,
    pub revision: u64,
    pub operations: Vec<Change>,
}
```

### 10.3 增量 writer

- 默认保留原始 bytes，在文件尾追加修改对象、xref 和 trailer；未修改对象不重写。
- writer 使用对象图和变更集，不直接依赖 UI 类型；输出前执行结构验证并重新打开自检。
- xref table/stream 选择遵循原文档兼容性策略；新对象号分配和 generation 规则固定。
- 保存后对未修改页面执行 Scene/text hash 回归，确保增量写入未改变原内容。
- 存在数字签名时必须展示“新增修订可能影响签名状态”；签名验证和权限判断由独立模块处理。

### 10.4 表单范围

| 阶段 | 能力 |
| --- | --- |
| P0 | 读取 widget 几何、名称、值和基本 appearance；只显示，不执行 JavaScript |
| P1 | 文本、复选、单选、下拉等常见 AcroForm 交互；保存值和 appearance |
| P2 | 计算、格式、验证动作的受控子集；签名字段；复杂 appearance |
| 明确延后 | XFA、任意 PDF JavaScript、外部提交和不受控动作 |

## 11. 外部基线与 Native 能力成熟度

### 11.1 稳定态边界

PDFium、PDF.js、MuPDF 或其他完整处理器只允许作为开发/CI 黑盒 baseline。它们不实现产品 trait，不接收用户请求，不参与 runtime policy，也不生成用户可见像素、文本或保存结果。产品对不支持能力返回 `CapabilityDecision::Unsupported`；对损坏、预算、完整性和内部错误返回各自稳定错误。

这一边界同时适用于桌面、浏览器、移动端和服务端构建。即使外部引擎通过独立进程、动态库、Wasm、远程服务或用户首次使用后下载，也仍属于产品路径，违反本规范。

### 11.2 Baseline runner 协议

```text
pub trait BaselineRunner {
    fn describe(&self) -> BaselineDescriptor;
    fn inspect(&mut self, input: CorpusObject, request: InspectRequest)
        -> Result<BaselineObservation, BaselineToolError>;
}

pub struct BaselineDescriptor {
    pub engine: String,
    pub upstream_revision: String,
    pub build_hash: [u8; 32],
    pub build_flags: Vec<String>,
    pub environment_hash: [u8; 32],
    pub license_manifest_hash: [u8; 32],
}

pub enum BaselineObservation {
    DocumentMetadata(CanonicalMetadata),
    PagePixels(CanonicalSurface),
    TextPage(CanonicalTextPage),
    Failure(BaselineFailure),
    Timing(BaselineTiming),
}
```

该协议位于 `tools/baseline/protocol`，不被任何 product crate 依赖。Runner 通过子进程/容器处理固定 corpus object，并把输出标准化为测试 artifact。它不得暴露外部引擎指针、对象生命周期或 API 类型，也不得被编译进 `engine.wasm`、桌面 worker 或 SDK。

### 11.3 CapabilityDecision 与产品行为

```text
pub enum SupportStatus {
    Supported,
    Unsupported,
    Rejected,
}

pub struct CapabilityDecision {
    pub status: SupportStatus,
    pub profile: CapabilityProfileId,
    pub missing: Vec<RequirementId>,
    pub contributors: Vec<RequirementId>,
    pub scope: DecisionScope,
    pub location: Option<ErrorLocation>,
    pub policy: PolicyVersion,
}
```

禁止使用单一 `render failed`。Decision 必须回答缺少哪项能力、受影响范围、对象/内容流位置、所用 profile/policy 版本，以及 UI 可否继续显示不受影响的元数据或页面。`Unsupported` 不是错误恢复入口；它不会调用 baseline。`Rejected` 用于损坏、禁止动作或策略明确拒绝的输入。任何已发布 surface 都必须来自同一 Native Scene/RenderConfig；不允许用外部输出拼接空缺区域。

### 11.4 Baseline 隔离、许可与隐私

- Baseline runner 默认只处理自建、公开可用或经授权的 corpus；不得把用户文档自动上传给外部服务。
- Runner binary/container、上游 revision、flags、字体、颜色配置和许可证 manifest 必须固定；版本变化产生新的 baseline id。
- PDFium/其他引擎输出属于 O4 observation，不得自动成为 golden，也不得决定 Native 的 Range、修复或错误策略。
- CI 可以在独立 job 下载或构建 baseline，但 release job 不继承该 filesystem layer；发布物扫描必须证明没有外部引擎残留。
- 若团队分发 baseline 工具或 CI image，开发工具许可证清单随该分发物提供；它与产品自身许可证和产品 SBOM 分开。

### 11.5 Native 能力成熟状态

| 状态 | 产品行为 | 证据要求 |
| --- | --- | --- |
| PLANNED | 不在 ReleaseProfile；结构化 Unsupported | 规范研究、feature taxonomy、owner |
| REFERENCE | 仍不发布；Reference 路径仅测试可用 | O0/O1 样例、边界、fuel、provenance |
| DIFFERENTIAL | 仍不默认发布；Fast/GPU 与 Reference/外部 baseline 离线差分 | O2 裁决、holdout、fuzz、性能分布 |
| CANARY | Native 实现对受控 cohort 开放；失败仍返回 Native 错误/Unsupported | 可回滚 capability flag、用户指标、无外部引擎 |
| DEFAULT | Native 默认启用 | 正确性、稳定性、性能和支持覆盖达到 profile 门槛 |
| STABLE | 稳定公共能力；变更遵守兼容性与 renderer epoch | 连续发布无严重回归、文档/API/回滚演练完整 |

状态描述 Native feature 的证据成熟度，不描述 PDFium 生命周期。外部 baseline 可以作为测试工具长期存在，不需要“退场”；只需保证它始终位于产品边界之外。

### 11.6 建议能力成熟顺序

| 组件 | 建议顺序 | 进入 DEFAULT 的关键门槛 |
| --- | --- | --- |
| 页数、目录、页标签、元数据 | 最早 | 对象解析稳定；真实 corpus 一致率达到绑定 ReleaseProfile 门槛 |
| 文本提取与搜索 | 较早 | Unicode、quad、顺序和低置信度策略完成 |
| 基础路径/图片/嵌入字体渲染 | 中期 | Reference/Fast 双路径与性能闭环 |
| 透明、Pattern、Shading、DeviceN | 较晚 | 复杂 corpus、颜色和 group 组合测试完成 |
| 注释显示与保存 | 较晚 | 增量 writer 自检与跨处理器离线打开验证 |
| AcroForm | 最后阶段 | 事件、appearance、保存和浏览器 DOM 一致 |
| XFA/JavaScript/3D | 产品决策 | 可以长期结构化 Unsupported，不得以外部引擎隐藏产品边界 |

### 11.7 晋级门槛

| 迁移 | 必须满足 |
| --- | --- |
| PLANNED → REFERENCE | 有结构化 CapabilityProfile；合法、非法、边界和组合样例；规范/provenance/预算字段完整 |
| REFERENCE → DIFFERENTIAL | O0/O1 全通过；Reference 自身已审查；Fast 或目标实现可运行；差分字段和 baseline 指纹完整 |
| DIFFERENTIAL → CANARY | 原子规范测试 100% 通过；critical/major 已知差异为 0；至少 1,000 个 eligible holdout 页面无 panic/hang；p95/PDFium baseline ≤ 1.10 |
| CANARY → DEFAULT | 至少 14 天且 100,000 个 eligible 页面访问；critical 为 0、major 页面率 ≤ 0.10%、unexpected-unsupported ≤ 0.50%；p95/baseline ≤ 1.05；回滚演练通过 |
| DEFAULT → STABLE | 连续两个稳定发布无 critical/major 回归；in-profile supported rate ≥ 99.90%；主要路径 p95/baseline ≤ 1.00；API/格式兼容性和回滚演练通过 |

页面访问数不足的内部/离线产品可在 `ReleaseProfile` 中用等价的固定 holdout 样本量替代，但不得降低缺陷率、稳定性和回滚要求。所有分母、采样窗口、去重方法和 cohort 必须随晋级记录归档。晋级报告可以引用 PDFium baseline 数据，但产品 release artifact 的生成与运行不得依赖 baseline 可用性。

## 12. 测试与质量工程体系

### 12.1 测试是第二套核心系统

测试系统不是 `tests/` 目录中的附属代码，而是与 parser、Scene 和 renderer 等价的产品资产。实现每个 feature 前，团队必须先能回答：规范依据是什么、最小合法/非法文件如何生成、语义预期是什么、像素如何判定、外部实现不同怎么办、性能如何测量、失败如何最小化、何时允许 Native 能力晋级。

> **最高优先级：** M0 阶段先完成 case manifest、生成器、runner 协议、PDFium baseline runner、Scene/Text/Pixel diff 骨架、benchmark harness 和 corpus manager。M0 使用 synthetic outputs 验证工具链；真实 Reference renderer 在 M3 交付。没有这些设施，不进入大规模图形特性开发。

### 12.2 测试分层

| 层 | 目的 | 典型输出 |
| --- | --- | --- |
| 单元测试 | 纯函数、边界和数据结构 | 值、错误、分配预算 |
| 语法/对象测试 | token、xref、stream、revision、repair | 对象树、诊断、NeedData 序列 |
| 原子规范测试 | 一个条款或一个差异点 | Scene/Text/Pixel golden |
| 生成式/属性测试 | 覆盖组合空间和不变量 | 模型一致性、无 panic |
| Metamorphic | 验证等价变换 | 变换前后语义相同 |
| 差分测试 | 发现实现分歧 | Native/PDFium/其他黑盒差异包 |
| 浏览器 E2E | 真实 Worker/Canvas/DOM/交互 | 截图、事件、a11y、性能 trace |
| Fuzz | 安全、恢复和状态空间 | 最小 crash/hang/预算样例 |
| 性能测试 | 组件和用户路径回归 | p50/p95/p99、内存、复制、supported/unsupported |
| Corpus 回归 | 真实世界兼容性 | 支持覆盖率、严重缺陷率、能力成熟指标 |

### 12.3 Case 目录与 Manifest

```text
tests/cases/transparency/soft-mask-luminosity-004/
├── input.pdf
├── case.toml
├── source.dsl            # when generated
└── expected/
    ├── parse.json
    ├── scene.json.zst
    ├── text.json
    ├── page-1.rgba.zst
    ├── diagnostics.json
    └── metrics.json
```

**接口 12-1 case.toml 示例**

```text
id = "transparency/soft-mask-luminosity-004"
title = "Luminosity soft mask with isolated group"
status = "active"
validity = "valid"
provenance = "self-authored-from-spec"
license = "project-test-data"
features = ["soft-mask", "luminosity", "transparency-group", "isolated"]
clauses = ["ISO-32000-2:2020/11.6.5"]

[input]
kind = "generated"
generator = "scene-dsl"
seed = 381092

[expected]
parse = true
scene = true
text = false
pixels = ["page-1"]
diagnostics = []

[oracle]
authority = "O1-analytic"
derivation = "expected/oracle.md"
reviewers_required = 2
reference_may_generate = false

[render]
width = 512
height = 512
profile = "srgb-reference-v1"
antialias = "reference-v1"

[tolerance]
mode = "exact"

[budget]
max_input_bytes = 65536
max_objects = 64
max_stream_output_bytes = 1048576
max_path_segments = 4096
max_group_depth = 8
max_operator_fuel = 20000
max_decode_fuel = 1048576
fuel_schedule = "fuel-v1"
watchdog_ms = 500       # outer safety only; not a semantic oracle
```

### 12.4 Manifest 必填字段

| 字段组 | 内容 |
| --- | --- |
| Identity | 唯一 id、标题、owner、状态、首次引入版本 |
| Specification | 规范版本、条款、勘误编号、解释说明 |
| Provenance | 自建/外部、来源、许可证、哈希、可再分发性 |
| Features | 语法、字体、颜色、透明、文本等结构化标签 |
| Validity | valid、invalid、ambiguous、real-world-tolerated |
| Expected | parse、diagnostic、Scene、text、pixel、CapabilityDecision/错误 |
| Budget | 输入、对象、递归、解压、像素、时间、内存上限 |
| Tolerance | exact、coverage-aware、color-aware、manual-review |
| Oracle | authority level、derivation、reviewers、是否允许 Reference 生成、最后审核版本 |
| Runners | Reference/Fast/GPU 是否必须运行；哪些外部 baseline 仅作 O4 observation |
| History | 已知差异、关联 issue、golden 变更记录 |

### 12.5 规范覆盖矩阵

建立机器可读的 `spec-map.toml`，从规范条款映射到测试、实现模块和 feature flag。CI 必须能够回答“某条款是否有测试”和“某段实现由哪些条款/回归覆盖”。

```text
["ISO-32000-2:2020/8.5.3"]
status = "implemented"
modules = ["core/content", "core/graphics", "core/raster/reference"]
tests = [
  "graphics/path/nonzero-fill-001",
  "graphics/path/evenodd-fill-002",
  "graphics/path/self-intersection-004",
]
owner = "graphics"
```

### 12.6 最小 PDF 生成器

项目必须拥有自己的 PDF 测试生成 DSL。它生成结构简单、可读、可重复的文件，并能系统切换 xref、object stream、filter、incremental revision、加密和损坏模式。手写二进制 fixture 只用于极小语法边界。

**接口 12-2 测试生成 DSL 概念**

```text
document(version: "2.0") {
  object(1) = catalog(pages: ref(2));
  object(2) = pages(kids: [ref(3)], count: 1);
  object(3) = page(
    media_box: [0, 0, 200, 200],
    resources: { "ExtGState": { "GS1": ref(5) } },
    contents: ref(4)
  );
  stream(4, filter: flate) {
    "q /GS1 gs 0 0 100 100 re f Q"
  }
  object(5) = dict { "Type": "/ExtGState", "ca": 0.5 };
  xref(kind: stream);
}
```

### 12.7 生成变体维度

| 维度 | 变体 |
| --- | --- |
| xref | table、stream、hybrid、错误 offset、增量链 |
| 对象 | 直接/间接、对象顺序、object stream、重复定义 |
| stream | 直接/间接 Length、拆分/合并、filter chain、截断 |
| 数字/字符串 | 空白、注释、指数、转义、编码和边界值 |
| 页面 | 资源继承层级、旋转、box 组合、错误 Count |
| 图形 | 矩阵分解、q/Q 冗余、路径等价表达、clip 顺序 |
| 修订 | 新增对象、替换对象、删除引用、签名后更新 |
| 安全 | 密码组合、权限、对象级加密、错误参数 |

### 12.8 Metamorphic testing

Metamorphic testing 验证“变换后应保持同一语义”的关系，可在没有绝对 golden 的复杂页面上发现架构级错误。每个等价变换都必须声明适用前提。

| 变换 | 预期不变量 |
| --- | --- |
| 重排无依赖间接对象 | 对象模型、Scene、文本和像素不变 |
| xref table ↔ xref stream | 文档语义不变 |
| 内容流拆分/合并 | 操作符序列和渲染不变 |
| 插入无副作用 `q Q` | Scene 与像素不变 |
| 矩阵分解为等价乘积 | 设备空间几何不变 |
| 资源名称重命名并同步引用 | 语义不变 |
| 直接对象改为间接对象 | 语义不变 |
| 新增无关增量修订 | 既有页面不变 |
| 整页渲染 ↔ tile 合成 | 拼接结果与整页输出一致 |
| Fast/GPU 分块顺序变化 | 最终输出与缓存键不变 |

### 12.9 模型与属性测试

- 对象 parser 与测试 AST 模型往返：`encode(parse(bytes))` 在 canonical 模式满足等价。
- 矩阵、path bounds、clip 交集和颜色函数使用随机输入验证代数不变量与范围。
- 对象 resolver 随机图必须满足循环检测、最新修订覆盖和预算终止。
- writer 生成文档重新打开后，ChangeSet 语义一致；未修改对象 hash 保持。
- 搜索索引的标准化文本必须能映射回原 TextAtom 范围，不产生越界或重叠错误。

### 12.10 三层渲染差分

```text
Layer 1: Scene semantic diff
  commands, order, transforms, clips, groups, glyphs, images, colors

Layer 2: Geometry / coverage diff
  path coverage, glyph coverage, masks, group boundaries

Layer 3: Final pixel diff
  alpha, color, missing regions, edge differences
```

先做 Scene diff，再做 coverage，最后做像素。否则整页出现数十万差异像素时，无法判断根因是内容解释、字体 outline、抗锯齿、颜色转换还是透明合成。

### 12.11 Scene diff

| 比较项 | 策略 |
| --- | --- |
| 命令序列 | 按 canonical command type 和来源比较，允许声明的等价重排 |
| 变换 | Reference 定点精确；外部引擎比较使用明示容差 |
| 路径 | 先比较 segment，再比较 bounds/拓扑摘要 |
| 文字 | font identity、glyph id、Unicode、quad、render mode |
| 组与 mask | 边界、isolated、knockout、blend、soft mask |
| 资源 | 语义 hash，不依赖运行时指针或分配顺序 |
| 诊断 | 错误代码和对象位置；环境相关字符串被剥离 |

### 12.12 Pixel diff

| 模式 | 用途 | 判定 |
| --- | --- | --- |
| Exact | 自建 reference fixture | 逐像素、逐通道完全一致 |
| Edge-aware | Fast/GPU 对 Reference | 边缘覆盖容差 + 内部区域严格 |
| Color-aware | ICC/DeviceN/外部基线 | 颜色差、alpha、区域面积联合 |
| Semantic alerts | 真实 corpus | 文字/图片/高对比组件缺失直接判严重 |
| Manual review | 规范歧义或多引擎分歧 | 保存差异包，人工裁决后固化测试 |

禁止只依赖 SSIM 或整页平均误差。小段文字完全消失可能对整页相似度影响很小，但属于严重缺陷。diff 工具必须检测连通差异区域、文字区域、图片区域、透明边界和高对比变化。

### 12.13 Text diff

| 维度 | 检查内容 |
| --- | --- |
| 字符 | Unicode sequence、原始 code、CID、glyph id、置信度 |
| 几何 | quad、baseline、writing mode、font size |
| 顺序 | source、visual、logical、structure order |
| 选择 | 点命中、拖选、跨行、跨方向、旋转页面 |
| 搜索 | 大小写、组合字符、ligature、断词、RTL、竖排 |
| 可访问性 | role、Alt、ActualText、结构父子、焦点顺序 |

### 12.14 差分测试与 Oracle 治理

Golden 必须声明 authority，禁止把首次实现输出自动视为正确答案。

| 等级 | 权威来源 | 允许用途 |
| --- | --- | --- |
| O0 Normative | 规范文本可直接推导的对象、状态、错误或 Scene 结果 | 语法/语义精确断言；最高优先级 |
| O1 Analytic | 人工可计算的几何、coverage、颜色或合成小样例，以及代数/属性不变量 | 验证 Reference 自身和边界算法 |
| O2 Adjudicated | 多个独立实现分歧后，由规范 reviewer 基于条款、勘误和最小样例裁决 | 复杂语义与真实文件 golden |
| O3 Reference Regression | 已通过 O0/O1/O2 审核的 Reference 输出 | Fast/GPU/跨平台回归；不能反向证明 Reference 正确 |
| O4 Observational | PDFium/其他黑盒共识、视觉检查或启发式预期 | 发现问题和 manual review；不得单独阻断规范符合性 |

Reference 代码变更不得由同一输出自动更新 O3 golden。Golden PR 必须包含 oracle 等级、推导说明、旧/新差异、受影响 feature/corpus、两名 reviewer，其中至少一名承担规范/测试视角。Pixel exact 表示符合 `reference-raster-vN` 的项目契约，不应写成“ISO 要求逐像素相同”。

PDFium 是高价值 baseline，但不是规范 oracle。差分运行可同时包含 Native Reference、Native Fast、Native GPU、PDFium 和其他经批准的黑盒处理器。结果分为以下类别：

| 分类 | 处理 |
| --- | --- |
| Native 明确错误 | 修复并加入最小回归 |
| PDFium 明确错误 | 保留规范判定；不得把 PDFium 输出设为 golden |
| 规范允许多种输出 | 定义本项目 canonical 输出和外部容差 |
| 规范/勘误歧义 | 记录 research issue，必要时提交标准问题 |
| 文件本身损坏 | 定义严格错误与宽容恢复的预期差异 |
| 无法归因 | 进入 manual review 队列，禁止自动更新 golden |

### 12.15 浏览器 E2E 测试

| 类别 | 场景 |
| --- | --- |
| 启动 | Wasm 下载/编译/实例化、Worker 建立、产品资源清单无外部 PDF 引擎 |
| 网络 | Range、无 Range、慢速、断线、ETag 改变、跨域错误 |
| 显示 | DPR、浏览器缩放、PDF 缩放、旋转、resize、连续滚动 |
| 交互 | 选择、复制、搜索、链接、注释、键盘导航、触摸 |
| 线程 | OffscreenCanvas 有/无、Worker 重启、消息乱序、取消 |
| GPU | 能力缺失、context lost、资源不足、CPU fallback |
| 内存 | 大量页、巨幅扫描件、低内存预算、页面虚拟化 |
| 可访问性 | DOM role、焦点顺序、结构树、屏幕阅读器快照 |

浏览器 runner 应使用符合团队许可证 allowlist 的 W3C WebDriver 兼容方式或自有轻量 harness。测试必须在至少三类浏览器引擎和多个 DPR/设备档运行；具体产品支持矩阵由 release policy 维护。

### 12.16 Fuzz 体系

| Fuzz 类型 | 目标 |
| --- | --- |
| 字节破坏型 | token、xref、stream、字体、图片、CMap 的截断、翻转和边界 |
| 结构生成型 | 合法对象图、修订链、页面树、资源和操作符组合 |
| 状态机型 | 打开/关闭、取消、密码重试、Range 到达顺序、worker 消息 |
| 差分型 | 同输入驱动 Reference/Fast/PDFium，检测崩溃与语义分歧 |
| Writer 型 | 随机 ChangeSet、增量保存、重开与语义不变量 |
| GPU 型 | shader 输入边界、资源生命周期、context lost 和 command 校验 |

### 12.17 Fuzz 目标清单

- lexer、number、string、name、array/dictionary parser。
- xref table/stream、object stream、incremental chain、repair scanner。
- ASCII85、LZW、predictor、JPEG/JPX/JBIG2/CCITT 等解码器。
- CMap、ToUnicode、Type1/CFF/TrueType charstring/table parser。
- content operand stack、graphics-state stack、inline image、Form XObject。
- path flatten、bounds、clip、blend、soft mask 和颜色函数。
- page tree、name tree、outline、structure tree、annotation/form。
- incremental writer、xref emission、重新打开和签名边界。

### 12.18 失败最小化

任何 fuzz、corpus 或差分失败都应自动生成 failure bundle，并进入结构感知 minimizer。普通字节级 delta debugging 对 PDF 效果有限，因此需要对象图和内容流级最小化。

```text
Failure bundle
├── input hash / source metadata
├── minimized.pdf
├── feature-report.json
├── diagnostics.json
├── scene-native.json.zst
├── scene-reference.json.zst
├── native.png / baseline.png / diff.png
├── text-native.json / text-baseline.json
├── trace.json.zst
└── environment.json
```

- 对象图最小化：删除不可达对象、页面、资源和修订。
- 内容流最小化：按操作符组删除，自动维护必要 `q/Q`、`BT/ET` 和资源。
- 字体/图片最小化：裁剪 table、glyph、segment 和颜色分量，同时保持触发条件。
- 语法最小化：把 stream/filter/xref 变为更简单形式，确认问题是否仍存在。
- 最小文件自动加入 regression，关联 issue 和首次修复版本。

### 12.19 Corpus 分层与治理

| Tier | 运行频率 | 内容 |
| --- | --- | --- |
| T0 | 每次提交 | 自建原子规范用例、关键回归、快速生成变体 |
| T1 | 每个 PR / 合并队列 | 数千高价值页面、模块定向 corpus |
| T2 | 每日 | 数万到数十万真实文件抽样、差分与性能 |
| T3 | 周期离线 | 大型公开/私有 corpus、长时 fuzz 和全面兼容统计 |

PDF Association 提供 PDF 2.0 示例、PDF-centric corpus 索引和多种测试套件，可作为外部语料发现入口 [R9][R10][R11]。每个外部文件仍需独立记录许可证、哈希和再分发条件，不能因为仓库公开就默认可复制。

### 12.20 Corpus 采样与指标防偏差

- 同时维护按文件、按页面、按用户页面访问和按 feature 加权的指标，防止大量简单 PDF 稀释复杂失败。
- 按来源、生成器、PDF 版本、页数、字体、颜色、透明、扫描件、语言和损坏程度分层。
- 训练/调试 corpus 与发布验收 corpus 分开，避免针对固定文件过拟合。
- 私有用户文件只保存不可逆 hash、feature 摘要和经授权最小样例；默认不上传原文档。

### 12.21 性能测试方法

性能测试必须区分组件吞吐与用户路径。所有结果包含硬件、OS、浏览器、编译器、提交、feature flags、cache 状态和 corpus hash。

| 层级 | 指标 |
| --- | --- |
| Byte/Range | 请求数、合并率、下载字节、到首个对象时间 |
| Syntax/Object | MB/s、objects/s、分配、cache hit、repair 成本 |
| Filters/Codecs | 输入/输出吞吐、峰值内存、取消延迟 |
| Content/Scene | operators/s、commands/s、Scene bytes、bounds/index 成本 |
| Font/Text | fonts/s、glyphs/s、chars/s、选择与索引延迟 |
| Raster | MPix/s、segments/s、blend pixels/s、tile latency |
| Runtime | 队列等待、stale work、cache hit、锁竞争、复制字节 |
| 产品路径 | 首个 preview、全质量视口、滚动 frame、缩放、跳页、搜索 |

### 12.22 Benchmark 场景

| 场景 | 必须区分 |
| --- | --- |
| Cold open | 进程冷、Wasm 冷、文件冷、无缓存 |
| Warm reopen | 代码/字体/对象或浏览器缓存存在 |
| First visible preview | 用户首次看到可识别页面内容 |
| First full-quality viewport | 当前视口所有目标 tile 完成 |
| Continuous scroll | 稳定帧时间、预取命中、任务取消 |
| Fast wheel/pinch | stale work、低质量占位和回填 |
| Random jump | 非相邻 Range、page tree 索引、cache miss |
| Large scan | 图片解码、缩放、内存带宽 |
| Vector/CAD | 路径数量、clip、透明和 GPU/CPU 差异 |
| CJK/text heavy | 字体、CMap、glyph cache、选择搜索 |
| Malformed | 修复时间、预算终止和结构化拒绝/unsupported |

### 12.23 统计与回归门槛

- 同一场景先 warm-up，再运行足够重复次数；报告 median、p95、p99 和置信区间，不只报最快值。
- CI 快速门槛可使用历史分布和噪声预算；重大发布在受控硬件池运行。
- 初始建议：组件 microbenchmark median 回归 >3% 或产品 p95 回归 >5% 自动阻断；团队可按噪声调整。
- 任何性能优化都必须同时运行正确性 diff 和峰值内存检查；不得用降低质量、跳过已声明 feature 或扩大 unsupported 范围来“提速”。
- 对 PDFium 的比较必须使用相同输入、输出尺寸、颜色目标、缓存状态和可见区域；整页对 tile 的比较需明确用户价值。

### 12.24 CI 流水线

| Lane | 内容 | 失败处理 |
| --- | --- | --- |
| Pre-submit local | format、lint、模块单测、受影响 T0 | 开发者本地修复 |
| PR fast | 全单测、T0、定向 T1、reference diff、快速 perf | 阻断合并 |
| Merge queue | 跨模块 T1、浏览器 smoke、外部 baseline differential | 阻断主干 |
| Nightly | T2、完整 browser、差分、长 fuzz、性能池 | 自动 issue + owner |
| Weekly | T3 抽样、GPU 设备矩阵、全量 provenance/license | 质量评审 |
| Release | 固定 corpus、签名构建、能力门槛、产品依赖纯度、回滚验证 | 阻断发布 |

### 12.25 Flaky 测试政策

- 不允许无限自动重试后标绿。第一次失败和重试结果都必须保留。
- 临时 quarantine 必须有 owner、issue、到期日期和影响范围；默认最多一个短发布周期。
- Golden 不得由 CI 自动覆盖；更新需附差异图、规范依据和两名 reviewer。
- 浏览器或 GPU 环境噪声应通过更好同步和环境固定解决，不得扩大容差掩盖真实回归。

### 12.26 覆盖率模型

| 覆盖率 | 含义 |
| --- | --- |
| 代码覆盖 | 行、分支、错误分支和 unsafe 边界 |
| 规范覆盖 | ISO 条款/勘误映射到测试 |
| Feature 覆盖 | 过滤器、字体、颜色、透明、操作符组合 |
| Corpus 覆盖 | 真实页面分布和高风险类别 |
| 状态覆盖 | PLANNED/REFERENCE/DIFFERENTIAL/CANARY/DEFAULT/STABLE 路径和回滚 |
| 平台覆盖 | 浏览器引擎、DPR、设备、GPU、桌面 OS |

### 12.27 每个 Feature 的 Definition of Done

- 有 research note、规范条款和来源记录。
- 有最小合法、非法、边界、参数组合和变体测试；有版本化 fuel 与 runtime budget。
- 测试声明 O0-O4 oracle authority；Reference 实现和 canonical Scene/Pixel/Text 预期由独立 reviewer 审查。
- Fast 路径与 Reference 差分通过；性能 benchmark 有结果。
- 加入 fuzz seed/target，失败可最小化。
- FeatureReport 包含 requirement graph/CapabilityProfile；CapabilityDecision/错误完整；ResourceLimit/Internal 不转交外部引擎；unsupported 路径可观测。
- 新增叶子依赖完成 ownership、license、budget/cancellation、Wasm/Native 与替换计划审查。
- 浏览器/桌面适用路径通过集成测试。
- 满足对应迁移门槛，状态由评审按 `PLANNED → REFERENCE → DIFFERENTIAL → CANARY → DEFAULT → STABLE` 晋级。

## 13. 安全、资源预算与可观测性

### 13.1 威胁模型

| 威胁 | 控制 |
| --- | --- |
| 内存破坏 | Rust 安全代码、最小 unsafe、FFI 隔离、sanitizer/fuzz |
| CPU DoS | 操作符、递归、路径、字体、图片、函数和时间预算 |
| 内存 DoS | 解压、像素、中间 group、cache 和 GPU 预算 |
| 解析循环 | 对象/资源递归检测、状态机和 deadline |
| 恶意 JavaScript/动作 | 默认不执行；明确 unsupported |
| 外部资源访问 | core 无网络；宿主按策略获取，禁止 PDF 任意发起 |
| 密码泄漏 | 短生命周期 Secret、禁止日志、崩溃包剥离 |
| 供应链污染 | lockfile、SBOM、许可证、来源和 vendoring 审查 |
| 隐私泄漏 | 遥测不含文档内容；corpus 使用授权和最小化 |

### 13.2 Budget 模型

```text
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

pub struct Budget {
    pub fuel: FuelBudget,
    pub runtime: RuntimeLimits,
}
```

预算必须可分层：全局、session、page、job 和 codec。子任务通过 `BudgetScope` 领取配额，结束后归还未使用部分。预算终止返回稳定错误，不允许 panic 或部分未标记输出。

Fuel 是可复现的语义限制：每个 token、对象解析、操作符、path segment、codec 输出单位和递归边必须按版本化 `FuelSchedule` 在执行前扣费。同一输入、profile 和 schedule 在不同机器上应得到相同的 fuel 结果。Wall-clock deadline 是防止实现 bug、FFI 卡死和环境异常的外层 watchdog；它可以终止 worker，但不得作为 O0-O3 golden 的预期错误，也不得用来声称某个文件在规范意义上“过于复杂”。

Cancellation 与 fuel 分离。所有长循环在最多 `cancellation_check_interval_fuel` 后检查 token；ReleaseProfile 另行约束实际取消 p95/p99。Native 超限返回稳定 `ResourceLimit`，不得通过外部引擎重试来获得第二份预算。

### 13.3 桌面沙箱

- engine worker 只持有宿主授予的文件/共享内存句柄，不拥有任意文件系统和网络权限。
- 产品只启动 Native engine worker；其内存、CPU、句柄和进程生命周期由平台沙箱限制。
- worker crash 后 UI 可重建 session；未保存 ChangeSet 由 UI/宿主持久化，不只存在于 worker。
- IPC 解码同样视为不可信输入，执行长度、枚举和 handle 验证。

### 13.4 可观测性

| 信号 | 字段 |
| --- | --- |
| Open trace | source 类型、Range、xref、page tree、首屏阶段耗时 |
| Scene trace | operators、commands、resources、feature、complexity |
| Render trace | backend、tile、queue、raster、blend、upload、cache |
| Capability event | status、profile、missing requirements、scope、location、policy version |
| Budget event | budget kind、limit、consumed、对象/stream/page 位置 |
| Crash bundle | build、平台、错误、最小技术摘要；不含原文档 |
| Product metric | preview/full tile、scroll frame、memory、supported/unsupported coverage |

### 13.5 隐私规则

- 默认遥测不得包含 PDF 字节、文本、文件名、URL query、密码或注释内容。
- 允许上传的内容仅包括不可逆文档 hash、feature 摘要、错误代码、大小分桶和性能数据。
- 需要原始样例时必须显式授权，并在接收后执行访问控制、保留期限和最小化。
- 浏览器日志不得把用户文本对象直接打印到 console。

## 14. 公共 API、IPC 与错误模型

### 14.1 Engine API

```text
pub trait Engine {
    fn open(&self, request: OpenRequest) -> RequestId;
    fn close(&self, session: SessionId);
    fn set_viewport(&self, session: SessionId, request: ViewportRequest);
    fn request_text(&self, session: SessionId, page: u32) -> RequestId;
    fn search(&self, session: SessionId, query: SearchQuery) -> RequestId;
    fn apply_changes(&self, session: SessionId, changes: ChangeSet) -> RequestId;
    fn save(&self, session: SessionId, target: SaveTarget) -> RequestId;
}
```

上面的 trait 是同进程 convenience API，不承担事件传输。实际 runtime 必须同时实现命令/事件端口，避免“返回 RequestId 但无处接收完成事件”的隐式全局回调：

```text
pub trait EnginePort: Send + Sync {
    fn submit(&self, command: CommandEnvelope) -> Result<(), ProtocolError>;
    fn try_recv(&self) -> Option<EventEnvelope>;
    fn wake_handle(&self) -> WakeHandle;
}
```

浏览器 adapter 把 EnginePort 映射到 `postMessage`，桌面 adapter 映射到 IPC queue。回调不得在 engine 内部锁、网络回调或 codec/平台 FFI 栈中同步调用宿主。

### 14.2 Handle 规则

- 所有跨边界对象使用 64 位 opaque handle，不传原生指针。handle 包含或映射到 generation，防止 use-after-close。
- 请求有 `RequestId`，事件可乱序到达；客户端按 request/generation 丢弃旧结果。
- close 是幂等的；关闭后新请求返回 `SessionClosed`，已有任务进入取消。
- Surface handle 有明确 owner、大小、格式、stride、释放协议和超时回收。

### 14.3 协议版本

```text
pub struct ProtocolHello {
    pub major: u16,
    pub minor: u16,
    pub schema_hash: [u8; 16],
    pub capabilities: HostCapabilities,
}

pub struct EnvelopeHeader {
    pub major: u16,
    pub minor: u16,
    pub message_type: u16,
    pub flags: u16,
    pub payload_len: u32,
    pub sequence: u64,
}

Compatibility:
- major mismatch: reject
- same major, host minor >= required: accept
- unknown optional field: ignore
- unknown mandatory capability: reject with explicit error
```

每个 transport 在解码 payload 前验证 envelope 长度、全局/消息类型上限、sequence 和 transfer slot 数量。浏览器结构化 clone 对象也必须先经过生成的 runtime validator，不能因为 TypeScript 静态类型存在就跳过不可信消息验证。

### 14.4 错误模型

```text
pub struct EngineError {
    pub code: ErrorCode,
    pub category: ErrorCategory,
    pub severity: Severity,
    pub recoverability: Recoverability,
    pub location: Option<ErrorLocation>,
    pub diagnostic_id: DiagnosticId,
}

pub enum ErrorCategory {
    Source,
    Syntax,
    Xref,
    Object,
    Decode,
    Security,
    Document,
    Content,
    Font,
    Color,
    Text,
    Render,
    Write,
    Unsupported,
    Budget,
    Cancelled,
    Internal,
}
```

### 14.5 错误与能力决策的关系

| Recoverability | 行为 |
| --- | --- |
| RetryAfterData | 发出 NeedData，数据到达后继续 |
| RetryWithPassword | 请求密码，不记录密码内容 |
| RetryNativeRenderer | GPU/context/资源路径可复用同一 Native Scene，以新的 backend class、RenderConfigHash 和 RendererEpoch 切换 Fast CPU；不得调用外部引擎 |
| UnsupportedCapability | 返回第 11.3 节的 CapabilityDecision；UI 显示范围和缺失能力 |
| UserActionRequired | 提示损坏、权限、资源限制或不支持 |
| FatalInternal | 终止 worker/session，生成安全 crash bundle |

`Recoverability` 由 error category、具体 code 和 policy version 共同映射，不由调用点随意指定。`Unsupported`、`Budget`、`SourceIntegrity` 和 `Internal` 不得映射到任何外部 PDF 引擎。CapabilityDecision 与 EngineError 可以同时出现：前者描述产品能力范围，后者描述本次操作为何未完成。

### 14.6 Surface 协议

跨边界协议不得传递只能在某个 Wasm instance 内解释的裸指针。Surface 分为通用 metadata、同 Worker ABI 和可传输 envelope：

```text
pub struct SurfaceMetadata {
    pub id: SurfaceId,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
    pub alpha: AlphaMode,
}

// Only callable by JS glue in the same Worker and Wasm Memory.
pub struct WasmLocalSurface {
    pub metadata: SurfaceMetadata,
    pub memory_epoch: u32,
    pub ptr: u32,
    pub len: u32,
}

// Serialized message plus an out-of-band transfer list.
pub struct SurfaceEnvelope {
    pub metadata: SurfaceMetadata,
    pub transport: SurfaceTransport,
}

pub enum SurfaceTransport {
    BrowserTransfer { slot: TransferSlot, kind: BrowserTransferKind },
    SharedMemory { handle: PlatformHandle, offset: u64, len: u64 },
    GpuTexture { handle: PlatformGpuHandle, backend: GpuBackend },
}
```

`BrowserTransfer` 的 slot 必须对应同一 `postMessage` transfer list 中的实际 `ImageBitmap` 或 `ArrayBuffer`，不是跨 realm 全局 token。OffscreenCanvas 只可作为 Worker-private staging，不能作为 DOM-bound canvas 的 transfer 或 wire `SurfaceTransport` variant。普通 Wasm Memory 的 `ptr` 只能由同 Worker 的 JS glue 读取；若需要发往主线程，必须复制/转移到 `ArrayBuffer`、创建 `ImageBitmap`，或在满足跨源隔离时使用经协商的 SharedArrayBuffer。Wasm memory grow 会增加 `memory_epoch`；旧 local surface 在 epoch 不匹配时拒绝访问。

Surface owner 是创建它的 worker/session。Envelope 必须定义 acquire、一次性 transfer、release/ack、超时回收和 close 行为。所有 `stride * height`、`offset + len`、像素格式和 handle 权限在接收端重新验证。桌面 OS handle 不得出现在浏览器 schema；协议生成器按 capability 生成互斥 variant。

## 15. 开发流程、里程碑与发布治理

### 15.1 Feature 开发闭环

```text
规范/问题研究
   ↓
research note + provenance
   ↓
最小 PDF + manifest + expected semantics
   ↓
Reference implementation
   ↓
O0/O1/O2 oracle review
   ↓
Fast implementation + benchmark
   ↓
fuzz seed + corpus classification
   ↓
DIFFERENTIAL / CANARY rollout
   ↓
指标达到绑定 ReleaseProfile 门槛
   ↓
DEFAULT / STABLE
```

### 15.2 PR 必备材料

- 关联规范条款、勘误或回归 issue。
- 新增/修改测试及其 provenance；若无测试，必须说明无法测试的客观原因。
- 正确性差分结果、oracle authority、推导材料和 golden 审查说明。
- 性能敏感路径提供 benchmark 前后结果和峰值内存。
- 新增 unsafe、FFI、依赖、生成数据或许可证变化单独标注。
- CapabilityProfile/FeatureReport、CapabilityDecision/错误和 ReleaseProfile 影响更新。

### 15.3 里程碑

| 里程碑 | 主要交付物 | 退出条件 |
| --- | --- | --- |
| M0 质量基础设施 | Manifest、生成器、runner schema、diff 骨架、benchmark、corpus、外部 baseline runner | synthetic parse/scene/text/pixel outputs 可生成完整 failure bundle；baseline 与产品构建依赖隔离 |
| M1 字节与对象 | SourceSnapshot、Range 暂停/恢复、syntax、xref、object、R0/R1 repair | Range 乱序/取消/source-change E2E 通过；页数/目录基础服务进入 DIFFERENTIAL |
| M2 文档与内容 | page tree、resources、Content VM、Scene v1 | Scene 规范用例和 canonical diff 稳定 |
| M3 Reference renderer | 路径、clip、基础文字/图片/透明；reference-raster-v1 | O0/O1 样例验证 Reference；经审核 O3 golden 覆盖 R0 Native 图形 |
| M4 Fast CPU + 桌面 vertical slice | tile、调度、完整缓存键、共享内存、Native RenderPlan、CapabilityDecision | R0 eligible 基础页面进入 CANARY；桌面打开/滚动/缩放像素闭环可用；不新增 advanced text |
| M5 浏览器 vertical slice | 单一 Native Engine Worker、OffscreenCanvas、transferable surface、TS viewer | 三浏览器引擎完成 M6 前的 Native 像素阅读闭环；资源/依赖扫描证明无外部 PDF 引擎；不新增文本语义/交互协议 |
| M6 R0 Release Candidate | 水平 CMap、ToUnicode/声明 encoding、R0 水平字体 profile、TextAtom/quad/confidence、选择、复制、搜索、链接、release-r0-v1、固定硬件 perf | `r0.font.horizontal.v1` 与 `r0.text.horizontal-ltr.v1` 达到发布要求；第 2.6 节全部 R0 gate 通过；本里程碑对应第 16.3 节“首个可用版本” |
| FT1 Post-R0 Font/Text + structure | RTL、竖排 CIDFont、ActualText、Tagged PDF、structure order、可访问性树；系统字体 fallback 仅作独立决策 gate | 各能力由互不混淆的 profile 和 O0-O3/holdout/E2E 证据晋级；不得追溯扩大 R0 |
| M7 高级图形/颜色 | soft mask、Pattern、Shading、DeviceN、ICC | 对应 CapabilityProfile 按第 11.7 节进入 DEFAULT；组合 supported/unsupported 有固定分母和阈值 |
| M8 注释与 writer | ChangeSet、增量保存、基础 form | 保存自检和签名感知完成 |
| M9 GPU | WebGPU 后端、context recovery、设备矩阵 | 指定 feature set 进入 CANARY/DEFAULT |
| M10 目标范围稳定化 | 目标产品 CapabilityProfile 全部达到 STABLE；长期回归与 API 兼容策略 | 目标范围 Native supported rate 达门槛；产品依赖纯度和回滚演练持续通过 |

里程碑是依赖 gate，不是日历承诺。项目启动前必须增加 `plan/r0.toml`，为 M0-M6 填写 owner、reviewer、计划 FTE、开始/目标日期、依赖、容量假设和最大并行工作数；未填写前本文档保持“候选稿”状态。关键依赖链为 M0 → M1 → M2 → M3 → M4 → M6；M5 可在 protocol/surface schema 冻结后与 M3/M4 后半段并行，但不能绕过 M4 的 CapabilityDecision 和 Native RenderPlan 契约。M6 的细化交付物、依赖与退出条件由 `plan/m6.toml` 管理；FT1 由 `plan/post-r0-font-text.toml` 管理，只有 M6/R0 退出后才能开始能力晋级。

### 15.4 建议工作流

| 工作流 | 职责 |
| --- | --- |
| 规范与 Conformance | 规范映射、勘误、最小测试、争议裁决 |
| Parser/Security | bytes、syntax、xref、filters、repair、预算 |
| Graphics/Color | Content VM、Scene、透明、颜色、Reference/Fast |
| Font/Text/A11y | 字体、CMap、Unicode、选择、搜索、structure |
| Runtime/Platform | 调度、缓存、IPC、浏览器、桌面、surface |
| Quality/Corpus | 生成器、diff、fuzz、benchmark、CI、语料治理 |
| Baseline/Release | 外部 runner、版本/许可指纹、能力状态、发布依赖纯度和回滚 |

这些是能力工作流，不必一一对应人员。早期团队可由同一工程师承担多个工作流，但每个高风险 feature 至少需要实现 reviewer 与测试/规范 reviewer 两个视角。

### 15.5 发布策略

- 所有 Native feature 由 policy feature set 控制，可按文档、页面、设备、版本或 canary cohort 开关。
- 回滚只改变 Native capability policy，不改变文档数据；被禁用能力返回结构化 Unsupported，不调用外部引擎。
- 每个 release 扫描依赖、二进制、Wasm、网络和安装 manifest，证明产品不包含 PDFium/其他完整 PDF 引擎。
- 每个 release 发布 Native supported/unsupported 覆盖、原因分布、性能变化和已知限制。
- 从 DEFAULT 到 STABLE 的决策必须经过质量评审，而不是仅由代码 owner 决定。

## 16. 风险、开放决策与验收标准

### 16.1 主要风险

| 风险 | 领先指标 | 缓解与备选 |
| --- | --- | --- |
| 范围失控 | 长期只有 parser，没有可展示产品闭环 | ReleaseProfile R0 固定；M4/M5 强制 Native-only vertical slice；未支持能力明确展示 |
| Reference 太慢/难维护 | golden 生成成本过高 | Reference 只用于测试/小页面；保持分层和明确算法 |
| 字体与 CJK 延期 | 文本/页面 unsupported 长期高 | 独立 font/text 工作流；优先高频字体；低置信度显式提示和指标 |
| 颜色/透明复杂度 | 复杂页面差异难归因 | Scene 保留语义；三层 diff；逐 feature 开放 |
| 测试 Oracle 错误 | 错误 golden 固化 | 多引擎差分 + 规范裁决；禁止自动采纳 PDFium |
| 许可证污染 | PR 出现来源不明代码/表 | provenance、扫描、生成器、双 reviewer |
| 浏览器复制成本 | 主线程卡顿、Wasm 内存峰值 | Worker、OffscreenCanvas、transferable、可选共享内存 |
| 外部基线污染产品 | release 依赖或资源清单出现 PDFium/外部引擎 | tools/product 单向依赖、独立 CI layer、发布产物与网络清单扫描 |
| GPU 稳定性 | 设备差异、context lost | CPU 始终可用；GPU 只开放已验证 feature set |
| Corpus 过拟合 | 固定集很好，用户文件失败 | 训练/验收 corpus 分离、采样分层、持续最小化 |

### 16.2 立项时仍需确认的决策

| 决策 | 建议默认值 |
| --- | --- |
| 项目名称与 package namespace | 目录保持职责名；发布包使用统一项目名前缀 |
| 项目许可证与 allowlist | 由项目所有者选择；生产依赖按兼容性逐项批准，不设置 Apache-2.0 blanket deny |
| 首发桌面 UI | 若需要跨平台快速交付，复用 TypeScript viewer + native worker |
| 浏览器最低能力 | Dedicated Worker 必须；OffscreenCanvas/WebGPU 为能力增强 |
| Reference AA 算法 | 固定点 + 确定性 coverage，先由原子测试验证再冻结 v1 |
| 首版 codec 范围 | 基础 filter + 经批准 JPEG 叶子 codec；JPX/JBIG2 在实现或接入经批准叶子 codec 前明确 Unsupported |
| AcroForm 范围 | 首版只读/基本交互，不执行任意 JavaScript |
| 打印/专业色彩 | 独立于屏幕阅读路径，P2 决策 |

### 16.3 首个可用版本验收

首个可用版本等同于 M6/R0，不再使用未定义的“P0 corpus”或“基本可用”作为验收语言：

- 第 2.6 节 `release-r0-v1` 的正确性、稳定性、能力判定、支持覆盖、性能、内存、取消和浏览器 gate 全部通过。
- 桌面和三浏览器引擎均能对 R0 in-profile 本地/远程 PDF 完成首屏、滚动、缩放、旋转、目录、链接、选择、复制和搜索；out-of-profile 输入返回结构化 Unsupported。
- UI 主线程不执行重型解析或 raster；旧 generation 显示次数为 0；Range source 变化稳定返回 `SourceChanged`。
- 产品仅包含 Native worker；所有已发布 surface 来自同一 Native Scene/RenderConfig，不混入外部 baseline 输出。
- Unsupported、ResourceLimit、invalid input 和 internal fault 均包含 profile、位置和 policy，不调用外部引擎。
- O0-O3 oracle、T0/T1/release holdout、browser E2E、fuzz smoke、性能、SBOM/许可证和产品依赖纯度测试均为 release blocking。

### 16.4 成熟版本验收

- 连续两个稳定发布中，目标产品范围 in-profile Native supported rate ≥ 99.90%，critical/major 已知缺陷为 0，相关 CapabilityProfile 方可进入 STABLE。
- 在固定硬件池的离线比较中，首屏、滚动、缩放、跳页、搜索和峰值内存均满足 Native/PDFium baseline p95 或峰值比 ≤ 1.00，其中至少两个主要路径 ≤ 0.85。
- 复杂字体、透明、颜色、文本和可访问性分别拥有 CapabilityProfile、O0-O3 oracle、组合覆盖和 holdout 证明，不以简单页面数量稀释失败率。
- Reference、Fast、GPU、browser、desktop 的差分与性能结果均可由固定 build、profile、corpus hash 和环境镜像复现。
- 产品发布物持续通过“无外部 PDF 引擎”验证；离线 baseline 工具可以长期保留，但其可用性不影响构建、发布或用户请求。

## 附录 A. 操作符覆盖清单

下表用于建立初始 Content VM 与测试映射。每个操作符应在 `spec-map.toml` 中关联最小合法、非法 operand、状态上下文、资源缺失、预算和 metamorphic 用例。

| 组 | 操作符 | 测试重点 |
| --- | --- | --- |
| General graphics state | w J j M d ri i gs | 线宽、端点、连接、miter、dash、render intent、flatness、ExtGState |
| Special graphics state | q Q cm | 保存/恢复、矩阵乘法、栈不平衡 |
| Path construction | m l c v y h re | 空路径、退化曲线、巨大坐标、闭合 |
| Path painting | S s f F f* B B* b b* n | nonzero/even-odd、隐式 close、空 paint |
| Clipping | W W* | 延迟应用、嵌套 clip、空 clip |
| Text objects/state | BT ET Tc Tw Tz TL Tf Tr Ts | 上下文、字体缺失、render mode、rise |
| Text positioning | Td TD Tm T* | text/line matrix、旋转、竖排 |
| Text showing | Tj TJ ' " | 字符串、数组调整、word/char spacing |
| Type 3 | d0 d1 | width、bbox、颜色化规则、glyph context |
| Color space/color | CS cs SC SCN sc scn G g RG rg K k | 默认空间、Pattern、DeviceN、分量数 |
| Shading | sh | 资源、bounds、函数、颜色空间 |
| XObject | Do | Image/Form、递归、资源作用域 |
| Inline image | BI ID EI | 字典缩写、终止识别、过滤器 |
| Marked content | MP DP BMC BDC EMC | 属性、MCID、结构、栈不平衡 |
| Compatibility | BX EX | 未知操作符处理 |

## 附录 B. 建议 Feature taxonomy

```text
syntax.*
xref.table | xref.stream | xref.hybrid | xref.incremental
stream.ascii85 | stream.flate | stream.lzw | stream.dct | stream.jpx | stream.jbig2
security.standard.r2 ... security.standard.r6
font.type1 | font.truetype | font.cff | font.cid | font.type3
text.tounicode | text.actualtext | text.rtl | text.vertical | text.tagged
image.mask | image.softmask | image.interpolate
color.gray | color.rgb | color.cmyk | color.cal | color.lab | color.icc
color.indexed | color.separation | color.devicen | color.overprint
graphics.path | graphics.clip | graphics.shading | graphics.pattern
transparency.alpha | transparency.blend.* | transparency.group | transparency.knockout
annotation.* | form.acro.* | action.javascript | xfa | richmedia
writer.incremental | writer.xref_table | writer.xref_stream | signature.aware
platform.browser.offscreen | platform.browser.webgpu | platform.shared_memory
```

Feature taxonomy 用于索引和统计，不直接等同于能力判定。CapabilityProfile 使用版本化谓词表达参数与组合，例如：

```text
profile "transparency.soft-mask.luminosity.v1" {
  requires = [
    transparency.soft_mask(kind = luminosity),
    transparency.group(isolated = true, knockout = false),
    color.output(profile = srgb_reference_v1),
  ]
  excludes = [color.icc, color.devicen, transparency.knockout]
  renderer = [reference_v1, fast_cpu_v1]
  state = "CANARY"
}
```

Policy 只对完整谓词求值结果晋级。新增参数、交互 feature 或 renderer epoch 时，必须生成新的 profile version，不得静默扩大旧 profile 的含义。

## 附录 C. 推荐性能结果格式

```text
{
  "schema": 1,
  "commit": "<git-sha>",
  "build": {"profile": "release-lto", "features": ["fast-cpu"]},
  "environment": {
    "os": "...",
    "cpu": "...",
    "memory": "...",
    "browser": "...",
    "gpu": "..."
  },
  "corpus": {"id": "t1-2026-07", "hash": "..."},
  "scenario": "first-full-quality-viewport",
  "state": "cold",
  "samples_ms": [42.1, 41.8, 43.0],
  "median_ms": 42.1,
  "p95_ms": 43.0,
  "peak_memory_bytes": 67108864,
  "bytes_downloaded": 183204,
  "capability": {
    "profile": "transparency.soft-mask.luminosity.v1",
    "decision": "supported",
    "unexpected_unsupported": 0
  },
  "external_baseline": {
    "id": "pdfium-<revision>-<build-hash>",
    "p95_ratio": 0.89,
    "peak_memory_ratio": 0.93
  },
  "counters": {
    "objects_parsed": 418,
    "scene_commands": 2317,
    "tiles_rendered": 8,
    "stale_jobs": 0
  }
}
```

## 附录 D. Native 能力成熟度跟踪记录

```text
capability_profile = "transparency.soft-mask.luminosity.v1"
state = "CANARY"
owner = "graphics"
profile_version = 1
renderer_epochs = ["reference_v1", "fast_cpu_v1"]

[coverage]
spec_cases = 38
regressions = 11
eligible_corpus_pages = 28419
in_profile_supported_rate = 0.9987
unexpected_unsupported_rate = 0.0004

[quality]
critical_visual_diffs = 0
major_visual_diffs = 0
text_diffs = 0
panic_or_hang = 0
capability_false_positive = 0
capability_false_negative = 0

[performance.external_baseline]
baseline_id = "pdfium-<revision>-<build-hash>"
native_p50_ratio_to_pdfium = 0.81
native_p95_ratio_to_pdfium = 0.89
peak_memory_ratio_to_pdfium = 0.93

[release_evidence]
canary_days = 14
eligible_page_visits = 100000
rollback_drill_passed = true

[blockers]
items = ["knockout combination remains excluded from this profile"]
```

该记录描述 Native capability profile 的成熟度；`performance.external_baseline` 只是可选的离线对照证据。没有 PDFium 或其他 baseline 时，产品仍必须能够构建、发布并对相同输入给出 Native 结果或稳定的 `CapabilityDecision::Unsupported`。

## 附录 E. 参考资料

规范实现应固定具体版本和勘误快照。本文档编写时，PDF Association 已发布包含 Errata Collection 3 的 PDF 2.0 资源；项目应把下载文件的哈希纳入 build/test 元数据 [R2]。

- [R1] [ISO 32000-2:2020 - Document management - Portable document format - Part 2: PDF 2.0](https://www.iso.org/standard/75839.html)。International Organization for Standardization。访问日期：2026-07-13。
- [R2] [Sponsored ISO standards for PDF technology / PDF 2.0 Errata Collection 3](https://pdfa.org/sponsored-standards/)。PDF Association。访问日期：2026-07-13。作为 2026-06 的规范与勘误基线入口。
- [R3] [PDFium README and standalone test program](https://pdfium.googlesource.com/pdfium/+/master/README.md)。PDFium project。访问日期：2026-07-13。
- [R4] [PDFium LICENSE](https://pdfium.googlesource.com/pdfium/+/main/LICENSE)。PDFium project。访问日期：2026-07-13。
- [R5] [HTML Standard - Canvas and OffscreenCanvas](https://html.spec.whatwg.org/multipage/canvas.html)。WHATWG。访问日期：2026-07-13。
- [R6] [WebAssembly JavaScript Interface 2, W3C Candidate Recommendation Draft](https://www.w3.org/TR/2026/CRD-wasm-js-api-2-20260527/)。World Wide Web Consortium。固定 2026-05-27 发布快照；访问日期：2026-07-13。编辑草案仅用于跟踪后续变化，不作为发布基线。
- [R7] [WebGPU Specification](https://www.w3.org/TR/webgpu/)。World Wide Web Consortium。访问日期：2026-07-13。
- [R8] [HTML Standard - structured data and SharedArrayBuffer isolation checks](https://html.spec.whatwg.org/multipage/structured-data.html)。WHATWG。访问日期：2026-07-13。
- [R9] [PDF-centric corpora index](https://github.com/pdf-association/pdf-corpora)。PDF Association。访问日期：2026-07-13。
- [R10] [PDF 2.0 example files](https://github.com/pdf-association/pdf20examples)。PDF Association。访问日期：2026-07-13。
- [R11] [PDF technical resources and test suites](https://pdfa.org/resource/?wpv-resource-type=test-suite)。PDF Association。访问日期：2026-07-13。

## 附录 F. 最终架构摘要

```text
1. Own the semantics:
   bytes → objects → document → content VM → Scene

2. Own the truth model:
   spec map + generated cases + O0-O3 oracle + audited Reference renderer

3. Optimize independently:
   Fast CPU → browser WebGPU → native GPU

4. Keep the browser thin:
   Rust/WASM engine + TypeScript host + DOM text/a11y layers

5. Keep the product boundary explicit:
   Native-only runtime + external black-box baselines in tools/CI + separate license ledgers

6. Promote by evidence:
   correctness + stability + performance + real corpus + rollback readiness
```
