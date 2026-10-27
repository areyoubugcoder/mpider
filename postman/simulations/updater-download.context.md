# 更新模拟上下文 — updater-download

2026-10-26，依据 Tauri 官方静态清单契约手写本地模拟，未登录 Postman，未搜索拥有者发布的 mock。

## Purpose

用 GitHub Release 附件的替身验证真实 updater HTTP 客户端、签名与缓存，不连接真实微信或替换应用。接口与测试数据见 `postman/mocks/updater-release/default.js`。

## Execution

```sh
pnpm dlx postman-cli@1.77.0 simulation run postman/simulations/updater-download.sim.yaml --port 4918 --output ndjson
MPIDER_UPDATER_TEST_ENDPOINT=http://127.0.0.1:4918/github cargo test -p mpider-tauri network_simulation --lib -- --ignored
```

以启动日志的 `listening` 地址为准。退出模拟用运行终端 Ctrl+C。请求日志不提交。

## Assumptions

- 观察于 2026-10-26：测试需显式设置模拟端点；测试构建单独允许回环 HTTP，正式配置仍用 HTTPS。
- 观察于 2026-10-26：假包带版本绑定签名，验证可通过，但不能交给系统安装器。实际 Windows/macOS 安装、权限与重启仍需发布前单独验证。
- 模拟不会热重载，改 handler 后必须重启。

## Observations

<!-- begin:observations -->
- 2026-10-26：`network_simulation` 通过，唯一成员 github 收到清单、完整包、损坏包、503、404、非 JSON 以及失败后的再次检查请求，无未调用成员。有效下载通过验签并在新 updater 实例中离线恢复，损坏包被拒绝。
- 2026-10-26 主线整合回归：合并工作台与更新分支后，真实 updater 客户端再次通过同一模拟；github 成员实际收到有效/损坏下载、404、503、非 JSON 和重试请求，无未调用成员。此验证仍不执行安装。
<!-- end:observations -->
