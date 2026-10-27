# 文章导出

公众号整号、单篇及勾选文章使用同一条导出流程：一次导出同时生成 Markdown 和 HTML，不需要分别选择格式，也不会重新请求微信或下载图片。开始前必须已经下载全部所选正文；用户取消文件夹选择时返回 `null`，不写文件。

选定目录后，新建唯一子目录；根目录包含 `index.md`、可直接用浏览器打开的 `index.html` 和 `manifest.json`。每篇位于 `articles/<文章ID>/`，同时包含 `article.md`、`article.html` 以及已有本地 `images/`。文件名使用数字 ID，避免 Windows 非法标题字符和同名覆盖。写入失败只清理本次暂存目录，全部成功后整体发布，不覆盖用户原有文件。

正文解析沿用由 [wechat-article-parser](https://github.com/huanjuedadehen/wechat-article-parser) 0.0.6 移植的 `article-md`，第三方 MIT 声明保留于 [THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md)。参考项目负责提取文章元数据、Markdown 及图片链接，本项目在其解析结果上实现本地媒体和双格式导出；不新增 Python 解析进程，也不声称已覆盖其最新 main 的所有版式。

HTML 由同一份 Markdown 渲染，保留标题、段落、链接、代码和表格等可读结构，不还原微信原页面排版或交互。它清洗脚本、事件属性、表单和内嵌页面，仅使用已复制到导出目录的图片；未保存的远程图片显示提示，不会在打开 HTML 时自动联网取图。旧库只有 Markdown 时仍可导出，两种格式文字一致；Markdown 内已有远程图片链接保留，第三方 Markdown 阅读器可能自行加载。付费全文、音视频文件、小程序和复杂交互不在导出范围内。

不读取或附带 `original.html`、受保护凭据文件、原始协议响应及协议证据。原文元数据 URL 与 HTML 超链接的目标地址移除已知会话参数（`key`、`pass_ticket`、`uin`、`wxtoken`）及 URL 用户信息。该处理不等于正文脱敏：Markdown 正文、链接显示文字和代码等保留已有内容，仍可能含带身份参数的链接或其他个人信息；分享导出前应自行检查。导出包含所选文章和公众号元数据，不是加密文件。缺图状态保留在清单中，导出不会自动补图。

## IPC 契约

`workbench_export_source { biz }` 与 `workbench_export_articles { articleIds }` 的参数和返回结构保持兼容：取消返回 `null`，成功返回 `{directory,total,with_body,missing_body,partial_images}`，错误返回失败信息。`with_body` 表示有正文的文章数，不按格式翻倍；正文必须齐全，因此成功导出的 `missing_body` 为 0。`partial_images` 为缺图文章数。

`manifest.json` 使用 `format_version: 2`、`formats: ["markdown", "html"]`；每篇原有 `markdown` 相对路径保留，新增 `html` 相对路径。导出前与写入时重新检查正文和删除状态，禁止把未下载或已删除的文章以空壳文件导出。
