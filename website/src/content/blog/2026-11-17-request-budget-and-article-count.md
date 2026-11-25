---
title: "用 MPider 采集两个公众号，列表预算和文章篇数怎么区分？"
description: "以街区口述史为例，区分跨公众号共享的列表预算、目标文章篇数和正文下载。"
pubDate: 2026-11-17
draft: false
---

> 本文整理自 2026-11-17 的原稿，操作说明对应 [MPider v2.0.1](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.1)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

账号是自己的，风险也要自己承担，导出文章却得额外付费。我们觉得这样的体验不合理，所以 MPider 选择免费开源。用户可以核查核心采集逻辑和凭证如何处理，看清楚再决定是否使用。

MPider 可以下载公众号文章的正文和图片，也能做公众号监测。这里的公众号监测指「主页识别」。开启后在微信里手动打开目标公众号主页，工具会登记公众号和会话。之后可以分别获取文章列表、下载正文。

## 列表预算里的「次」，为什么不能按文章篇数算？

假设你在做街区口述史，想从两个社区公众号里找老街改造前的采访和照片。你在「采集历史文章」里选了追加篇数，又在「限流配置」看到了列表请求预算。这两个数字管的是不同的事。追加篇数是本次想找回多少篇旧文。列表预算记录的是列表请求次数，两个公众号共享近 24 小时的滚动预算。一次列表请求可以带回多篇文章，结果也可能包含已入库的条目。

1. 在「公众号」页开启「主页识别」，在微信依次手动打开两个公众号主页。确认登记成功后，分别在对应公众号行创建「采集历史文章」任务。
2. 到「任务」看历史列表进度。遇到列表任务等待时，可以查看「限流配置」的请求记录和预算，了解程序正在等什么。预算只是工具的本地控制，剩余次数不能用来判断账号还可以安全请求多少次。

![图：MPider 的历史采集任务页分开显示目标篇数、已获取页数和新入库篇数（演示数据）。](../../assets/blog/workbench-tasks.png)

图：MPider 的历史采集任务页分开显示目标篇数、已获取页数和新入库篇数（演示数据）。

3. 已入库的采访可以在「文章库」勾选下载。正文和图片有各自的请求设置，不消耗列表预算。等正文下完，再把选中的采访同时导出为 Markdown 和 HTML，方便离线对照文字和老照片。

做资料整理时，可以先处理已经入库的采访，再回头查看列表任务进度。

当前客户端适用于 Windows 10/11 x64，需要 WebView2。采集已验证微信 4.1.15.56 x64，首次登记需要在可操作的桌面上登录微信。个人微信号可能受到限制或封禁，请避开重要账号，只保存有权处理的内容。

QQ 交流群 https://qm.qq.com/q/s4JavFxA3e

项目源码 https://github.com/areyoubugcoder/mpider

操作视频 https://xhslink.cn/o/8c23uHExcT6

客户端下载 https://github.com/areyoubugcoder/mpider/releases/latest

官网 https://mpider.followto.us

## 公开资料

- [项目说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/README.md)

- [Windows 使用说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/docs/windows-standalone.md)

- [常见问题](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/docs/faq.md)
