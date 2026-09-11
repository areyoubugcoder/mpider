import {
  Check,
  Crosshair,
  Loader2,
  Minus,
  MousePointerClick,
  RefreshCw,
  X,
} from "lucide-react";
import { useApp } from "@/store";
import { cn } from "@/lib/utils";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import type {
  InterceptProbe,
  SelfCheck,
  SystemStatus,
  TestClick,
} from "@/lib/tauri";

/** 单步状态。info=中性（如系统代理运行时才开，不算失败）。 */
type StepState = "done" | "fail" | "pending" | "checking" | "info";

interface Step {
  key: string;
  label: string;
  sub?: string;
  state: StepState;
}

/** 「种子点位标定」一步的副标题：标定坐标 / 未标定 / 已失效。 */
function markLabel(rpa: SelfCheck): string {
  if (rpa.mark === "ok" && rpa.cached_point) {
    return `已标定 (${rpa.cached_point[0]}, ${rpa.cached_point[1]})`;
  }
  if (rpa.mark === "stale") return "已失效（窗口尺寸/分辨率变了）";
  return "未标定";
}

/**
 * 综合环境自检的各步状态。覆盖：证书 / 系统代理 / 微信客户端 / 文件助手窗口 / 种子点位标定。
 * 前三类前提由证书态与系统状态给；后两类（窗口 / 标定）来自 RPA 自检（Windows）。
 */
function toSteps(
  certInstalled: boolean | null,
  system: SystemStatus | null,
  rpa: SelfCheck | null,
  rpaChecking: boolean,
  probe: InterceptProbe | null,
  probing: boolean,
  testClicking: boolean,
  testResult: TestClick | null,
): Step[] {
  // 证书
  const cert: StepState =
    certInstalled === null ? "checking" : certInstalled ? "done" : "fail";
  // 系统代理：自检时真设一次代理→经 MITM 验证能否拦截→复位。以探针结果为准。
  const proxy: StepState = probing
    ? "checking"
    : probe
      ? probe.proxy_set_ok && probe.intercept_ok
        ? "done"
        : "fail"
      : "pending";
  // 微信客户端
  const wechat: StepState = !system
    ? "checking"
    : system.wechat_running
      ? "done"
      : "fail";
  // 文件助手窗口 / 种子点位标定：来自 RPA 自检
  const rpaLoading = rpaChecking && !rpa;
  const fh: StepState = rpaLoading
    ? "checking"
    : rpa
      ? rpa.fh_found
        ? "done"
        : "fail"
      : "pending";
  const mark: StepState = rpaLoading
    ? "checking"
    : !rpa || !rpa.fh_found
      ? "pending"
      : rpa.mark === "ok"
        ? "done"
        : "fail";
  // 浏览器拉起：由「测试点击」结果驱动的验证步（点击标定点位能否真的拉起微信内置浏览器）。
  // 有副作用（动鼠标 / 开浏览器），不随自动自检跑，需手动点「测试点击」；未测=待验证。
  const browser: StepState = testClicking
    ? "checking"
    : testResult
      ? testResult.ok
        ? "done"
        : "fail"
      : "pending";

  return [
    {
      key: "cert",
      label: "证书",
      sub: certInstalled === false ? "未安装" : undefined,
      state: cert,
    },
    {
      key: "proxy",
      label: "系统代理拦截测试",
      sub: probing
        ? "测试中…"
        : probe
          ? probe.intercept_ok
            ? "测试正常"
            : "测试失败"
          : "待测试",
      state: proxy,
    },
    {
      key: "wechat",
      label: "微信客户端",
      sub: system && !system.wechat_running ? "未启动" : undefined,
      state: wechat,
    },
    { key: "fh", label: "文件助手窗口", state: fh },
    {
      key: "mark",
      label: "种子点位标定",
      sub: rpa ? markLabel(rpa) : undefined,
      state: mark,
    },
    {
      key: "browser",
      label: "浏览器拉起",
      sub: testClicking
        ? "测试中…"
        : testResult
          ? testResult.ok
            ? "已拉起"
            : "未拉起"
          : "点「测试点击」验证",
      state: browser,
    },
  ];
}

/** 步骤节点：彩色圆圈 + 图标。 */
function StepNode({ state }: { state: StepState }) {
  const cls = cn(
    "grid size-8 shrink-0 place-items-center rounded-full transition-colors",
    state === "done" && "bg-emerald-500 text-white",
    state === "fail" && "bg-amber-500 text-white",
    state === "info" && "bg-muted-foreground/25 text-muted-foreground",
    state === "checking" && "bg-muted text-muted-foreground",
    state === "pending" && "border border-dashed border-border bg-muted text-muted-foreground",
  );
  return (
    <span className={cls}>
      {state === "done" && <Check className="size-4" />}
      {state === "fail" && <X className="size-4" />}
      {state === "info" && <Minus className="size-4" />}
      {state === "checking" && <Loader2 className="size-4 animate-spin" />}
      {state === "pending" && <span className="size-1.5 rounded-full bg-current" />}
    </span>
  );
}

/**
 * 启动自检卡：**横线 step 样式**综合呈现抓取前置环境
 * 「证书 → 系统代理 → 微信客户端 → 文件助手窗口 → 种子点位标定」。
 * 程序启动即自动自检（见 store 启动 effect），此处可「重新自检 / 框选种子（人工标定点位）/ 测试点击」。
 */
export function RpaSelfCheck() {
  const {
    certInstalled,
    system,
    refreshCert,
    refreshStatus,
    rpaCheck,
    rpaChecking,
    refreshRpaCheck,
    rpaPicking,
    rpaMarkMsg,
    rpaMark,
    beginSeedPick,
    cancelSeedPick,
    rpaTestClicking,
    rpaTestClickResult,
    testClickRpa,
    proxyProbe,
    proxyProbing,
    refreshProxyProbe,
  } = useApp();

  const steps = toSteps(
    certInstalled,
    system,
    rpaCheck,
    rpaChecking,
    proxyProbe,
    proxyProbing,
    rpaTestClicking,
    rpaTestClickResult,
  );
  // 就绪 = 证书 + 系统代理能拦截 + 微信 + 文件助手窗口 + 种子点位已标定。
  const ready =
    !!certInstalled &&
    !!proxyProbe?.intercept_ok &&
    !!system?.wechat_running &&
    !!rpaCheck?.fh_found &&
    rpaCheck?.mark === "ok";
  const busy =
    rpaChecking || proxyProbing || certInstalled === null || system === null;

  const suggestPick = !!rpaCheck && rpaCheck.fh_found && rpaCheck.mark !== "ok";

  const recheck = () => {
    void refreshCert();
    void refreshStatus();
    void refreshRpaCheck();
    void refreshProxyProbe();
  };

  return (
    <Card className="mb-3">
      <CardHeader className="flex flex-row items-start justify-between gap-2 p-4 pb-2">
        <div>
          <CardTitle className="flex items-center gap-2 text-sm">
            启动自检（环境）
            <span
              className={cn(
                "rounded-full px-2 py-0.5 text-[11px] font-medium",
                ready
                  ? "bg-emerald-500/15 text-emerald-600"
                  : "bg-amber-500/15 text-amber-600",
              )}
            >
              {busy ? "自检中…" : ready ? "就绪" : "未就绪"}
            </span>
          </CardTitle>
        </div>
        {/* 每个按钮挂一个简短说明气泡（悬停/聚焦弹出，基于 Radix Tooltip）。 */}
        <TooltipProvider delayDuration={200} skipDelayDuration={400}>
          <div className="flex shrink-0 items-center gap-2">
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="outline" size="sm" loading={busy} onClick={recheck}>
                  {!busy && <RefreshCw />}
                  重新自检
                </Button>
              </TooltipTrigger>
              <TooltipContent>
                重新检查前置环境（证书 / 系统代理拦截 / 微信 / 文件助手窗口 / 种子点位标定）。
                只查状态，不动鼠标、不拉起浏览器。
              </TooltipContent>
            </Tooltip>
            {rpaPicking ? (
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="destructive"
                    size="sm"
                    onClick={() => void cancelSeedPick()}
                  >
                    <X />
                    取消框选
                  </Button>
                </TooltipTrigger>
                <TooltipContent>
                  关掉全屏框选层（在框选层内按 Esc 或右键效果相同）。
                </TooltipContent>
              </Tooltip>
            ) : (
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={rpaTestClicking || !rpaCheck?.fh_found}
                    onClick={() => void beginSeedPick()}
                  >
                    <Crosshair />
                    框选种子
                  </Button>
                </TooltipTrigger>
                <TooltipContent>
                  人工标定种子点位：置顶「文件传输助手」后弹出全屏框选层，用鼠标框住
                  <span className="font-semibold">种子链接那行蓝字</span>即可。
                  位置会缓存，之后每次自动点它；文件助手窗口尺寸 / 分辨率变了、消息被顶走要重新框选。
                </TooltipContent>
              </Tooltip>
            )}
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="secondary"
                  size="sm"
                  loading={rpaTestClicking}
                  disabled={rpaPicking || rpaCheck?.mark !== "ok"}
                  onClick={() => void testClickRpa()}
                >
                  {!rpaTestClicking && <MousePointerClick />}
                  测试点击
                </Button>
              </TooltipTrigger>
              <TooltipContent>
                标定后验证「浏览器拉起」：先清残留浏览器，再置顶文件助手 → 在标定框内
                <span className="font-semibold">随机取点真点一次</span> →
                看是否拉起微信内置浏览器（窗口留着不关，便于核对打开的是不是种子页）。会真的动鼠标。
              </TooltipContent>
            </Tooltip>
          </div>
        </TooltipProvider>
      </CardHeader>
      <CardContent className="p-4 pt-2">
        {/* 横线 step：每步 flex-1，节点居中，左右各半条连线，线正好接在节点中心 */}
        <div className="flex items-start pt-1">
          {steps.map((s, i) => (
            <div key={s.key} className="flex flex-1 flex-col items-center">
              <div className="flex w-full items-center">
                <div
                  className={cn(
                    "h-0.5 flex-1",
                    i === 0
                      ? "invisible"
                      : steps[i - 1].state === "done"
                        ? "bg-emerald-500"
                        : "bg-border",
                  )}
                />
                <StepNode state={s.state} />
                <div
                  className={cn(
                    "h-0.5 flex-1",
                    i === steps.length - 1
                      ? "invisible"
                      : s.state === "done"
                        ? "bg-emerald-500"
                        : "bg-border",
                  )}
                />
              </div>
              <div className="mt-1.5 px-1 text-center text-[11.5px] font-medium leading-tight">
                {s.label}
              </div>
              {s.sub && (
                <div className="mt-0.5 px-1 text-center text-[10.5px] text-muted-foreground">
                  {s.sub}
                </div>
              )}
            </div>
          ))}
        </div>

        {/* 显示缩放诊断：坐标全走物理像素、缩放≠100% 本身不影响点击，但点击落空时先看这里排除缩放/分辨率因素。 */}
        {rpaCheck?.fh_found && typeof rpaCheck.scale_pct === "number" && (
          <p className="mt-3 text-xs text-muted-foreground">
            显示缩放 {rpaCheck.scale_pct}%
            {rpaCheck.screen && ` · 分辨率 ${rpaCheck.screen[0]}×${rpaCheck.screen[1]}`}
            {rpaCheck.scale_pct !== 100 &&
              "（点击按物理像素，缩放不影响；若点击总落空可先排除缩放/分辨率因素）"}
          </p>
        )}
        {/* 系统代理拦截探针诊断 */}
        {proxyProbe && !proxyProbe.intercept_ok && (
          <p className="mt-3 text-xs text-amber-600">{proxyProbe.message}</p>
        )}
        {/* 诊断消息（RPA 侧，Windows 才有意义） */}
        {rpaCheck?.message && (
          <p
            className={cn(
              "mt-3 text-xs",
              ready ? "text-muted-foreground" : "text-amber-600",
            )}
          >
            {rpaCheck.message}
          </p>
        )}
        {rpaPicking && (
          <p className="mt-1 text-xs font-semibold text-foreground">
            框选层已打开：在「文件传输助手」窗口里按住鼠标拖一个框，框住种子链接那条消息后松开即完成；按 Esc 取消。
          </p>
        )}
        {suggestPick && !rpaPicking && (
          <p className="mt-1 text-xs text-muted-foreground">
            提示：先把种子链接发到「文件传输助手」（见上方「种子链接」），再点「框选种子」框住那条消息；之后请保持文件助手窗口尺寸与消息位置不变。
          </p>
        )}
        {rpaMarkMsg && (
          <p
            className={cn(
              "mt-1 break-all text-xs",
              rpaMark ? "text-muted-foreground" : "text-amber-600",
            )}
          >
            {rpaMarkMsg}
          </p>
        )}
        {/* 测试点击结果：结论 + 关键中间量（点位 / 是否点击 / 新窗口），供人工核对标定 */}
        {rpaTestClickResult && (
          <div
            className={cn(
              "mt-2 rounded-md border px-3 py-2 text-xs",
              rpaTestClickResult.ok
                ? "border-emerald-500/30 bg-emerald-500/5 text-emerald-700"
                : "border-amber-500/30 bg-amber-500/5 text-amber-700",
            )}
          >
            <p className="font-medium">
              {rpaTestClickResult.ok ? "测试点击成功" : "测试点击未成功"}
            </p>
            <p className="mt-0.5 break-all">{rpaTestClickResult.message}</p>
            <p className="mt-1 text-muted-foreground">
              点位{" "}
              {rpaTestClickResult.point
                ? `(${rpaTestClickResult.point[0]}, ${rpaTestClickResult.point[1]})`
                : "无"}{" "}
              · 已点击 {rpaTestClickResult.clicked ? "是" : "否"} · 新窗口{" "}
              {rpaTestClickResult.opened.length}
              {rpaTestClickResult.fh_rect &&
                ` · 文件助手 (${rpaTestClickResult.fh_rect.join(", ")})`}
              {rpaTestClickResult.screen &&
                ` · 屏幕 ${rpaTestClickResult.screen[0]}×${rpaTestClickResult.screen[1]}`}
              {typeof rpaTestClickResult.scale_pct === "number" &&
                ` · 缩放 ${rpaTestClickResult.scale_pct}%`}
            </p>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
