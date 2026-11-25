---
title: "公众号旧文还没采完，能先把本周的活动公告下载下来吗？"
description: "用社区图书馆活动资料举例，说明如何先同步公众号首页、下载最新公告，并保留旧文采集的进度。"
pubDate: 2026-11-12
draft: false
---

> 本文整理自 2026-11-12 的原稿，操作说明对应 [MPider v2.0.1](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.1)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

摘要：用社区图书馆活动资料举例，说明如何先同步公众号首页、下载最新公告，并保留旧文采集的进度。

账号是自己的，风险由自己承担，导出文章却还要额外付费，这样的体验不合理。MPider 选择免费开源，公开核心采集逻辑和凭证处理，让用户可以核查，把知情权和选择权交还给用户。

MPider 可以做公众号监测、下载公众号文章。正文和图片可以保存在自己的电脑里，离线阅读，也可以同时导出 Markdown 和 HTML。

![任务页可区分首页同步、历史采集和正文下载，便于核对任务状态（演示数据）](../../assets/blog/workbench-tasks.png)

任务页可区分首页同步、历史采集和正文下载，便于核对任务状态。截图为公开版本的演示界面，具体操作以本文注明的版本和最新发行说明为准。

## 旧文归档没做完，先把眼下要看的公告存好

假设你在帮社区图书馆整理自己的公众号资料，上个月的活动回顾还没采完，但现在要先核对周末读书会的新公告和海报。MPider 把「同步最新文章」和「采集历史文章」分开管理。同步首页会去重入库，并保留历史采集的检查点。

1. 在「公众号」页开启「主页识别」，在已登录的微信里手动打开目标公众号主页，等名称和会话登记。这里的公众号监测用于登记，完成后再发起采集。
2. 在「任务」里核对历史采集进度。想把时间留给眼下的公告，可以先暂停历史任务。在对应公众号行点「同步最新文章」，取回首页列表。
3. 到文章库按公众号筛选，找到周末的公告，勾选下载正文与图片。打开已下载的文章，核对时间、地点和海报。需要拿去离线核对时，再导出这些篇目的 Markdown 和 HTML。
4. 公告整理好后，回到「任务」里的历史采集继续。若提示需要更新会话，按提示手动重开目标公众号主页，再沿原检查点尝试继续。

## 首页没有这篇公告怎么办？

首页同步的范围是公众号首页。公告发得更早时，可以按需要继续历史采集，采集目标可按追加篇数或截止日期设定。按公众号定时增量采集仍是计划方向，当前可以按自己需要手动同步。

正式版 v2.0.1 面向 Windows 10/11 x64，需要 WebView2，采集已验证微信 4.1.15.56 x64。使用个人微信号仍可能受到限制或封禁，开源和本地运行无法保证账号安全。请避免使用重要账号，仅保存有权处理的文章。

## 项目入口

QQ 交流群：https://qm.qq.com/q/s4JavFxA3e
项目源码：https://github.com/areyoubugcoder/mpider
操作视频：https://xhslink.cn/o/8c23uHExcT6
客户端下载：https://github.com/areyoubugcoder/mpider/releases/latest
官网：https://mpider.followto.us

## 公开资料

- [项目说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/README.md)

- [Windows 使用说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/docs/windows-standalone.md)

- [常见问题](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/docs/faq.md)
