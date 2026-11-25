---
title: "公众号文章导出 HTML，字号、颜色和配图还能保留吗？"
description: "以书法字帖解读为例，介绍 HTML 排版保留及旧 Markdown 正文的边界。"
pubDate: 2026-11-23
draft: false
---

> 本文整理自 2026-11-23 的原稿，操作说明对应 [MPider v2.0.3](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.3)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

账号是自己的，风险也由自己承担，导出文章却还要额外付费。我们觉得这种体验不合理，所以 MPider 选择免费开源。核心采集逻辑和凭证处理可供核查，把知情权和选择权交还给用户。

MPider 可以下载公众号文章的正文和图片，也能做公众号监测。这里的监测是「主页识别」。开启后到微信手动打开目标公众号主页，工具会登记公众号与会话，接着由你发起列表采集和正文下载。

假设你在学书法，公众号的一篇字帖解读用不同颜色区分笔画要点，还有居中的示例图。把文章导出成 HTML 时，MPider 会优先使用本地缓存的原始 HTML 提取正文，保留段落、字号、颜色、居中、间距、图片宽度以及已保存的本地背景图等排版信息。

第一步，在「公众号」页开启「主页识别」，到微信手动打开书法公众号主页。登记后同步首页或采集历史列表，到文章库挑出字帖解读，下载正文和配图。

![文章库提供正文下载、阅读和导出入口。截图使用演示数据。](../../assets/blog/workbench-articles.png)

文章库提供正文下载、阅读和导出入口。截图使用演示数据。

第二步，导出选中的文章。一次导出会同时生成 Markdown 和 HTML。打开导出文件夹里的 index.html，再点进那篇解读，检查彩色提示、段落和示例图的显示，留作临帖前的离线参考。

如果导出的是早期保存、只有 Markdown 正文的旧文章，工具会使用已有 Markdown，无法从中恢复原来的字号、颜色等排版。检查效果时要把这类旧内容单独考虑。复杂交互和微信专用组件也不在完整还原范围内。

导出只使用已保存在本地的图片，缺图会显示提示。临帖前可以先断网打开 HTML，确认需要对照的字帖图和文字都能查看。移动资料时带上整个导出目录和 images 文件夹，方便在另一台电脑阅读。

当前客户端适用于 Windows 10/11 x64，需要 WebView2。采集已验证微信 4.1.15.56 x64，首次登记需要已登录且可以操作的微信桌面。个人微信号可能受到限制或封禁，请避开重要账号，只保存有权处理的文章。

QQ 交流群 https://qm.qq.com/q/s4JavFxA3e

项目源码 https://github.com/areyoubugcoder/mpider

操作视频 https://xhslink.cn/o/8c23uHExcT6

客户端下载 https://github.com/areyoubugcoder/mpider/releases/latest

官网 https://mpider.followto.us

## 公开资料

- [项目说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.3/README.md)

- [Windows 使用说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.3/docs/windows-standalone.md)

- [常见问题](https://github.com/areyoubugcoder/mpider/blob/v2.0.3/docs/faq.md)
