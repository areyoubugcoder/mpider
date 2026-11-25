---
title: "MPider 便携版换成安装版，公众号文章和采集进度怎么备份？"
description: "以工作室整理课程公众号为例，区分文章导出与应用数据备份，说明同一台 Windows 电脑上迁移前后的核对步骤。"
pubDate: 2026-11-10
draft: false
---

> 本文整理自 2026-11-10 的原稿，操作说明对应 [MPider v2.0.1](https://github.com/areyoubugcoder/mpider/releases/tag/v2.0.1)。当前功能与环境要求以[最新发行说明](https://github.com/areyoubugcoder/mpider/releases/latest)为准。

摘要：以工作室整理课程公众号为例，区分文章导出与应用数据备份，说明同一台 Windows 电脑上迁移前后的核对步骤。

账号是自己的，风险由自己承担，导出文章却还要额外付费，这种体验不合理。MPider 选择免费开源，公开核心采集逻辑和凭证处理，让用户能核查，把知情权和选择权交还给用户。

MPider 可以做公众号监测，也可以下载公众号文章的正文和图片。这里的公众号监测是「主页识别」：开启后，在已登录的微信里手动打开目标公众号主页，登记公众号和会话。之后再同步首页或采集历史列表，从文章库勾选下载正文和图片，保存在自己的电脑上离线阅读。

![任务页可区分首页同步、历史采集和正文下载，便于核对任务状态（演示数据）](../../assets/blog/workbench-tasks.png)

任务页可区分首页同步、历史采集和正文下载，便于核对任务状态。截图为公开版本的演示界面，具体操作以本文注明的版本和最新发行说明为准。

## 换安装版之前，该留哪些东西？

假设你在工作室用便携版保存了课程公众号的讲义，已经下载了一部分，历史任务也留有进度。准备在同一台电脑换成安装版时，可以按下面的顺序处理。

1. 先导出需要单独保留的讲义。在文章库选中正文已下载的篇目并导出。导出目录有 Markdown、HTML 和已下载的配图，可以先用浏览器打开 HTML，检查几篇常用讲义。
2. 打开「系统设置」，查看当前应用数据目录。暂停任务，再从系统托盘退出 MPider，然后备份这个完整目录。便携版的数据也存放在应用数据目录，通常不在 EXE 旁边。只复制数据库会遗漏正文、图片和受保护的会话文件。
3. 从项目 Release 下载正式安装包，按说明安装。打开安装版后，先核对设置里显示的数据目录，再检查公众号列表、几篇已下载文章和「任务」里的历史进度。保留备份，直到确认这些内容都能找到。

## 之前没跑完的历史任务怎么接着做？

应用重启后，未完成任务保留检查点并暂停。核对进度后，在「任务」里选择继续。需要更新会话时，按提示在微信手动打开目标公众号主页，更新后沿原检查点尝试继续。受保护的会话文件与当前 Windows 用户绑定，换 Windows 用户时不能直接复用。

文章导出适合给讲义留一份可独立阅读的副本。完整应用目录备份用于保留应用数据和进度，应保存在自己控制的位置。

当前正式版 v2.0.1 面向 Windows 10/11 x64，需要 WebView2，采集已验证微信 4.1.15.56 x64。个人微信号可能受到限制或封禁，开源与本地运行都不能保证账号安全。请避免使用重要账号，只保存有权处理的文章。

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
