---
title: "MPider 文章库一页只显示 10 条，公众号旧文只采到了 10 篇吗？"
description: "用剧团复排举例，区分文章库每页条数、历史采集范围和正文下载。"
pubDate: 2026-11-18
draft: false
---

> 本文整理自 2026-11-18 的原稿，操作说明对应 [MPider v2.0.1](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.1)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

账号是自己的，使用风险也由自己承担，导出文章却还要另付费。我们觉得这样的体验不合理，所以 MPider 选择免费开源。核心采集逻辑和凭证处理都可以核查，看清楚再决定怎么用。

MPider 可以下载公众号文章的正文和图片，也可以做公众号监测。这里的监测是「主页识别」。开启后，在微信手动打开目标公众号主页，MPider 会登记公众号和会话。文章列表和正文由你分别发起任务。

假设你在筹备社区剧团的复排，想从剧团公众号找以前发过的排练笔记。采集历史后，文章库一开始只显示一屏。先看右上方的文章总数和底部分页。默认每页显示 10 条，在分页处可以切换为 20、50 或 100 条。这项设置只改变一页能看到多少条。要扩大采集范围，需要回到该公众号的历史采集任务里设置追加篇数或截止日期。

操作时先在「公众号」页开启「主页识别」，到微信手动打开剧团公众号主页。登记后，在对应公众号行点击「采集历史文章」，按你要回顾的排练阶段选择范围。接着到「文章库」筛选该公众号，翻页查看，勾选需要的排练笔记，下载正文和配图。

下载后可离线阅读，也可以把选中的文章同时导出为 Markdown 和 HTML。复排前先核对自己勾选的文章和下载进度，再打开导出目录，把需要对照的排练笔记放在手边。

![MPider 文章库界面，可按公众号筛选并查看文章总数。截图使用演示数据。](../../assets/blog/workbench-articles.png)

MPider 文章库界面，可按公众号筛选并查看文章总数。截图使用演示数据。

当前客户端适用于 Windows 10/11 x64，需要 WebView2。采集已验证微信 4.1.15.56 x64，首次登记时请在已登录、可操作的微信桌面打开公众号主页。使用个人微信号可能受到限制或封禁，请避开重要账号，只保存有权处理的内容。

QQ 交流群 https://qm.qq.com/q/s4JavFxA3e

项目源码 https://github.com/areyoubugcoder/mpider

操作视频 https://xhslink.cn/o/8c23uHExcT6

客户端下载 https://github.com/areyoubugcoder/mpider/releases/latest

官网 https://mpider.followto.us

## 公开资料

- [项目说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/README.md)

- [Windows 使用说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/docs/windows-standalone.md)

- [常见问题](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/docs/faq.md)
