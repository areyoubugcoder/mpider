---
title: "MPider 点「下载全部正文」，会把已保存的文章再下一遍吗？"
description: "以补充岩石辨识资料为例，介绍已有正文跳过、按需下载和任务进度核对。"
pubDate: 2026-11-24
draft: false
---

> 本文整理自 2026-11-24 的原稿，操作说明对应 [MPider v2.0.3](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.3)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

账号是自己的，风险也由自己承担，导出文章却还要额外付费。我们觉得这种体验不合理，所以 MPider 选择免费开源。核心采集逻辑和凭证处理可供核查，把知情权和选择权交还给用户。

MPider 可以下载公众号文章的正文和图片，也能做公众号监测。这里的监测是「主页识别」。开启后到微信手动打开目标公众号主页，工具会登记公众号与会话，接着由你发起列表采集和正文下载。

假设你从地质科普公众号保存了几篇岩石辨识图文，后来又采进一批旧文章。想补下后面那批，点「下载全部正文」时，MPider 会跳过已经保存的正文，把需要下载的文章放进正文下载队列。单篇、勾选和整号下载都使用统一的持久化队列。

第一步，在「公众号」页开启「主页识别」，到微信手动打开地质公众号主页。登记后同步首页，或按追加篇数、截止日期采集历史列表。已有文章留在本地，继续按需要收集辨识资料。

第二步，在该公众号行点击「下载全部正文」。到「任务」的正文下载标签查看进度。若只想补几篇，在文章库勾选目标文章后下载即可。已有正文自动跳过，可以先打开已经下载好的图文核对岩石照片和说明。

![公众号列表提供「下载全部正文」入口，登记后可以按公众号发起下载。截图使用演示数据。](../../assets/blog/workbench-sources.png)

公众号列表提供「下载全部正文」入口，登记后可以按公众号发起下载。截图使用演示数据。

第三步，下载结束后，到文章库核对需要的文章，再导出为 Markdown 和 HTML。双击导出文件夹里的 index.html，检查实际导出的标题和配图，保留整份文件夹以便离线翻阅。

遇到验证、拒绝访问或明显异常时先暂停下载，查看记录并按提示处理。整理资料时根据实际需要安排任务，软件的间隔和预算也只是本地控制措施。

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
