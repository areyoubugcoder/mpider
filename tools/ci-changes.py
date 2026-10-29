"""按 PR 改动选择检查；共享契约联动验证，未知输入保守运行全部检查。"""

import json
import os
from pathlib import Path
import subprocess

CHECKS = ("frontend", "rust", "native", "ci")

EMBEDDED_GUIDES = {
    "CHANGELOG.md",
    "docs/faq.md",
    "docs/laboratory.md",
    "docs/request-policy.md",
}

NON_BUILD_FILES = {
    ".gitignore",
    ".github/release-notes.md",
    ".github/PULL_REQUEST_TEMPLATE.md",
}

NON_BUILD_PREFIXES = (
    "docs/",
    # 官网由独立 Pages workflow 构建与校验，不触发桌面客户端构建。
    "website/",
    ".github/ISSUE_TEMPLATE/",
    # 当前 CI 不启动这些本地模拟，也不构建移动端。
    "postman/mocks/",
    "postman/simulations/",
    "src-tauri/icons-v2/android/",
    "src-tauri/icons-v2/ios/",
)

CI_FILES = {
    ".github/workflows/pages.yml",
    "tools/ci-changes.py", "tools/test_ci_changes.py",
    "tools/check-ci-results.py", "tools/test_ci_results.py",
}

SHARED_CONTRACT_FILES = {
    "frontend/src/lib/tauri.ts", "frontend/src/lib/workbench.ts",
    "frontend/src/views/types.ts", "crates/core/src/model.rs",
    "crates/core/src/workbench/mod.rs", "crates/core/src/store/workbench.rs",
    "crates/core/src/store/native.rs",
}

NATIVE_TESTS = {"tools/test_native_bridge_filter.mjs", "tools/test_session_agent.mjs"}

FRONTEND_FILES = {
    "vite.config.ts", "tailwind.config.cjs", "config/advertisements.json",
    "tools/release-updater.mjs", "tools/test_release_updater.mjs",
}


def checks_for_paths(paths):
    selected = set()
    for path in paths:
        if path in EMBEDDED_GUIDES:
            selected.add("frontend")
        elif path in NON_BUILD_FILES or path.startswith(NON_BUILD_PREFIXES):
            continue
        elif "/" not in path and (path.endswith(".md") or path == "LICENSE"):
            continue
        elif path in CI_FILES:
            selected.add("ci")
        elif path in SHARED_CONTRACT_FILES or path.startswith(("src-tauri/src/", "crates/core/src/native/")):
            selected.update(("frontend", "rust", "native"))
        elif path in NATIVE_TESTS or (path.startswith("native/") and Path(path).name.startswith("test_")):
            selected.add("native")
        elif path.startswith("native/") or path == "tools/native_bridge_probe.js":
            # 工作进程及指纹被 Rust include_str! 编入，需验证两边。
            selected.update(("rust", "native"))
        elif path in {"Cargo.toml", "src-tauri/tauri.conf.json"}:
            # 前端回归同时核对三处项目版本。
            selected.update(("frontend", "rust"))
        elif path.startswith(("crates/", "src-tauri/")) or path in {"Cargo.lock", "tools/run-windows-test.ps1"}:
            selected.add("rust")
        elif (path.startswith("frontend/") or path in FRONTEND_FILES
              or (path.startswith("tools/test_") and path.endswith((".ts", ".mjs")))):
            selected.add("frontend")
        else:
            # workflow 自身、依赖清单、共享构建配置和未知文件实际运行检查。
            selected.update(CHECKS)
    return {check: check in selected for check in CHECKS}


def changed_paths(event, *, cwd=None):
    if "pull_request" in event:
        pr = event["pull_request"]
        revision = pr["base"]["sha"] + "..." + pr["head"]["sha"]
        command = ["git", "diff", "--name-only", "--no-renames", "-z", revision]
    else:
        before = event.get("before", "")
        if not before or set(before) == {"0"}:
            command = ["git", "ls-files", "-z"]
        else:
            command = ["git", "diff", "--name-only", "--no-renames", "-z", before, event["after"]]
    result = subprocess.check_output(command, cwd=cwd)
    return [os.fsdecode(path) for path in result.split(b"\0") if path]


def checks_for_event(event, event_name):
    if event_name == "workflow_dispatch":
        return dict.fromkeys(CHECKS, True)
    try:
        return checks_for_paths(changed_paths(event))
    except subprocess.CalledProcessError:
        # 旧对象缺失或 Git 比较失败时保守执行，禁止当成无改动跳过。
        return dict.fromkeys(CHECKS, True)


if __name__ == "__main__":
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    checks = checks_for_event(event, os.environ["GITHUB_EVENT_NAME"])
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        for name, enabled in checks.items():
            output.write(f"{name}={str(enabled).lower()}\n")
    print(json.dumps(checks))
