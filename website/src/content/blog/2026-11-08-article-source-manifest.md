---
title: "公众号文章存到电脑后，怎样保留标题、日期和原文链接清单？"
description: "以整理摄影教程为例，用 MPider 下载正文和图片，再利用导出的 CSV 清单核对出处并给自己的笔记建立索引。"
pubDate: 2026-11-08
draft: false
---

> 本文整理自 2026-11-08 的原稿，操作说明对应 [MPider v2.0.1](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.1)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

摘要：以整理摄影教程为例，用 MPider 下载正文和图片，再利用导出的 CSV 清单核对出处并给自己的笔记建立索引。

账号是自己的，风险由自己承担，导出文章还要额外付费，这种体验不合理。MPider 选择免费开源，公开核心采集逻辑和凭证处理，让用户有机会核查，把知情权和选择权交还给用户。

MPider 可以做公众号监测，也能下载公众号文章的正文和图片。下载后的内容能离线阅读，同时导出 Markdown、HTML 和文章清单。今天用整理摄影教程的场景，讲这份清单怎么用。

![文章库用于查看文章列表、下载正文与导出离线文件（演示数据）](../../assets/blog/workbench-articles.png)

文章库用于查看文章列表、下载正文与导出离线文件。截图为公开版本的演示界面，具体操作以本文注明的版本和最新发行说明为准。

## 摄影笔记旁边留一份来源清单

假设你在学习布光，从同一个摄影公众号保存了几篇教程，还另外写了自己的练习笔记。以后回看笔记时，想知道参考的是哪篇文章、什么时候发布、原文在哪里，就可以查 MPider 导出目录里的 manifest.csv。

清单包含 sn、标题、发布时间、原文链接、类型和正文状态。用 Excel、WPS 或 Numbers 打开，就能按行查看这批导出的文章。文章发布时间按北京时间显示。

## 先保存教程，再生成清单

1. 在 MPider「公众号」页开启「主页识别」，再到已登录的微信里手动打开目标公众号主页，登记公众号和会话。这就是这里说的公众号监测操作。
2. 在对应公众号行点「同步最新文章」获取首页。需要更早的教程时，用「采集历史文章」选择追加篇数或截止日期。历史任务支持检查点续采，需要更新会话时按提示重新打开目标主页。
3. 到文章库按公众号筛选，勾选这次需要的教程，下载正文和图片。等所选正文下载完成，再执行勾选导出并选择保存目录。
4. 打开导出目录里的 manifest.csv，对照标题核对篇目，再从 index.html 进入正文检查配图。Markdown、HTML、图片和 CSV 清单会一起写出。

## 把清单接到自己的练习笔记

你可以在清单副本里手动加上「练习主题」和「笔记位置」两列。例如，将某篇文章对应到自己写的“单灯侧光练习”笔记。这样，以后先查练习主题，就能找到文章标题和原文链接，再回到本地正文核对方法。

这两列由你在表格软件中填写。MPider 提供文章导出和原有清单字段，自己的理解、练习记录与笔记关联由自己补充。

保留原始导出目录，另存清单副本做整理，会更方便以后核对。导出清单中的「成功」表示正文成功导出，配图情况还要查看文章和 README.txt 中的缺图提示。

当前正式版 v2.0.1 要求 Windows 10/11 x64、WebView2，已验证的微信版本是 4.1.15.56 x64。登记公众号与更新会话时需要可交互的微信桌面。开源、本地运行或低频请求都无法保证账号安全，请评估限制与封禁风险，避免使用重要账号，只保存你有权处理的文章。

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
