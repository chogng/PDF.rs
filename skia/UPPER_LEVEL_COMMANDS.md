# 上层集成的绘制命令对照表

本文是实现 `pdf-rs-skia` **上层集成层**时使用的能力对照表，不是让 PDF.rs 或其他
调用方直接依赖 `Canvas`、`DisplayListBuilder` 或 `GpuCommandEncoder` 的 API 文档。
这些类型记录的是 Skia 下层目前能执行或编码的绘制能力；上层集成层应根据本表把上游
请求转换为合适的下层调用。

## 调用方向与职责

```text
PDF.rs / 其他上游
  └─ 描述“画什么”、页面/资源数据、目标与渲染选项
       └─ Skia 上层集成层
            ├─ 校验请求、管理资源生命周期与缓存
            ├─ 选择 CPU、GPU 或平台目标
            └─ 调用下层 Canvas、DisplayList 或 GPU encoder
                 └─ core / image / CPU / GPU / Metal 执行
```

上游不应直接创建 `Canvas`、`Surface`、`DisplayListBuilder`、`GpuCommandEncoder` 或选择
执行后端。它只提供渲染意图、目标描述与必要的源数据；Skia 上层负责将这些内容传递给
下层。这个方向与 PDFium 的 `FPDF_RenderPage*` 入口一致：入口建立 render context 和
device，随后由内部 renderer 与 device 完成实际绘制。

当前仓库中的 `Canvas`、`DisplayList` 和 GPU encoder 是下层能力，尚不是统一的上层
`RenderRequest` / `RenderTarget` 接口。本文用于约束该集成层未来应覆盖哪些能力，不能
据此把下层类型泄露给 PDF.rs。

### 位图输入

上层收到 PNG、JPEG 或 WebP 的**已编码字节**时，应先通过 facade 导出的
`ImageCodec::decode`（或带调用方配额的 `decode_with_limits`）解码为 `Image`，再登记并
绘制。`Image` 本身只表示紧密排列、straight-alpha 的 RGBA8 像素，既不识别也不保存
编码格式。`CodecLimits` 必须由处理不可信输入的上层按资源预算收紧；默认值只是通用
安全上限。未来 PNG/JPEG/WebP 等编码输出也应放在 `skia/codec`，而不是给 `Image` 加入
编码相关状态。

## 先看结论

| 能力 | CPU `Canvas`（即时执行） | `DisplayListBuilder`（录制后由 CPU 回放） | `GpuCommandEncoder`（录制后提交后端） |
| --- | --- | --- | --- |
| 清屏 | `clear` | `clear` | `clear` |
| 保存/恢复状态 | `save` / `restore` | `save` / `restore` | `save` / `restore` |
| 设置变换 | `set_transform`、`concat` | `set_transform`、`concat_transform` | `set_transform`、`concat_transform` |
| 矩形裁剪 | `clip_rect` | `clip_rect` | `clip_rect` |
| 填充矩形 | `fill_rect` | 无（用矩形 `Path` + `fill_path`） | `fill_rect` |
| 填充路径 | `fill_path` | `fill_path` | `fill_path` |
| 描边路径 | `stroke_path` | `stroke_path` | **无** |
| 绘制位图 | `draw_image` | `draw_image` | `draw_image` |
| 绘制文字 | `draw_glyph_run` | `draw_glyph_run` | **无** |
| 当前实际硬件后端 | CPU 可用 | CPU 可用 | Metal 目前仅支持 `clear` |

因此，`Canvas` 是现阶段下层命令最完整、也是语义参考实现；`DisplayList` 适合由上层
缓存或跨线程传递同一组 CPU 绘制；GPU 命令集和 Metal 后端都还没有覆盖完整的 Canvas
能力。

## 通用约定

- 坐标使用固定点 `Scalar`（Q16.16）；通过 `Scalar::from_i32` 或
  `Scalar::from_ratio` 创建。计算溢出返回 `NumericOverflow`，不会静默截断。
- `Rect::new(left, top, right, bottom)` 必须是正面积矩形（`left < right` 且
  `top < bottom`），坐标系原点在左上。
- `Color` 是 straight-alpha 的 sRGBA8。`BlendMode` 覆盖 Porter-Duff、Plus、Modulate 及
  Multiply/Screen/Overlay 等高级混合；它描述**像素合成**，不是路径的 union/intersect 等
  几何布尔运算。
- 变换是仿射矩阵 `(a, b, c, d, e, f)`。`set_transform` 替换当前变换；Canvas 的
  `concat(next)`、DisplayList/GPU encoder 的 `concat_transform(next)` 均表示先应用当前
  变换、再应用 `next`。
- `save` 保存变换和裁剪，`restore` 恢复最近一层；没有匹配的 `save` 会返回
  `RestoreUnderflow`。Canvas 默认最多 256 层，可由 `SurfaceLimits` 收紧。
- `clear` 总是作用于整个目标，忽略当前变换和裁剪。

## 1. CPU Canvas：下层即时执行路径

调用顺序为：`Surface::new(...)` → `surface.canvas()` → 以下命令。`Canvas` 持有对
`Surface` 的可变借用，结束后可经 `Surface::pixels()` 读取紧密排列的 RGBA8 像素。

### 状态命令

| 命令 | 作用 | 现有边界 |
| --- | --- | --- |
| `clear(color)` | 用一个颜色覆盖整个目标。 | 忽略状态；无返回值。 |
| `save()` | 压入当前变换与裁剪。 | 受 `max_save_depth` 限制。 |
| `restore()` | 弹出并恢复最近状态。 | 空栈报错。 |
| `set_transform(transform)` | 替换后续绘制使用的变换。 | 不会与旧变换相乘。 |
| `concat(transform)` | 把变换追加到当前变换。 | 可能因固定点计算溢出失败。 |
| `clip_rect(ClipRect::new(rect))` | 用变换后的矩形和当前裁剪求交。 | 当前变换含旋转或错切时返回 `UnsupportedTransform`；仅矩形裁剪。 |

### 图形命令

| 命令 | 作用 | 已确定的行为/边界 |
| --- | --- | --- |
| `fill_rect(rect, paint)` | 填充变换后的矩形。 | 通过通用路径光栅化，允许旋转/错切。 |
| `fill_path(path, rule, paint)` | 按 `EvenOdd` 或 `NonZero` 填充路径。 | 二、三次贝塞尔以固定步数展平；开放轮廓在填充时会隐式闭合。 |
| `stroke_path(path, width, paint)` | 描边路径。 | 宽度必须为正；固定步数曲线展平；只有**圆头、圆角**，没有 butt/square cap、miter/bevel join、虚线或描边对齐选项。 |
| `draw_image(image, destination, opacity, blend_mode)` | 绘制 RGBA8 图片到目标矩形。 | 最近邻采样；`opacity` 只乘源 alpha；旋转或错切变换被拒绝，尚无逆变换采样和滤镜。 |
| `draw_glyph_run(run, provider, paint)` | 根据字形轮廓填充一段已整形文字。 | 调用方负责字体查找、Unicode shaping 与 fallback；缺失字形会跳过；轮廓经与普通路径相同的填充管线绘制。 |

## 2. DisplayList：下层可移植 CPU 命令表

`DisplayListBuilder` 把资源和绘制命令录制为不可变 `DisplayList`，再通过
`Surface::execute_display_list(&list, &glyph_provider)` 回放。它的命令枚举为：

| `DrawCommand` / Builder 方法 | 参数 | 说明 |
| --- | --- | --- |
| `Clear` / `clear` | `Color` | 清屏。 |
| `Save` / `save` | 无 | 保存状态。 |
| `Restore` / `restore` | 无 | 恢复状态。 |
| `ClipRect` / `clip_rect` | `Rect` | 矩形裁剪。 |
| `SetTransform` / `set_transform` | `Transform` | 替换变换。 |
| `ConcatTransform` / `concat_transform` | `Transform` | 叠加变换。 |
| `FillPath` / `fill_path` | `PathId`、`FillRule`、`Paint` | 填充已登记的路径。 |
| `StrokePath` / `stroke_path` | `PathId`、正 `Scalar` 宽度、`Paint` | 描边已登记的路径。 |
| `DrawImage` / `draw_image` | `ImageId`、目标 `Rect`、`u8` opacity、`Paint` | 绘制已登记图片；使用 `paint.blend_mode()`。 |
| `DrawGlyphRun` / `draw_glyph_run` | `GlyphRunId`、`Paint` | 绘制已登记的整形字形序列。 |

资源须先经 `add_path`、`add_image` 或 `add_glyph_run` 登记，取得仅在该列表中有效的 ID；
`finish()` 后发布列表。构建器的 `max_items` 同时限制**命令数及每一种资源数**。

### 与 Canvas 的差异

- 没有 `fill_rect`：需要用 `PathBuilder::add_rect` 建路径后调用 `fill_path`。
- 命令本身不携带“当时状态”的快照；回放时按命令顺序维护状态。因此保存/恢复顺序是
  列表语义的一部分。
- 回放使用 Canvas，所以最终约束与 CPU Canvas 一致，包括图片轴对齐限制、描边样式和
  文字轮廓解析要求。

## 3. GPU：下层编码与提交路径

`GpuCommandEncoder` 是另一套后端中立的命令表，不是 `DisplayList` 的执行器。流程为：
创建 encoder → 登记资源 → 录制命令 → `finish()` → 由实现 `GpuBackend` 的后端
`submit` 到 `GpuSurfaceDescriptor` 指定大小的表面。

| `GpuCommand` / Encoder 方法 | 参数 | 说明 |
| --- | --- | --- |
| `Clear` / `clear` | `Color` | 清空完整目标，不受裁剪影响。 |
| 状态 / `save`、`restore`、`set_transform`、`concat_transform`、`clip_rect` | 同名状态参数 | 这些方法修改 encoder 状态；每个绘制命令会记录当时的变换和 target-space scissor 快照。 |
| `FillRect` / `fill_rect` | `Rect`、`Paint` | 填充矩形。 |
| `FillPath` / `fill_path` | `GpuPathId`、`FillRule`、`Paint` | 填充已登记路径。 |
| `DrawImage` / `draw_image` | `GpuImageId`、目标 `Rect`、`u8` opacity、`BlendMode` | 绘制已登记 RGBA8 图片。 |

GPU encoder 也要求先调用 `add_path` / `add_image`。`GpuCommandLimits` 可分别限制命令、路径、
图片和状态栈深度。裁剪仍只接受轴对齐变换；裁剪为空时，后续绘制命令不会被录制。

### GPU 当前缺口

- GPU 命令层没有 `stroke_path`、`draw_glyph_run`，也没有专用的渐变、滤镜、图层或
  复杂裁剪命令。
- `SoftwareGpuBackend` 能用 CPU Canvas 回放上述 GPU 命令，主要用于一致性测试，并不是真正的
  硬件 GPU 实现。
- `MetalBackend` 当前会真实创建纹理并执行 `Clear`；遇到 `FillRect`、`FillPath` 或
  `DrawImage` 会返回 `UnsupportedCommand`。即 Metal 的着色器管线虽已有准备，硬件绘制命令尚未
  落地。

## 4. 路径构造能力（为下层绘制准备资源）

路径不是 `Canvas` 状态命令，但它决定 `fill_path` 和 `stroke_path` 可以表达哪些图形。使用
`PathBuilder::new(max_verbs)` 创建，并以 `finish()` 发布不可变 `Path`。

| 分组 | 方法 | 说明 |
| --- | --- | --- |
| 基本轮廓 | `move_to`、`line_to`、`quad_to`、`conic_to`、`cubic_to`、`close` | 直线、二次/有理二次/三次贝塞尔；除 `move_to` 外必须有活跃轮廓。 |
| 基本形状 | `add_rect`、`add_oval`、`add_circle`、`add_round_rect` | oval/圆角使用确定性的三次贝塞尔近似；圆半径必须正，圆角半径不得为负且会夹到矩形半宽/半高。 |
| 多边形 | `add_polygon` | 接受开放或闭合多边形；开放至少两个点，闭合至少三个点。 |
| 椭圆弧 | `add_arc` | 从 `ArcStart` 开始、按 `ArcDirection` 画 1–4 个 90° 段。 |
| 任意角度弧 | `add_arc_degrees`、`arc_to` | `Angle` 使用顺时针 canvas 度数；扫角不能为 0，绝对值不能超过一整圈；最多拆成四段三次贝塞尔。`arc_to` 会在需要时先连一条直线到弧起点。 |
| 旋转椭圆弧 | `add_rotated_arc_degrees`、`arc_to_rotated` | 在椭圆中心旋转后输出三次贝塞尔段；参数仍使用确定性 Q16.16 角度。 |
| 组合/查询 | `append_path`、`Path::reversed`、`Path::transformed`、`Path::bounds`、`Path::tight_bounds` | 支持追加、反向、生成变换副本、控制点保守边界和多项式贝塞尔 extrema-aware 保守边界。 |

## 用这份表排查不足

优先确认目标调用路径属于哪一层；不要把“CPU 已可画”误判为“DisplayList 或 Metal 已可画”。
当前最明显的不对齐项是：

1. `DisplayList` 缺少 `fill_rect` 的直接命令；
2. GPU 命令层缺少描边与文字；
3. Metal 尚未实现任何非清屏命令；
4. 裁剪仍只有矩形，图片仍是 RGBA8；
5. 图片不支持非轴对齐变换/过滤，描边样式也只有圆头圆角；
6. 文本层只消费调用方已完成整形的 `GlyphRun`，不提供字体解析、fallback 或 shaping；
7. 路径的几何布尔运算、stroke-to-path 和 path effects 尚未暴露；它们不能由像素混合模式替代。

源码入口：Geometry 在 `geometry/src/lib.rs`，Path 在 `path/src/lib.rs`，CPU Canvas 在
`cpu/src/canvas.rs`，DisplayList 在 `core/src/display_list.rs`，GPU 命令层在
`gpu/src/lib.rs`，Metal 后端在 `gpu/metal/src/lib.rs`。
