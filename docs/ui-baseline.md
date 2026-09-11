# 前端视觉基线（沉静 · 工具感）

本文是 `frontend/` 的 UI 单一事实源：改样式前先读。线条形态保持 shadcn 原有的细淡风格。token 定义在 `frontend/src/index.css`，Tailwind 扩展在 `tailwind.config.cjs`，基础组件在 `frontend/src/components/ui/`。

## 一句话

灰阶搭骨架、排版讲层级、颜色只标语义；线条细淡、阴影柔和，不做粗边 / 硬阴影 / 按压位移。

## Token

| 组 | 浅色 | 暗色 | 用途 |
|---|---|---|---|
| `--foreground`（`--ink`） | `#1C293C` 深石板蓝 | 暖白 | 正文 |
| `--background` | `#FBFBF9` 暖白 | 暖炭色 | 页面 surface；主内容区铺 `bg-muted/40` 衬出白卡片 |
| `--card` | 纯白 | 略亮于 surface | 卡片 / 弹窗 |
| `--primary` | `#FDC800` 黄 | 略降饱和的黄 | **只做** 主 CTA / focus ring / 进度条 / 「当前激活」徽章 |
| `--link` | 沉稳蓝 | 亮蓝 | 文字链接、表格行内动作 |
| `--success` / `--warning` / `--destructive` | `#16A34A` / `#D97706` / `#DC2626` | 提亮版 | 状态点、徽章、提醒条；warning 管「要留意 / 快到期」，destructive 管「出事了 / 不可逆」 |
| `--border` / `--input` | 偏暖淡灰 1px | 深灰 | 卡片、输入框、侧栏 / 状态栏分区线 |
| `--border-muted` | 更淡一档 | 更深一档 | 表格行分隔 |

暗色 **独立调校**，不是浅色取反：明暗随系统（`darkMode: "media"`），改 token 两套都要过一遍对比度。规范里的次色紫 `#432DD7` 本项目**未引入**。

## 线条与阴影

沿用 shadcn 默认形态：容器 `border border-border`，卡片 `rounded-xl` 白底细边无阴影，浮层（下拉 / 选择框 / 气泡）`shadow-md`，弹窗 `shadow-lg`。**不用** 2px 边、硬偏移阴影、发光、彩色投影、hover 位移。

## 字体与字号

- Inter（可变字体，随产物打包：`@fontsource-variable/inter`）承担 UI 文本，中文回退 PingFang / 微软雅黑。
- JetBrains Mono（`@fontsource-variable/jetbrains-mono`）**只给机器可读文本**：biz、key、路径、命令、日志行、时间戳。
- 标尺 13 / 15 / 17 / 21 / 27 / 35 → `text-xs / sm / base / lg / xl / 2xl`。正文 15（桌面控制台密度高，不用规范网页版的 17），表格 / 说明 13，页面标题 `text-xl font-semibold`（27）。`text-[11px]` 只用于表头、分组标签这类小字。

## 按钮体系

形态沿用 shadcn 扁平样式，差异只在填色：

| variant | 填色 | 用在 |
|---|---|---|
| `default` | 黄 | 页面主 CTA（开始巡检、保存、批量添加），一屏尽量只放一个 |
| `outline` | 透明 + 1px 边 | 中性次操作（刷新、复制、取消、翻页） |
| `destructive` | 红 | 不可逆（删除、清空、解除退避），必须二次确认 |
| `secondary` | 浅灰 | 与 outline 等价的弱化版，少用 |
| `ghost` | 无边 | 行内图标动作、次级导航、「查看」 |
| `link` | 沉稳蓝（`--link`） | 段落内跳转 |

黄色不做文字色（白底上 ≈ 1.6:1 不可读）：文字链接 / 行内动作用 **沉稳蓝 `text-link`**（参考 Claude 设置页的链接色），主 CTA 与进度条才是黄。

## 组件形态速查

- **Badge**：胶囊，`default` 浅黄底墨字 = 当前激活，其余状态色淡底 + 彩字。
- **Alert**：`warning` 琥珀淡边淡底，`destructive` 红淡边淡底。
- **Progress**：淡黄轨道，指示条黄（「正在进行」）。
- **Switch**：开 = 黄，关 = 灰。
- **Sidebar**：与内容同一片暖白、1px 线分区；选中项中性浅灰圆角块，hover 更淡；品牌块直接用应用图标（`frontend/src/assets/app-icon.png`，与 `src-tauri/icons-v2` 同源，换 logo 时一起换）。
- **StatusBar**：状态点走 success / warning / destructive token，不发光。
- **Toast（sonner）**：`richColors` 按语义配色。

## 原则

1. **色彩承担语义，排版承担叙事**：层级靠字号 / 字重 / 间距，不靠颜色。
2. **可逆与不可逆要看起来就不同**：删除 / 清空 / 解除退避 = `destructive` + 二次确认；刷新 / 复制 / 确认已读 = 中性按钮。
3. **机器可读文本用等宽字体**：能复制进程序 / 终端的都 `font-mono`。
4. **错误与空态文案两句话**：发生了什么 + 下一步怎么办。
5. **一屏一个黄**：主 CTA 与进度条之外不再铺黄，导航选中态用中性灰。

## 改样式的检查清单

- [ ] 新颜色是否走 token（`hsl(var(--x))`），没有 `#hex` / `text-[#…]`
- [ ] 边框 1px（`border-border` / `border-border-muted`），阴影只有 `shadow-sm/md/lg`
- [ ] 字号只用标尺，像素字号只允许 `text-[11px]` 以下的小标签
- [ ] 黄色没有当文字色用
- [ ] 暗色下看一眼（系统切深色）：边框、状态色是否仍可读
