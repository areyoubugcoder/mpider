"""汇总 CI：应运行的任务必须成功，跳过、失败和取消不能冒充成功。"""

import json
import os
import sys


def result_errors(needs):
    changes = needs.get("changes", {})
    if changes.get("result") != "success":
        return ["路径判断未成功，不能确认检查范围"]
    errors = []
    for name in ("frontend", "rust", "native", "ci"):
        enabled = changes.get("outputs", {}).get(name)
        result = needs.get(name, {}).get("result")
        if enabled not in ("true", "false"):
            errors.append(f"{name}: 缺少有效的路径判断结果")
        elif enabled == "true" and result != "success":
            errors.append(f"{name}: 应执行的检查未通过（{result}）")
        elif enabled == "false" and result not in ("success", "skipped"):
            errors.append(f"{name}: 检查异常（{result}）")
    return errors


if __name__ == "__main__":
    errors = result_errors(json.loads(os.environ["CI_NEEDS"]))
    if errors:
        print("\n".join(errors), file=sys.stderr)
        sys.exit(1)
    print("所需检查均已通过，无关任务已跳过")
