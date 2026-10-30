## MPider v1.0.2

- **HTML 保留正文排版**：从已缓存的原始 HTML 提取正文，保留字号、颜色、对齐、间距和图片样式；没有 HTML 的旧库继续使用 Markdown 导出。
- **导出目录更易浏览**：直接打开 `index.html` 搜索标题，文章按「序号_标题」命名；HTML 与 Markdown 共用去重图片，附带 CSV 清单与使用说明，不生成文章 ZIP。
- 兼容跨电脑复制数据库后缺少原归档目录的情况，修正原图与本地 640px 图片的匹配。
- 导出文章底部新增 Mp2RSS、MPHistory 产品链接，携带 `from=mpider` 来源标识。

Windows x64：推荐使用 `MPider_1.0.2_x64-setup.exe`，也提供中文 MSI 和完整便携 ZIP。

本次验证包括离线回归、新华社 49 篇文章的导出和浏览器排版对照；不包含新的微信真机采集或 Windows 安装升级实测。仅支持已适配的微信 4.1.15.56 x64。

[更新记录](https://github.com/areyoubugcoder/mpider/blob/main/CHANGELOG.md)
