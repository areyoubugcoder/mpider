"""仅让影响应用或构建的改动运行完整 CI；纯公开资料改动跳过构建。"""

import json
import os
from pathlib import Path
import subprocess

EMBEDDED_GUIDES = {
    "docs/faq.md",
    "docs/laboratory.md",
    "docs/request-policy.md",
}


def requires_build(paths):
    for path in paths:
        if path in EMBEDDED_GUIDES:
            return True
        if path.startswith(("docs/", ".github/ISSUE_TEMPLATE/")):
            continue
        if "/" not in path and (path.endswith(".md") or path == "LICENSE"):
            continue
        if path in {".github/release-notes.md", ".github/pull_request_template.md"}:
            continue
        return True
    return False


def changed_paths(event):
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
    result = subprocess.check_output(command)
    return [os.fsdecode(path) for path in result.split(b"\0") if path]


if __name__ == "__main__":
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    try:
        paths = changed_paths(event)
        build = requires_build(paths)
    except subprocess.CalledProcessError:
        # 强制更新等情况下旧对象可能不在本地，保守执行检查。
        paths = []
        build = True
    value = str(build).lower()
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write("build=" + value + "\n")
    print(json.dumps({"build": build, "changed_files": len(paths)}))
