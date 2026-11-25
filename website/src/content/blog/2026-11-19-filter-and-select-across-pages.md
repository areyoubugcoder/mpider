---
title: "MPider v2.0.2 怎么筛出已下载的扦插教程，再跨页导出？"
description: "按标题关键词和下载状态选出已下载图文，再核对跨页选择和离线文件。"
pubDate: 2026-11-19
draft: false
---

> 本文整理自 2026-11-19 的原稿，操作说明对应 [MPider v2.0.2](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.2)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

账号是自己的，风险也由自己承担，导出文章却要额外付费。我们觉得这样的体验不合理，所以 MPider 选择免费开源。核心采集逻辑和凭证处理都可以核查，把知情权和选择权交还给用户。

MPider 可以下载公众号文章的正文和图片，也能做公众号监测。这里的监测是「主页识别」。开启后到微信手动打开目标公众号主页，工具会登记公众号和会话，然后由你发起列表采集和正文下载。

比如你从一个园艺公众号采集了一批教程，只想带走已经下载好的「扦插」文章，周末在阳台边操作边看。v2.0.2 已加入标题关键词和下载状态筛选，可以先缩小范围，再选择跨页的匹配结果。

第一步，在「公众号」页开启「主页识别」，到微信手动打开园艺公众号主页。登记后点该号的「采集历史文章」，按追加篇数或截止日期获取旧文列表。已有文章的用户可以直接进入文章库。

![在公众号页开启主页识别并登记目标公众号，再从该号发起文章采集。截图使用演示数据。](../../assets/blog/workbench-sources.png)

在公众号页开启主页识别并登记目标公众号，再从该号发起文章采集。截图使用演示数据。

第二步，在文章库选中这个公众号，输入标题关键词「扦插」，将下载状态筛选为「已下载正文」。核对列表后，点击「选择全部筛选结果」，即可选中各分页里匹配的文章。表头复选框用于选择本页。修改公众号、关键词或下载状态会清空选择，改完条件后要重新核对。

第三步，导出选中的文章，同时得到 Markdown、HTML 和已保存的图片。打开导出目录里的 index.html，核对教程标题和操作配图，出门或断网前先试读一遍。

如果还有想要的教程没下载，可以切到「未下载正文」筛选，里面也包含下载失败的条目。查看失败原因、处理后再下载需要的文章，之后重新筛选和导出。遇到验证或访问限制时先暂停，按提示处理。

当前客户端适用于 Windows 10/11 x64，需要 WebView2。采集已验证微信 4.1.15.56 x64，首次登记需要已登录且可以操作的微信桌面。个人微信号可能受到限制或封禁，请避开重要账号，只保存有权处理的文章。

QQ 交流群 https://qm.qq.com/q/s4JavFxA3e

项目源码 https://github.com/areyoubugcoder/mpider

操作视频 https://xhslink.cn/o/8c23uHExcT6

客户端下载 https://github.com/areyoubugcoder/mpider/releases/latest

官网 https://mpider.followto.us

## 公开资料

- [项目说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.2/README.md)

- [Windows 使用说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.2/docs/windows-standalone.md)

- [常见问题](https://github.com/areyoubugcoder/mpider/blob/v2.0.2/docs/faq.md)
