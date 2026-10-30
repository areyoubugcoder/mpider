# 第三方代码归属（THIRD-PARTY NOTICES）

本仓库包含由第三方开源项目移植而来的代码，以下声明按其许可证要求保留。

## crates/article-md

`crates/article-md` 的文章 HTML 解析逻辑移植自 Python 开源项目
[**wechat-article-parser**](https://github.com/huanjuedadehen/wechat-article-parser) 0.0.6（PyPI，MIT 许可）。版式探测顺序、字段语义与正文清洗规则
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

## Windows 便携包运行环境

本仓库不存储第三方二进制；组装发行包时按 `tools/windows-runtime.lock.json` 下载并校验固定 SHA-256。依赖原包的许可证和元数据原样保留，不能仅复制 EXE 或删去许可文件。

| 组件 | 固定版本 | 许可文件在发行包内的位置 | 上游 |
| --- | --- | --- | --- |
| CPython（含其附带组件声明） | 3.12.10 x64 embed | `runtime/LICENSE.txt` | [Python 官方发行目录](https://www.python.org/ftp/python/3.12.10/) |
| Frida Python bindings | 17.23.3 | `runtime/session-deps/frida-17.23.3.dist-info/licenses/COPYING`（wxWindows Library Licence 3.1），`licenses/Frida-COPYING.LIB`（其引用的 GNU Library GPL v2 全文） | [Frida](https://github.com/frida/frida/tree/17.23.3)、[PyPI](https://pypi.org/project/frida/17.23.3/) |
| cryptography（含 wheel 附带声明） | 50.0.2 | `runtime/session-deps/cryptography-50.0.2.dist-info/licenses/`（完整 LICENSE、Apache 与 BSD 文本） | [PyPI](https://pypi.org/project/cryptography/50.0.2/) |
| cffi | 2.0.0 | `runtime/session-deps/cffi-2.0.0.dist-info/licenses/`（LICENSE、AUTHORS） | [PyPI](https://pypi.org/project/cffi/2.0.0/) |
| pycparser | 2.23 | `runtime/session-deps/pycparser-2.23.dist-info/LICENSE` | [PyPI](https://pypi.org/project/pycparser/2.23/) |

应用未修改这些运行环境组件。Frida 的 wxWindows 例外文本引用的 LGPL 全文取自 [GNU 官方许可档案](https://www.gnu.org/licenses/old-licenses/lgpl-2.0.txt)，原样保存在 `tools/licenses/Frida-COPYING.LIB` 并固定摘要，组装时不再联网获取。微信与 Microsoft WebView2 不在便携包中再分发。

## Rust、界面与字体依赖

发行时 `tools/collect-third-party-licenses.py` 从锁定的 Cargo 解析图和 pnpm 已安装依赖复制完整 LICENSE、NOTICE、COPYRIGHT、OFL 等文件到 `licenses/dependencies/`。其中 `index.json` 保留名称、版本、原作者、许可和公开源码来源，不包含构建机路径。Inter 与 JetBrains Mono 的 OFL 全文随前端依赖保留。

部分上游发布包遗漏独立许可文件，补充来源和 SHA-256 固定于 `tools/third-party-license-sources.json`，缺少声明且没有已审阅补充来源时构建失败。fxhash/mac 选择上游声明允许的 Apache-2.0；selectors 附 MPL-2.0 全文及原版源码；dlv 保留 MIT 条款、原作者元数据及原安装包源码。原始版权声明与标准许可模板分别保留，不用模板占位符替代作者信息。所有 MPL Rust 依赖的原版 crate 源码另附于 `licenses/dependencies/sources/`，亦可按清单公开地址免费获取。
