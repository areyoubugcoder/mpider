---
title: "下载的公众号旧文太多，怎么离线按标题找文章？"
description: "以湿地观鸟为例，介绍如何从历史列表勾选下载并用导出目录按标题查找。"
pubDate: 2026-11-15
draft: false
---

> 本文整理自 2026-11-15 的原稿，操作说明对应 [MPider v2.0.1](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.1)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

账号是自己的，使用风险也由自己承担，导出文章却还要另外付费。我们觉得这不合理，所以把 MPider 做成免费开源工具。核心采集逻辑和凭证处理都可以查，怎么用、要不要用，由用户自己决定。

MPider 可以下载公众号文章的正文和图片，也能做公众号监测。这里的监测指「主页识别」。你开启它后，在微信里手动打开公众号主页，MPider 就会登记公众号和会话。文章列表和正文要在登记后分别获取。

## 下载之后，怎么从一堆旧文里找出那一篇？

![文章库用于查看文章列表、下载正文与导出离线文件（演示数据）](../../assets/blog/workbench-articles.png)

文章库用于查看文章列表、下载正文与导出离线文件。截图为公开版本的演示界面，具体操作以本文注明的版本和最新发行说明为准。

比如你在准备观鸟活动，想把一个自然教育公众号发布过的常见鸟类辨识文章存到电脑。到了户外网络不稳定，打开归档目录就能按标题找图文，不必记住文章当时发在哪一天。

1. 在 MPider 的「公众号」页开启「主页识别」，到微信手动打开目标公众号主页。登记成功后，在该公众号一行点「采集历史文章」，按追加篇数或截止日期取回列表。
2. 在「文章库」筛选这个公众号，勾选需要的辨识文章并下载正文。等目标正文下完，再导出勾选的文章。一次导出会生成 Markdown、HTML 和已保存的图片。
3. 双击导出文件夹里的 index.html。目录按发布时间倒序排列，输入标题里的词，例如「白鹭」，然后点开匹配的文章查看正文和已下载的照片。目录搜的是标题，若物种只出现在正文里，可以打开可能相关的文章，再用浏览器的页内查找。

想带到另一台电脑看，把整个导出文件夹一起复制，保留图片目录。出门前可以先断网打开目录和几篇常用文章，确认要看的照片已保存。

当前发行版适用于 Windows 10/11 x64，需要 WebView2。采集已验证微信 4.1.15.56 x64，首次登记时微信须已登录，桌面须能操作。个人微信号可能受到限制或封禁，请避开重要账号，只保存有权处理的文章。

QQ 交流群 https://qm.qq.com/q/s4JavFxA3e

项目源码 https://github.com/areyoubugcoder/mpider

操作视频 https://xhslink.cn/o/8c23uHExcT6

客户端下载 https://github.com/areyoubugcoder/mpider/releases/latest

官网 https://mpider.followto.us

## 公开资料

- [项目说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/README.md)

- [Windows 使用说明](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/docs/windows-standalone.md)

- [常见问题](https://github.com/areyoubugcoder/mpider/blob/v2.0.1/docs/faq.md)
