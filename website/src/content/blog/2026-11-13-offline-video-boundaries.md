---
title: "公众号教程带视频，下载到电脑后还能离线看哪些内容？"
description: "用自行车保养教程说明离线导出的图文范围，如何检查缺图，以及哪些步骤要回原文看视频。"
pubDate: 2026-11-13
draft: false
---

> 本文整理自 2026-11-13 的原稿，操作说明对应 [MPider v2.0.1](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.1)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

摘要：用自行车保养教程说明离线导出的图文范围，如何检查缺图，以及哪些步骤要回原文看视频。

账号是自己的，风险由自己承担，导出文章还要额外付费，这样的体验不合理。MPider 因此选择免费开源，把核心采集逻辑和凭证处理公开给用户核查，把知情权和选择权交还给用户。

MPider 提供公众号监测和公众号文章下载。正文和已下载的图片可以留在电脑上离线阅读，也能同时导出 Markdown 和 HTML。文章里的音视频则要回原文观看或收听。

![文章库用于查看文章列表、下载正文与导出离线文件（演示数据）](../../assets/blog/workbench-articles.png)

文章库用于查看文章列表、下载正文与导出离线文件。截图为公开版本的演示界面，具体操作以本文注明的版本和最新发行说明为准。

## 自行车保养教程能留下什么？

假设你想把几篇自行车清洁教程带到没有网络的车库。图文写明了工具清单和清洁顺序，原文还有一段动作演示。你可以先保存图文，在有网时看完需要的视频，再用离线副本核对顺序和配图。

1. 在 MPider 的「公众号」页开启「主页识别」，到已登录的微信里手动打开目标公众号主页，登记公众号和会话。这里的公众号监测用于完成登记。
2. 回到公众号列表，点「同步最新文章」取首页。如果教程发得更早，按追加篇数或截止日期采集历史列表。
3. 到文章库勾选所需教程，下载正文和图片。等所选正文都下载好，再执行勾选导出并选择保存目录。
4. 打开导出目录里的 index.html，进入教程检查正文和步骤图。碰到视频或音频提示时，记下对应篇目和段落，联网时从保留的原文入口查看。

## 有正文，为什么有的步骤图还是空的？

导出使用已经下载到本机的图片。没保存的图片会显示提示，打开 HTML 时也不会自动补图。出门前可以对照文章和导出目录里的 README.txt 核对缺图情况。若重要动作只出现在视频里，先回原文看完，再用自己的笔记记录要点。

当前正式版 v2.0.1 面向 Windows 10/11 x64，需要 WebView2，采集已验证微信 4.1.15.56 x64。使用个人微信号可能遇到限制或封禁，开源和本地运行无法保证账号安全。请避免使用重要账号，只保存有权处理的内容。

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
