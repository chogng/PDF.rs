# PDF.rs 开发文档

本目录保存 PDF.rs 的稳定态架构、工程规范和协议契约。文档描述目标产品边界；阶段性实验、迁移脚手架和短期计划进入 ADR、issue 或 roadmap，不写成永久架构。

## 规范入口

| 文档 | 编号 | 作用 |
| --- | --- | --- |
| [独立 Rust PDF 引擎开发设计文档](architecture/independent_rust_pdf_engine_development_spec.md) | RPE-ARCH-001 | 产品边界、总体架构、能力成熟度和发布门槛 |
| [代码规范](standards/coding-standard.md) | RPE-STD-001 | Rust/TypeScript、错误、并发、unsafe、依赖和评审规则 |
| [生命周期与并发规范](standards/lifecycle-and-concurrency.md) | RPE-STD-002 | Worker、Session、Request、Range、Surface、Cache、Save 生命周期 |
| [测试规范](standards/testing-standard.md) | RPE-STD-003 | O0-O4 oracle、golden、差分、fuzz、corpus、性能和 CI 准入 |
| [可溯源与来源治理规范](standards/traceability-and-provenance.md) | RPE-STD-004 | 规范—feature—实现—测试—发布双向追踪及许可证/来源记录 |
| [安全与资源预算规范](standards/security-and-resource-budget.md) | RPE-STD-005 | 不可信输入、FuelBudget、沙箱、隐私、供应链和安全响应 |
| [Engine 交互协议规范](protocol/engine-protocol.md) | RPE-PROTO-001 | Browser/Desktop Host 与 Native Engine 的命令、事件和资源所有权 |

## 文档优先级

出现冲突时按以下顺序处理：

1. 已批准且尚未被 supersede 的架构不变量和 ADR；
2. 公共协议、持久化格式和 ReleaseProfile 的版本化契约；
3. 本目录中的工程规范；
4. 模块 README/PROVENANCE、代码注释和局部约定。

冲突不得由实现者静默选择。影响产品边界、公共协议、错误语义、许可证、安全预算或可溯源性的变化必须通过 ADR，并同步修改相关文档和测试。

## 稳定态与持续迭代

“稳定态”表示文档描述长期目标结构，而不是表示永不修改。允许持续迭代，但必须遵守：

- 文档保留编号、版本、状态和修改记录；
- 破坏性契约变化升级相应 major/schema/epoch；
- 语义变化同时更新规范映射、CapabilityProfile、测试和发布证据；
- 临时例外记录 owner、风险、补偿措施和到期条件；
- 已批准 ADR 不原地改写，通过新 ADR supersede；
- Release 绑定具体文档/schema/profile hash，确保可重现。

## 规范性术语

- **必须 / MUST**：违反即阻断合并或发布。
- **应当 / SHOULD**：默认执行；偏离必须书面说明理由和补偿措施。
- **可以 / MAY**：可选实现，不改变公共契约。
- **Native**：本项目自主实现的 PDF 解析、Scene、文本和渲染路径。
- **Baseline**：仅在开发/CI 中运行的外部黑盒对照，不属于产品请求路径。

## 维护约定

- 文档 owner 与对应代码/协议 owner 一致。
- 每个语义 PR 检查是否需要更新架构、规范、PROVENANCE、case manifest 或 ADR。
- 文档中的代码片段是契约草图；canonical schema/类型落地后，由生成器或链接替代重复定义。
- Markdown 链接、编号、schema 示例和机器可读映射由 CI 校验。
- PDFium 可长期作为开发/CI baseline，但不得链接、vendoring、打包或动态下载到产品运行时。
