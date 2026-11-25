---
title: "同一批公众号文章再次导出，会覆盖我做过批注的旧文件吗？"
description: "用花艺课归档举例，说明 MPider 怎样处理同名导出目录，以及如何核对两批文件。"
pubDate: 2026-11-14
draft: false
---

> 本文整理自 2026-11-14 的原稿，操作说明对应 [MPider v2.0.1](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.1)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

摘要：用花艺课归档举例，说明 MPider 怎样处理同名导出目录，以及如何核对两批文件。

账号是自己的，风险自己承担，导出文章却还要额外付费，这种体验不合理。MPider 选择免费开源，让核心采集逻辑和凭证处理可以核查，把知情权和选择权交还给用户。

MPider 可以做公众号监测，也可以下载公众号文章的正文和图片，留在电脑上离线阅读，并同时导出 Markdown 和 HTML。

![文章库用于查看文章列表、下载正文与导出离线文件（演示数据）](../../assets/blog/workbench-articles.png)

文章库用于查看文章列表、下载正文与导出离线文件。截图为公开版本的演示界面，具体操作以本文注明的版本和最新发行说明为准。

假设你整理了一批花艺课的公众号教程，已经在第一批导出的 Markdown 里写了练习心得。隔了一周，你又添了几篇花材处理的教程，想重新导出一份完整资料。选回同一个保存位置时，如果目标目录同名，MPider 会给新目录加后缀，旧目录和里面的批注会保留。

1. 首次登记时，在 MPider 的「公众号」页开启「主页识别」，在已登录的微信里手动打开目标公众号主页，等待公众号和会话登记。随后在对应公众号行手动同步首页，或按篇数、截止日期采集历史。
2. 在文章库勾选这次要带进资料包的教程，下载所需正文和图片。已有正文会跳过。等所选正文都准备好，再执行勾选导出。
3. 选择原来的保存位置，等导出完成后打开新文件夹里的 index.html，核对新增教程。新目录包含本次所选文章的 Markdown、HTML 和已下载的图片，可以离线翻阅。
4. 要把练习心得带到新资料包，可以对照标题和原文链接，手动把批注补进相应文件。重新导出用的是 MPider 里保存的正文，写在旧导出目录里的笔记需要自己整理。

如果想明确区分两次归档，也可以按日期选不同的保存位置。这样下次打开文件夹就能看出哪个是课堂笔记，哪个是新下载的阅读副本。

正式发行版 v2.0.1 面向 Windows 10/11 x64，需要 WebView2。采集已验证微信 4.1.15.56 x64，首次登记需要可交互的微信桌面。使用个人微信号可能遇到限制或封禁，开源无法保证账号安全。请避免使用重要账号，只保存有权处理的内容。

QQ 交流群：https://qm.qq.com/q/s4JavFxA3e
项目源码：https://github.com/areyoubugcoder/mpider
操作视频：https://xhslink.cn/o/8c23uHExcT6
客户端下载：https://github.com/areyoubugcoder/mpider/releases/latest
官网：https://mpider.followto.us

## 公开资料

- [项目说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/README.md)

- [Windows 使用说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/docs/windows-standalone.md)

- [常见问题](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/docs/faq.md)
