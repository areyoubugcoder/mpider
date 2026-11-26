---
title: "MPider 文章库里找不到旧教程，怎样检查已删除记录并恢复？"
description: "用修补衣物的教程归档举例，先核对公众号和筛选条件，再恢复 MPider 内删除的文章记录，按需下载并导出。"
pubDate: 2026-11-26
draft: false
---

账号是自己的，风险也由自己承担，导出文章却还要额外付费。MPider 因此选择免费开源，让核心采集逻辑和凭证处理可以核查，把知情权和选择权交还给用户。

MPider 能下载公众号文章的正文和图片，也支持公众号监测。这里用到的监测叫「主页识别」：开启后，在微信里手动打开目标公众号主页，登记公众号和会话，再自行发起列表采集和正文下载。按公众号定时增量采集仍在计划中。

找一篇曾收藏的缝补教程时，可以用「袖口」做标题关键词。先确定文章记录是否还在，再看正文是否已保存。下面按这两个结果决定恢复、下载或补采。

![文章库的公众号筛选和文章行操作](../../assets/blog/workbench-articles.png)

文章库公开演示图，使用虚构数据。图中展示了文章列表，删除筛选和恢复按钮请以 v3.0.0 的实际界面为准。

## 怎样找出已删除记录并恢复

1. 在「文章库」选对公众号，输入标题里记得的几个字，把「下载状态」设为「全部状态」，将「包含已删除文章」从「否」改为「是」。
2. 找到带「已删除」标记的文章，在该行「操作」列点击「恢复」。MPider 提示「文章已恢复」，该条目的「已删除」标记消失。把「包含已删除文章」改回「否」，确认文章仍在列表。若提示文章不存在或已恢复，刷新列表后再核对状态。
3. 行内有「阅读」就先打开，检查步骤文字和图片。若只显示「下载」，说明没有可读的本地正文。原文仍可访问时可重新下载；遇到验证、访问限制或原文失效，先查看任务提示。若提示列表会话失效，按提示更新会话后再继续列表任务。
4. 正文齐全后，在该行「更多」里选「导出文章」。一次导出会写出 Markdown、HTML 和已保存的图片。打开导出目录的 `index.html`，进入目标文章，核对文字和步骤图。批量导出显示跳过数量时，表示所选范围中有记录缺少正文。可先补下载，再导出。

**恢复范围**：文章库的「恢复」只恢复 MPider 内标记删除的记录。原先保存的正文和文件会保留，恢复操作不会从微信补下载文件，也无法找回作者在公众号上删除的原文。

[公开的 v3.0.0 回归测试](https://github.com/areyoubugcoder/mpider/blob/v3.0.0/crates/core/src/workbench/tests.rs#L1308)覆盖了这个区别：测试删除两条记录、恢复其中一条，再检查该条正文保持原样、另一条仍为已删除。图文是否齐全仍要在自己的导出文件里核对。

## 如果连已删除记录也找不到

1. 先把公众号切换为「全部公众号」，缩短或清空标题关键词，并确认「下载状态」为「全部状态」。找不到记录不等于正文未下载：没有正文的记录仍可显示在列表。
2. 如果本地记录确实不存在，先在「公众号」页开启「主页识别」，到已登录的微信手动打开目标主页，等名称和会话登记完成。
3. 回到该公众号所在行。近期文章用「同步最新文章」获取首页，旧文用「采集历史文章」按追加篇数或截止日期采集。列表入库后再到文章库找标题，选中需要的文章下载正文。

## 适用环境

本文于 2026 年 11 月 26 日对照 [MPider v3.0.0 发行说明](https://github.com/areyoubugcoder/mpider/releases/tag/v3.0.0)及对应源码编写。该版本面向 Windows 10/11 x64，需要 WebView2；项目验证的微信版本为 4.1.15.56 x64。首次登记或更新会话需要已登录、解锁且可交互的微信桌面。个人微信号可能被限制或封禁，请避开重要账号，只处理有权保存的内容。

## 下载和交流

QQ 交流群 [https://qm.qq.com/q/s4JavFxA3e](https://qm.qq.com/q/s4JavFxA3e)

项目源码 [https://github.com/areyoubugcoder/mpider](https://github.com/areyoubugcoder/mpider)

操作视频 [https://xhslink.cn/o/8c23uHExcT6](https://xhslink.cn/o/8c23uHExcT6)

客户端下载 [https://github.com/areyoubugcoder/mpider/releases/latest](https://github.com/areyoubugcoder/mpider/releases/latest)

官网 [https://mpider.followto.us](https://mpider.followto.us)

## 公开资料

- [MPider v3.0.0 项目说明](https://github.com/areyoubugcoder/mpider/blob/v3.0.0/README.md)
- [文章导出说明](https://github.com/areyoubugcoder/mpider/blob/v3.0.0/docs/article-export.md)
- [常见问题](https://github.com/areyoubugcoder/mpider/blob/v3.0.0/docs/faq.md)
- [知乎原文](https://zhuanlan.zhihu.com/p/2109225810066514872)
