---
title: "分享公众号离线阅读材料前，用 MPider 导出后还要检查什么？"
description: "以获准传阅的诗歌读书会材料为例，说明导出内容、配图和链接的核对步骤。"
pubDate: 2026-11-21
draft: false
---

> 本文整理自 2026-11-21 的原稿，操作说明对应 [MPider v2.0.3](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.3)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

账号是自己的，使用风险也由自己承担，导出文章却还要额外付费。我们觉得这种体验不合理，所以 MPider 选择免费开源。核心采集逻辑和凭证处理可以核查，把知情权和选择权交还给用户。

MPider 可以下载公众号文章的正文和图片，也能做公众号监测。这里的监测是「主页识别」。开启后到微信手动打开目标公众号主页，工具会登记公众号和会话，再由你发起文章列表采集和正文下载。

假设你准备主持一场诗歌读书会，作者允许小组传阅几篇公众号解读文章。你想提前给参加者准备一份离线材料。用 MPider 可以把勾选的文章同时导出为 Markdown 和 HTML，传阅前还需要核对正文、图片和链接里的内容。

第一步，在「公众号」页开启「主页识别」，到微信手动打开目标公众号主页。登记后按需要同步首页或采集历史列表，再在文章库挑出获准传阅的几篇解读，下载正文与配图。

![从文章库勾选需要的文章，下载正文后阅读和导出。截图使用演示数据。](../../assets/blog/workbench-articles.png)

从文章库勾选需要的文章，下载正文后阅读和导出。截图使用演示数据。

第二步，勾选这些文章并导出。打开生成的 index.html，逐篇查看实际正文和图片，再核对 manifest.csv 中的标题、发布时间和原文链接。若有缺正文被跳过的条目，先确认材料是否够用。

第三步，检查准备分享的文字、图片和超链接，看看是否包含需要去除的联系方式、私人备注或其他敏感信息。MPider 的普通导出会移除已知会话参数，正文文字仍要自己检查。公开文档也明确提醒，数据库、完整应用数据目录和会话文件需要妥善保管，分享阅读材料时使用核对过的文章导出文件夹。

确认内容和传阅权限后，保留整个导出文件夹及 images 目录，参加者就能打开 index.html 查阅图文。需要把个人批注加进 Markdown 时，再检查一遍准备发出的那份副本，方便读书会围绕同一批材料讨论。

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
