# 第三方代码归属（THIRD-PARTY NOTICES）

本仓库包含由第三方开源项目移植而来的代码，以下声明按其许可证要求保留。

## crates/article-md

`crates/article-md` 的文章 HTML 解析逻辑移植自 Python 开源项目
**wechat-article-parser** 0.0.6（PyPI，MIT 许可）。版式探测顺序、字段语义与正文清洗规则
与其逐一对齐，Rust 实现为重写，不含原项目源码。

原项目许可证全文：

```
MIT License

Copyright (c) 2026 Gang

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
