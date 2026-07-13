# PDF.rs 可溯源与来源治理规范

- 文档编号：RPE-STD-004
- 版本：0.1
- 状态：稳定态开发基线（持续迭代）
- 适用范围：规范、设计、代码、测试、数据、依赖、外部观察与发布证据

## 1. 目的

本规范建立以下双向可审计链路：

```text
规范/勘误/项目决策
        ↕
Feature + CapabilityRequirement
        ↕
实现模块/算法/数据
        ↕
测试 Case + Oracle + Corpus
        ↕
PR/ADR + CI/Benchmark/Fuzz 证据
        ↕
ReleaseProfile + 发布结果
```

团队必须能够从任一已发布能力追到其规范依据、实现 owner、测试和发布证据，也必须能从某条规范追到覆盖它的 feature、实现和测试。

可溯源同时服务于正确性、自主实现证明、许可证治理、供应链审计和回归影响分析。

## 2. 来源分类

| 类型 | 示例 | 可用于什么 |
| --- | --- | --- |
| Normative | ISO 32000、勘误、Web/Wasm/WebGPU 标准 | 定义行为与约束 |
| Analytic | 数学推导、公开标准算法、人工小样例 | O1 oracle、算法依据 |
| Project decision | 架构文档、ADR、ReleaseProfile | 定义项目稳定契约 |
| External observation | PDFium/其他黑盒输出、视觉检查 | O4 问题发现 |
| External implementation | 开源项目源码和设计 | 仅在许可与自主实现政策允许范围内研究 |
| Dependency | codec、crypto、font、ICC 等叶子库 | 经审批后实现非 PDF 语义能力 |
| Data | 字体、CMap、颜色 profile、测试 PDF | 经来源/许可/哈希治理后使用 |
| Generated artifact | 表、golden、shader、schema、bindings | 由固定生成器和输入重现 |

外部 observation 不能升级为 normative；“多个引擎都这么做”只能触发 O2 裁决，不能替代规范分析。

## 3. 稳定标识符

所有映射使用稳定 ID，不使用标题或文件路径作为唯一身份。

| 对象 | ID 示例 |
| --- | --- |
| 规范条款 | `ISO-32000-2:2020/11.6.5` |
| 勘误 | `PDF20-ERRATA-EC3/<item-id>` |
| 架构决策 | `AD-007` |
| ADR | `ADR-0012` |
| Feature | `transparency.soft-mask.luminosity` |
| CapabilityProfile | `transparency.soft-mask.luminosity.v1` |
| 测试 case | `transparency/soft-mask-luminosity-004` |
| Corpus manifest | `t2-2026-07@sha256:<hash>` |
| Fuel schedule | `fuel-v1` |
| Renderer epoch | `reference-v1`、`fast-cpu-v1` |
| Diagnostic | `RPE-RENDER-0017` |
| ReleaseProfile | `release-r0-v1` |

ID 一旦进入主干不得复用。重命名使用 alias/deprecation 映射；删除对象保留 tombstone 和最后适用版本。

## 4. 仓库记录结构

```text
docs/
├── architecture/
├── standards/
├── protocol/
├── adr/
├── research/
│   └── <feature-id>.md
└── traceability/
    ├── spec-map.toml
    ├── feature-map.toml
    ├── dependency-ledger.toml
    ├── data-ledger.toml
    └── baseline-ledger.toml

core/<module>/PROVENANCE.md
tests/cases/<case-id>/case.toml
tests/corpus/manifests/<corpus-id>.toml
release/profiles/<release-profile>.toml
```

机器可读文件是审计和 CI 的事实来源；Markdown 解释理由、风险和上下文。两者冲突时阻断发布并由 owner 修复，不允许默认为某一方正确。

## 5. 规范映射

`spec-map.toml` 至少表达：

```toml
[[requirement]]
id = "ISO-32000-2:2020/11.6.5"
snapshot_hash = "sha256:<licensed-local-snapshot-hash>"
summary = "Soft-mask requirements"
features = ["transparency.soft-mask.alpha", "transparency.soft-mask.luminosity"]
implementation = ["core/scene", "core/reference", "core/fast_cpu"]
tests = [
  "transparency/soft-mask-alpha-001",
  "transparency/soft-mask-luminosity-004",
]
status = "partial"
notes = "docs/research/transparency.soft-mask.md"
```

- 规范快照必须固定版本、勘误集合和 hash。
- `status` 使用 `unmapped`、`planned`、`partial`、`covered`、`excluded`。
- `excluded` 必须说明产品范围、CapabilityDecision 和 ReleaseProfile 影响。
- 一个测试可以覆盖多个条款，但必须标注主要断言，避免虚假覆盖。
- “有链接”不等于 covered；covered 要求存在可执行断言和明确 oracle。

## 6. Feature 映射

`feature-map.toml` 是能力成熟度和影响分析的事实来源：

```toml
[[feature]]
id = "transparency.soft-mask.luminosity"
owner = "graphics"
state = "CANARY"
profile = "transparency.soft-mask.luminosity.v1"
clauses = ["ISO-32000-2:2020/11.6.5"]
modules = ["core/content", "core/scene", "core/reference", "core/fast_cpu"]
tests = ["transparency/soft-mask-luminosity-004"]
fuzz_targets = ["content_vm", "soft_mask_scene"]
benchmarks = ["soft-mask-tile"]
introduced_in = "0.1.0"
```

Feature 状态只描述 Native 证据成熟度：`PLANNED → REFERENCE → DIFFERENTIAL → CANARY → DEFAULT → STABLE`。它不描述 PDFium 生命周期。

## 7. 模块 `PROVENANCE.md`

每个 PDF 语义核心和自主 renderer 模块必须维护 `PROVENANCE.md`：

```markdown
# Scope

# Semantic owner

# Normative sources

# Algorithms and derivations

# External observations

# Dependencies and generated data

# Tests and fuzz targets

# Known deviations and unsupported cases

# History
```

要求：

- 记录具体条款、版本和勘误，不只写“PDF spec”。
- 标准算法说明推导或权威来源。
- 外部观察记录 runner/version 和观察结论，不复制实现代码。
- 列出模块依赖的生成数据、输入 hash 和生成器。
- 列出已知偏差、CapabilityProfile 排除项和关联 issue。
- 影响语义的 PR 必须同步更新。

## 8. 自主实现与外部研究记录

项目采用规范与测试驱动的独立实现。允许：

- 阅读规范、勘误、论文和标准算法；
- 运行外部引擎观察行为；
- 阅读外部项目的高层架构并形成问题/约束型 research note；
- 在许可允许且经过审查后使用非 PDF 语义叶子库。

禁止：

- 复制、机械翻译或仅改名外部核心函数、状态机和常量表；
- 从外部代码逐语句生成本项目实现；
- 用外部输出直接生成 golden 后宣称正确；
- 把外部完整 PDF/2D 引擎链接、vendoring 或编译进产品；
- 删除来源记录以规避审查。

Research note 必须区分“规范要求”“外部观察”“本项目推导”“待裁决”。如果实现作者深度接触了某段外部代码，PR 中必须披露项目、文件/模块、许可证和采取的隔离措施，由 reviewer 判断是否需要重新设计或额外审查。

## 9. 外部 baseline 记录

`baseline-ledger.toml` 至少包含：

```toml
[[baseline]]
id = "pdfium-<revision>-<build-hash>"
engine = "pdfium"
upstream_revision = "<revision>"
build_hash = "sha256:<hash>"
runner_schema = 1
platform = "linux-x86_64"
fonts = "fonts-<hash>"
color = "color-<hash>"
license_manifest = "licenses/pdfium-<hash>.json"
distribution = "ci-only"
```

- Baseline 属于开发/CI 工具账本，不进入产品依赖或产品 SBOM。
- Runner 必须是进程级黑盒，只处理固定 hash 的测试对象。
- 团队分发 runner/CI image 时，必须提供该分发物实际闭包的许可证材料。
- Baseline 输出是 O4 observation；其变化不得自动修改 Native 行为。
- Release 构建不依赖 baseline 可用性，且扫描确认无外部引擎残留。

## 10. 依赖决策记录

每个新增或重大升级依赖必须记录：

```toml
[[dependency]]
name = "<crate/package>"
version = "<exact-or-lock-reference>"
scope = "product|development|test|toolchain"
semantic_owner = "<project module>"
purpose = "<bounded purpose>"
source = "<registry/repository>"
source_hash = "sha256:<hash>"
license_expression = "<SPDX>"
license_decision = "approved|rejected|conditional"
redistribution = "<notes>"
failure_isolation = "<boundary>"
budget_hook = true
cancellation_hook = true
wasm = "supported|not-applicable|blocked"
native = "supported|not-applicable|blocked"
replacement_plan = "<plan>"
owner = "<team>"
reviewed_at = "<date>"
```

生产、开发、测试、工具链和 corpus 分开建账。Apache-2.0 不被 blanket deny，但仍按项目许可证、分发方式和组织政策逐项审查。GPL/AGPL、未知、自定义或无许可证默认阻断，直到书面批准。

## 11. 数据与生成物来源

字体、CMap、ICC profile、编码表、Unicode 数据、shader、测试 PDF 和 golden 必须记录：

- 来源 URL/repository 和获取日期；
- 精确版本/revision 与输入 hash；
- 许可证/SPDX、归属、修改和再分发条件；
- 是否包含个人/机密数据；
- 生成器路径、版本、命令参数和输出 hash；
- owner、更新策略和删除策略。

公开可访问不等于允许复制或再分发。无法确认许可的数据不得进入普通仓库、release artifact 或共享 CI cache。

生成器必须可重放；输出头部或 manifest 记录 `generated_by`、`input_hashes`、`generator_revision` 和 `schema`。禁止手工修改生成文件后不更新生成器。

## 12. 测试 provenance

每个 fixture 的 `case.toml` 必须记录：

- 自建、生成、公开外部或私有授权；
- source hash 和最小化前后关系；
- 许可证、可再分发性和访问级别；
- Feature/条款/勘误；
- O0-O4 oracle 与推导；
- 首次失败、修复 issue 和 golden history。

从用户文件最小化得到的样例仍可能保留敏感或受版权保护内容；只有在人工确认不可逆去标识且获授权后，才能转入公开/self-authored corpus。

## 13. PR 与 ADR 要求

语义、协议、来源或依赖变更的 PR 描述必须包含：

```text
Requirements / feature IDs:
Behavioral change:
Implementation owner:
Tests and oracle levels:
Provenance changes:
Dependency/data/license changes:
CapabilityProfile impact:
ReleaseProfile impact:
Security/budget impact:
Known deviations:
```

以下变更必须有 ADR：

- 改变跨模块职责或依赖方向；
- 新增产品级第三方叶子能力；
- 改变公共协议 major/minor 兼容策略；
- 改变 canonical Scene、Text、Pixel 或错误契约；
- 改变 FuelSchedule、renderer epoch 或持久化格式；
- 永久偏离某项稳定规范。

ADR 必须不可变；决策变化通过新的 ADR supersede 旧 ADR。

## 14. 发布证据

每次 release 保存内容寻址的证据索引：

- ReleaseProfile/schema/hash；
- source commit、toolchain 和依赖 lock/SBOM；
- 规范/feature map hash；
- corpus manifest 和 holdout 标识；
- 测试、fuzz、性能和能力成熟度结果；
- baseline id（若运行，仅作离线证据）；
- 产品构建无外部 PDF/2D 引擎扫描结果；
- 已批准例外、owner 和到期日；
- 构建 artifact hash 与签名。

发布证据必须足以重现“什么代码、什么输入、什么环境、按什么规则被判定为通过”。

## 15. CI 强制检查

CI 至少验证：

- 所有 Feature/Case/ADR/Diagnostic ID 唯一且引用存在；
- spec-map 中 `covered` 条款具有 active 测试和有效 oracle；
- 核心模块存在 `PROVENANCE.md`；
- fixture/corpus 具有 hash、许可和访问策略；
- 生成物 hash 与生成器输出一致；
- 新依赖存在审批记录且 scope 正确；
- baseline/tool 依赖未进入 product graph；
- ReleaseProfile 引用的 feature/corpus/epoch 均存在；
- 文档链接和 schema 通过校验。

建议提供可查询命令：

```text
trace requirement <id>
trace feature <id>
trace test <id>
trace module <path>
trace release <profile>
```

## 16. 审计与异常处理

- 每个稳定发布前审查新增依赖、外部数据、O2 裁决和规范偏离。
- 周期性抽样验证记录与真实代码/fixture 一致。
- 发现来源不清、许可错误或疑似复制时，立即冻结相关 artifact 的发布和再分发，保留审计证据并由负责人裁决。
- 修复记录必须说明受影响版本、替换/删除范围、重新生成的 hash 和发布处置。
- 不得通过改写 Git 历史隐藏 provenance 问题；敏感内容移除按安全事件流程执行并保留受控审计记录。

## 17. 评审清单

- [ ] 每个行为变更可追到规范、勘误或项目决策。
- [ ] Feature、模块、Case、oracle 和 release 证据形成闭环。
- [ ] 外部观察与规范结论明确分开。
- [ ] 核心实现未复制或机械翻译外部完整引擎。
- [ ] 依赖、测试数据和生成物的许可/hash/owner 完整。
- [ ] PDFium 等 baseline 只存在于开发/CI 工具边界。
- [ ] PR/ADR/PROVENANCE/spec-map 已同步更新。
- [ ] 发布证据能够重现准入结论。
