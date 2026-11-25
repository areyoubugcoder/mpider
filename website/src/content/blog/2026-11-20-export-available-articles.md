---
title: "公众号教程有一篇下载失败，MPider 能先导出其余文章吗？"
description: "用缝纫布包教程举例，说明如何先导出已有正文，再核对跳过数量并补齐。"
pubDate: 2026-11-20
draft: false
---

> 本文整理自 2026-11-20 的原稿，操作说明对应 [MPider v2.0.3](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.3)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

账号是自己的，使用风险也由自己承担，导出文章却还要额外付费。我们觉得这种体验不合理，所以 MPider 选择免费开源。核心采集逻辑和凭证处理可以核查，把知情权和选择权交还给用户。

MPider 可以下载公众号文章的正文和图片，也能做公众号监测。这里的监测是「主页识别」。开启后到微信手动打开目标公众号主页，工具会登记公众号和会话，再由你发起文章列表采集和正文下载。

比如你在整理缝纫公众号里的布包教程，已经下载了几篇图文，还有一篇一直下载失败。想先把已保存的教程带到工作台看，可以用当前版本的批量导出。至少有一篇文章存在本地正文，就能导出，缺少正文的条目会自动跳过，并提示跳过数量。

第一步，在「公众号」页开启「主页识别」，在微信手动打开缝纫公众号主页。登记后，按需要同步首页或采集历史文章。在文章库勾选布包教程，下载正文和操作配图。

![文章库里可下载正文，已下载文章提供阅读和导出入口。截图使用演示数据。](../../assets/blog/workbench-articles.png)

文章库里可下载正文，已下载文章提供阅读和导出入口。截图使用演示数据。

第二步，选中这批教程并导出到一个文件夹。一次会得到 Markdown 和 HTML。留意完成时的跳过数量，打开 index.html，核对实际导出了哪些标题。用 manifest.csv 查看文章清单，再试读教程里的裁布图和缝合步骤。

第三步，要补齐那篇失败文章时，到文章库用「未下载正文」筛选查找，里面包含下载失败条目。查看异常原因，处理后再下载。如果出现验证或访问限制，先暂停并按提示处理。补齐正文后，可以重新导出。

出门前把整个导出文件夹一起带走，保留 images 目录，方便离线对照配图做布包。导出目录只包含这次实际导出的内容，核对目录和跳过数量后再决定资料是否够用。

当前客户端适用于 Windows 10/11 x64，需要 WebView2。采集已验证微信 4.1.15.56 x64，首次登记需要已登录、可操作的微信桌面。使用个人微信号可能受到限制或封禁，请避开重要账号，只保存有权处理的内容。

QQ 交流群 https://qm.qq.com/q/s4JavFxA3e

项目源码 https://github.com/areyoubugcoder/mpider

操作视频 https://xhslink.cn/o/8c23uHExcT6

客户端下载 https://github.com/areyoubugcoder/mpider/releases/latest

官网 https://mpider.followto.us

## 公开资料

- [项目说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.3/README.md)

- [Windows 使用说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.3/docs/windows-standalone.md)

- [常见问题](https://github.com/areyoubugcoder/mpider/blob/v2.0.3/docs/faq.md)
