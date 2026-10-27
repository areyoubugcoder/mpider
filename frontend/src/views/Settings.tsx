import { useEffect, useMemo, useRef, useState } from "react";
import { useMutation } from "@tanstack/react-query";
import {
  Bell,
  ChevronsDownUp,
  ChevronsUpDown,
  Copy,
  Download,
  Upload,
  FileText,
  Gauge,
  History,
  KeyRound,
  Network,
  Radar,
  RefreshCw,
  RotateCcw,
  RotateCw,
  Route,
  Send,
  ShieldCheck,
  ShieldOff,
} from "lucide-react";
import { toast } from "sonner";
import { useApp } from "@/store";
import { msg } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import {
  CollapsibleSection,
  useSectionOpenState,
} from "@/components/ui/collapsible-section";
import {
  DEFAULT_CFG,
  captureManualStart,
  captureManualStop,
  captureStatus,
  notifyStatus,
  notifyTest,
  type RealRunConfig,
  exportConfig,
  parseConfigImport,
  revealInFolder,
} from "@/lib/tauri";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { useQuery } from "@tanstack/react-query";

// ============ 区块划分与展开状态 ============

/** 设置页各区块 id（证书 + 七个配置模块）。 */
type SectionId =
  | "cert"
  | "report"
  | "capture"
  | "relay"
  | "list"
  | "cred"
  | "sweep"
  | "history"
  | "detail"
  | "notify";

/** 展开状态持久化 key。 */
const OPEN_STATE_KEY = "mp.settings.open";

/** 默认展开策略：只展开「抓包与系统代理」；证书按安装状态决定（null = 运行时决定）；其余收起。 */
const OPEN_DEFAULTS: Record<SectionId, boolean | null> = {
  cert: null,
  report: false,
  capture: true,
  relay: false,
  list: false,
  cred: false,
  sweep: false,
  history: false,
  detail: false,
  notify: false,
};

/** 每个配置区块包含的字段（用于「已修改」标记）。 */
const SECTION_KEYS: Record<
  Exclude<SectionId, "cert">,
  (keyof RealRunConfig)[]
> = {
  report: ["report_enabled", "report_url", "report_token", "report_timeout_secs"],
  capture: [
    "capture_port",
    "seed_host",
    "seed_port",
    "sysproxy_service",
    "set_sysproxy",
    "capture_wait_seconds",
  ],
  relay: [
    "relay_dwell_ms",
    "relay_dwell_max_ms",
    "seed_dwell_ms",
    "relay_launch_timeout_seconds",
    "relay_stall_seconds",
    "relay_max_relaunch",
    "rpa_hard_refresh",
  ],
  list: [
    "list_gap_min_ms",
    "list_gap_max_ms",
    "list_daily_budget",
    "page_sleep_min_ms",
    "page_sleep_max_ms",
    "list_max_pages",
  ],
  cred: ["cred_refresh_attempts", "cred_refresh_wait_seconds"],
  sweep: [
    "sweep_idle_seconds",
    "sweep_batch_size",
    "sweep_batch_gap_seconds",
    "sweep_fail_pause_seconds",
  ],
  history: ["history_page_count", "history_gap_seconds", "history_budget_reserve"],
  detail: ["detail_throttle_ms", "detail_workers"],
  notify: ["feishu_enabled", "feishu_webhook", "feishu_secret"],
};

type OpenState = ReturnType<typeof useSectionOpenState<SectionId>>;

// ============ 摘要格式化 ============

/** 毫秒 → 秒的短写：8000 → "8"，2500 → "2.5"。 */
function secShort(ms: number): string {
  const s = (Number(ms) || 0) / 1000;
  return Number.isInteger(s) ? String(s) : s.toFixed(1).replace(/\.0$/, "");
}

/** 毫秒区间 → 「8–20s」；上限不大于下限即固定值「8s」。 */
function fmtMsRange(minMs: number, maxMs: number): string {
  const lo = Number(minMs) || 0;
  const hi = Number(maxMs) || 0;
  return hi > lo ? `${secShort(lo)}–${secShort(hi)}s` : `${secShort(lo)}s`;
}

/** 秒数 → 「1 小时」/「1 小时 30 分」/「15 分」/「45 秒」。 */
function fmtDur(secs: number): string {
  const s = Math.max(0, Math.floor(Number(secs) || 0));
  if (s >= 3600) {
    const m = Math.floor((s % 3600) / 60);
    return m
      ? `${Math.floor(s / 3600)} 小时 ${m} 分`
      : `${Math.floor(s / 3600)} 小时`;
  }
  if (s >= 60) return `${Math.floor(s / 60)} 分`;
  return `${s} 秒`;
}

// ============ 手动抓凭证代理（调试） ============

/**
 * 「手动开启抓凭证代理」面板：不跑任务只起 MITM + 系统代理，用户自己在微信里点链接，
 * 每条解密到的微信请求路径与 profile_ext 响应概况写进「抓凭证」环节日志（日志页按环节筛）。
 * 到时自动停；与整链互斥。用表单当前值（端口 / 网络服务名 / 是否设系统代理），不必先保存。
 */
function ManualCapturePanel({ form }: { form: RealRunConfig }) {
  const [minutes, setMinutes] = useState(10);
  const [now, setNow] = useState(() => Date.now() / 1000);
  const state = useQuery({
    queryKey: ["capture-status"],
    queryFn: captureStatus,
    refetchInterval: 2_000,
  });
  const running = state.data?.running ?? false;
  // 剩余时间倒计时（本地每秒刷新，不额外打后端）。
  useEffect(() => {
    if (!running) return;
    const t = window.setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => window.clearInterval(t);
  }, [running]);
  const start = useMutation({
    mutationFn: () => captureManualStart(form, minutes),
    onSuccess: (st) => {
      void state.refetch();
      toast.success(`抓凭证代理已开启，监听 ${st.proxy_addr ?? ""}`, {
        description: st.sysproxy_set
          ? `现在去微信里点链接；${minutes} 分钟后自动停，日志页按环节「抓凭证」看请求`
          : "未设系统代理（开关关着），请手动把系统代理指向该地址",
      });
    },
    onError: (e) => toast.error("开启失败", { description: msg(e) }),
  });
  const stop = useMutation({
    mutationFn: captureManualStop,
    onSuccess: () => {
      void state.refetch();
      toast.success("抓凭证代理已停止，系统代理已复位");
    },
    onError: (e) => toast.error("停止失败", { description: msg(e) }),
  });
  const left = state.data?.stop_at ? Math.max(0, state.data.stop_at - now) : 0;

  return (
    <div className="mt-3 rounded-lg border border-border bg-muted/30 p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-2 text-xs font-medium">
            手动开启抓凭证代理（调试）
            {running ? (
              <Badge variant="success">
                运行中 · {state.data?.proxy_addr} · 剩 {fmtDur(left)}
              </Badge>
            ) : (
              <Badge variant="muted">未开启</Badge>
            )}
            {running && state.data && !state.data.sysproxy_set ? (
              <Badge variant="warning">系统代理未指向</Badge>
            ) : null}
          </div>
          <p className="mt-0.5 text-xs text-muted-foreground">
            不跑任务只起 MITM 和系统代理，你自己在微信里点链接；每条解密到的微信请求路径与
            profile_ext 响应概况记进日志页环节「抓凭证」，抓到的凭证照常入库。开着期间不能运行采集，到时自动停。
          </p>
          {state.data?.last_error ? (
            <p className="mt-0.5 text-xs text-destructive">
              {state.data.last_error}
            </p>
          ) : null}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {running ? (
            <Button
              size="sm"
              variant="outline"
              onClick={() => stop.mutate()}
              disabled={stop.isPending}
            >
              {stop.isPending ? "停止中…" : "停止"}
            </Button>
          ) : (
            <>
              <Input
                type="number"
                min={1}
                max={60}
                className="w-20"
                value={minutes}
                onChange={(e) =>
                  setMinutes(
                    Math.min(60, Math.max(1, Number(e.target.value) || 1)),
                  )
                }
                title="自动停止时长（分钟，1~60）"
              />
              <span className="text-xs text-muted-foreground">分钟</span>
              <Button
                size="sm"
                onClick={() => start.mutate()}
                disabled={start.isPending}
              >
                <Radar />
                {start.isPending ? "开启中…" : "开启"}
              </Button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

// ============ 证书 ============

/** 证书向导徽章：检查中 / 已安装 ✓ / 未安装。读单一数据源 certInstalled。 */
function CertBadge({ installed }: { installed: boolean | null }) {
  if (installed === null) return <Badge variant="muted">检查中…</Badge>;
  return installed ? (
    <Badge variant="success">已安装 ✓</Badge>
  ) : (
    <Badge variant="warning">未安装</Badge>
  );
}

/**
 * 傻瓜式证书安装向导（可折叠）。
 * 默认：已安装收起（标题行只留「已安装 ✓」徽章）；未安装 / 检查中展开。
 * 开始安装 / 移除时强制展开，让操作日志可见。
 */
function CertWizard({ openState }: { openState: OpenState }) {
  const {
    certInstalled,
    certDetail,
    certLog,
    installing,
    uninstalling,
    installCert,
    uninstallCert,
    refreshCert,
  } = useApp();
  const [showManual, setShowManual] = useState(false);

  const open = openState.isOpen("cert", certInstalled !== true);

  /** 重新检查证书状态（按钮显示 loading；结果由徽章体现，只在异常时 toast）。 */
  const recheck = useMutation({
    mutationFn: () => refreshCert(),
    onError: (e) => toast.error("检查证书状态失败", { description: msg(e) }),
  });

  const runInstall = () => {
    openState.setOpen("cert", true);
    void installCert();
  };
  const runUninstall = () => {
    openState.setOpen("cert", true);
    void uninstallCert();
  };

  return (
    <CollapsibleSection
      icon={<ShieldCheck />}
      title="证书"
      badge={
        <>
          <CertBadge installed={certInstalled} />
          {!certInstalled && (
            <span className="text-xs text-warning">第一次使用必须做</span>
          )}
        </>
      }
      summary={certInstalled ? "正常使用无需再动" : "抓取前必须安装"}
      description="给工具配一把只读得懂公众号页面的钥匙（mp.weixin.qq.com），不碰微信聊天记录，也不影响其它软件"
      open={open}
      onOpenChange={(v) => openState.setOpen("cert", v)}
    >
      <div className="mb-3.5 space-y-2 text-xs leading-relaxed">
        <p>
          这个工具要帮你<b>抓取微信公众号的文章</b>
          。公众号网页是加密传输的，就像信封贴了封条——工具默认看不到里面的内容。
        </p>
        <ul className="ml-5 list-disc space-y-1">
          <li>
            <b>不装</b>：抓取会失败，运行没有结果。
          </li>
          <li>
            <b>装了</b>：本机多出一张<b>只给本工具用</b>
            的证书，你随时可以在系统「钥匙串访问」里删掉它。
          </li>
        </ul>
      </div>

      <div className="mb-2 flex flex-wrap items-center gap-2.5">
        {/* 已安装 → 主操作变成「移除证书」；未安装 / 检查中 → 「一键安装证书」 */}
        {certInstalled ? (
          <Button
            variant="outline"
            className="text-destructive hover:text-destructive"
            loading={uninstalling}
            onClick={runUninstall}
            disabled={!certDetail.can_auto}
          >
            {!uninstalling && <ShieldOff />}
            {uninstalling
              ? "移除中…（注意密码框）"
              : certDetail.can_auto
                ? "移除证书"
                : "该平台请手动移除"}
          </Button>
        ) : (
          <Button
            loading={installing}
            onClick={runInstall}
            disabled={!certDetail.can_auto || certInstalled === null}
          >
            {!installing && <KeyRound />}
            {installing
              ? "安装中…（注意密码框）"
              : certDetail.can_auto
                ? "一键安装证书"
                : "该平台请手动安装"}
          </Button>
        )}
        <Button
          variant="outline"
          loading={recheck.isPending}
          onClick={() => recheck.mutate()}
        >
          {!recheck.isPending && <RotateCw />}
          重新检查
        </Button>
        {!certInstalled && (
          <Button variant="link" onClick={() => setShowManual((v) => !v)}>
            装不上？手动安装
          </Button>
        )}
      </div>

      <p className="mt-1.5 text-xs text-muted-foreground">
        {certInstalled ? (
          <>
            证书已装好，正常使用无需再动。点「移除证书」会把它从系统信任库删掉
            （系统可能弹密码框），删掉后抓取会失败，需要时可再次一键安装。
          </>
        ) : (
          <>
            点「一键安装」后，系统会弹出密码框，输入你的<b>开机/登录密码</b>
            授权即可（只需这一次）。
          </>
        )}
      </p>

      {certLog.length > 0 && (
        <pre className="mt-3 max-h-40 overflow-y-auto whitespace-pre-wrap break-words rounded-lg bg-muted/60 p-2.5 font-mono text-xs leading-relaxed">
          {certLog.join("\n")}
        </pre>
      )}

      {showManual && !certInstalled && (
        <div className="mt-3 rounded-lg bg-muted/50 p-3.5 text-xs leading-relaxed">
          <p className="mb-1 text-muted-foreground">
            如果一键安装没成功，可以这样手动装：
          </p>
          <ol className="ml-5 list-decimal space-y-1">
            <li>
              证书文件在这里：
              <code className="break-all">
                {certDetail.ca_path || "（点安装时生成）"}
              </code>
            </li>
            <li>
              打开「终端」，粘贴执行：
              <br />
              <code className="break-all">
                {certDetail.manual_command || "（当前平台无自动命令）"}
              </code>
            </li>
            <li>
              或者直接双击证书文件，在「钥匙串访问」里把它设为「始终信任」。
            </li>
          </ol>
        </div>
      )}
    </CollapsibleSection>
  );
}

// ============ 运行配置表单 ============

/** 表单字段：标签 + 输入 + 说明。 */
function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <Label className="text-xs text-muted-foreground">{label}</Label>
      {children}
      {hint && (
        <small className="text-[11px] text-muted-foreground">{hint}</small>
      )}
    </div>
  );
}

/** 区块内的两列字段网格。 */
function FieldGrid({ children }: { children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-1 gap-x-4 gap-y-4 md:grid-cols-2">
      {children}
    </div>
  );
}

/** 开关行：标题 + 说明 + Switch（占满两列）。 */
function SwitchRow({
  title,
  hint,
  checked,
  onCheckedChange,
}: {
  title: string;
  hint: string;
  checked: boolean;
  onCheckedChange: (v: boolean) => void;
}) {
  return (
    <div className="flex items-center justify-between rounded-lg bg-muted/50 p-3 md:col-span-2">
      <div className="pr-4">
        <div className="text-xs font-medium">{title}</div>
        <div className="mt-0.5 text-xs text-muted-foreground">{hint}</div>
      </div>
      <Switch checked={checked} onCheckedChange={onCheckedChange} />
    </div>
  );
}

/** 运行配置表单：按功能模块分区块折叠，底部 sticky 保存条。 */
function RunConfigForm({ openState }: { openState: OpenState }) {
  const { cfg, saveCfg, running, autoClick, isMac } = useApp();
  const [form, setForm] = useState<RealRunConfig>(cfg);

  // store 里的 cfg 变化（如别处保存）时同步到本地表单。
  useEffect(() => {
    setForm(cfg);
  }, [cfg]);

  const set = <K extends keyof RealRunConfig>(k: K, v: RealRunConfig[K]) =>
    setForm((f) => ({ ...f, [k]: v }));

  /** 字段级浅比较：每个区块 / 整体是否有未保存的修改。 */
  const dirtyKeys = useMemo(() => {
    const s = new Set<keyof RealRunConfig>();
    for (const k of Object.keys(form) as (keyof RealRunConfig)[]) {
      if (form[k] !== cfg[k]) s.add(k);
    }
    return s;
  }, [form, cfg]);
  const isDirty = (id: Exclude<SectionId, "cert">) =>
    SECTION_KEYS[id].some((k) => dirtyKeys.has(k));
  const anyDirty = dirtyKeys.size > 0;

  const onSave = () => {
    const next: RealRunConfig = {
      report_enabled: form.report_enabled,
      report_url: form.report_url.trim(),
      report_token: form.report_token.trim(),
      report_timeout_secs: Math.max(
        1,
        Math.round(Number(form.report_timeout_secs) || 15),
      ),
      capture_port: Number(form.capture_port) || 0,
      seed_host: form.seed_host.trim() || "127.0.0.1",
      seed_port: Math.min(
        65535,
        Math.max(1, Math.round(Number(form.seed_port) || 8787)),
      ),
      relay_dwell_ms: Number(form.relay_dwell_ms) || 0,
      relay_dwell_max_ms: Number(form.relay_dwell_max_ms) || 0,
      seed_dwell_ms: Math.max(0, Math.round(Number(form.seed_dwell_ms) || 0)),
      capture_wait_seconds: Number(form.capture_wait_seconds) || 0,
      relay_launch_timeout_seconds: Math.max(
        0,
        Number(form.relay_launch_timeout_seconds) || 0,
      ),
      relay_stall_seconds: Math.max(0, Number(form.relay_stall_seconds) || 0),
      relay_max_relaunch: Math.max(
        0,
        Math.round(Number(form.relay_max_relaunch) || 0),
      ),
      list_max_pages: Math.max(
        1,
        Math.round(Number(form.list_max_pages) || 50),
      ),
      page_sleep_min_ms: Math.max(0, Number(form.page_sleep_min_ms) || 0),
      page_sleep_max_ms: Math.max(0, Number(form.page_sleep_max_ms) || 0),
      // 产品要求至少 3 次重试
      cred_refresh_attempts: Math.max(
        3,
        Math.round(Number(form.cred_refresh_attempts) || 3),
      ),
      cred_refresh_wait_seconds: Math.max(
        0,
        Number(form.cred_refresh_wait_seconds) || 0,
      ),
      set_sysproxy: form.set_sysproxy,
      sysproxy_service: form.sysproxy_service.trim() || "Wi-Fi",
      rpa_hard_refresh: !!form.rpa_hard_refresh,
      sweep_idle_seconds: Math.max(
        60,
        Math.round(Number(form.sweep_idle_seconds) || 3600),
      ),
      sweep_batch_size: Math.min(
        100,
        Math.max(1, Math.round(Number(form.sweep_batch_size) || 20)),
      ),
      list_gap_min_ms: Math.max(
        0,
        Math.round(Number(form.list_gap_min_ms) || 0),
      ),
      list_gap_max_ms: Math.max(
        0,
        Math.round(Number(form.list_gap_max_ms) || 0),
      ),
      list_daily_budget: Math.max(
        0,
        Math.round(Number(form.list_daily_budget) || 0),
      ),
      sweep_batch_gap_seconds: Math.max(
        0,
        Math.round(Number(form.sweep_batch_gap_seconds) || 0),
      ),
      sweep_fail_pause_seconds: Math.max(
        0,
        Math.round(Number(form.sweep_fail_pause_seconds) || 0),
      ),
      history_page_count: Math.min(
        50,
        Math.max(1, Math.round(Number(form.history_page_count) || 10)),
      ),
      history_gap_seconds: Math.max(
        Math.ceil((Number(form.list_gap_max_ms) || 0) / 1000),
        Math.round(Number(form.history_gap_seconds) || 60),
      ),
      history_budget_reserve: Math.max(
        0,
        Math.round(Number(form.history_budget_reserve) || 0),
      ),
      detail_throttle_ms: Math.max(0, Number(form.detail_throttle_ms) || 2000),
      detail_workers: Math.min(
        12,
        Math.max(1, Math.round(Number(form.detail_workers) || 1)),
      ),
      feishu_enabled: form.feishu_enabled,
      feishu_webhook: form.feishu_webhook.trim(),
      feishu_secret: form.feishu_secret.trim(),
    };
    if (next.report_enabled && !/^https?:\/\/.+/.test(next.report_url)) {
      toast.error("上报地址不合法", {
        description: "开启数据上报后必须填 http:// 或 https:// 开头的地址",
      });
      return;
    }
    if (
      next.feishu_enabled &&
      !/^https:\/\/.+\/open-apis\/bot\/v2\/hook\/.+/.test(next.feishu_webhook)
    ) {
      toast.error("飞书通知已开启但 Webhook 地址不对", {
        description:
          "应形如 https://open.feishu.cn/open-apis/bot/v2/hook/<uuid>；先关掉开关或填对地址再保存",
      });
      return;
    }
    try {
      saveCfg(next);
      toast.success("配置已保存", {
        description: running
          ? "巡检正在运行：新配置在下次「开始巡检」时生效"
          : "下次「开始巡检」/ 抓取历史即按这套配置运行",
      });
    } catch (e) {
      toast.error("保存配置失败", { description: msg(e) });
    }
  };

  /** 恢复默认：只重置表单不自动保存；上报地址 / token / 飞书机器人配置保留，避免误清。 */
  const onReset = () => {
    setForm((f) => ({
      ...DEFAULT_CFG,
      report_url: f.report_url,
      report_token: f.report_token,
      feishu_webhook: f.feishu_webhook,
      feishu_secret: f.feishu_secret,
    }));
    toast.info("已填入默认值（上报地址 / token / 飞书机器人保留）", {
      description: "尚未保存：确认无误后点「保存配置」",
    });
  };

  /**
   * 导出运行配置：导出的是**已保存**的那份（cfg），不是表单里未保存的改动。
   * 文本含上报 token / 飞书 Webhook 与密钥原文，导出后提醒注意保管。
   */
  const exportNotes = () => {
    const notes: string[] = [];
    if (anyDirty) notes.push("有未保存的修改，导出的是已保存的配置");
    if (cfg.report_token || cfg.feishu_webhook || cfg.feishu_secret)
      notes.push("内容含上报 token / 飞书 Webhook 与密钥，注意保管");
    return notes.join("；") || undefined;
  };
  const [exporting, setExporting] = useState<"" | "file" | "clipboard">("");
  const onExportFile = async () => {
    setExporting("file");
    try {
      const out = await exportConfig(cfg, true);
      const path = out.path ?? "";
      toast.success("配置已导出为文件", {
        description: [path, exportNotes()].filter(Boolean).join("\n"),
        action: path
          ? {
              label: "打开所在文件夹",
              onClick: () =>
                void revealInFolder(path).catch((e) =>
                  toast.error("打开文件夹失败", { description: msg(e) }),
                ),
            }
          : undefined,
      });
    } catch (e) {
      toast.error("导出配置失败", { description: msg(e) });
    } finally {
      setExporting("");
    }
  };
  const onExportClipboard = async () => {
    setExporting("clipboard");
    try {
      const out = await exportConfig(cfg, false);
      await navigator.clipboard.writeText(out.text);
      toast.success("配置已复制到剪贴板", { description: exportNotes() });
    } catch (e) {
      toast.error("复制配置失败", { description: msg(e) });
    } finally {
      setExporting("");
    }
  };

  /**
   * 导入运行配置：粘贴文本或选文件，解析后**只填入表单不自动保存**（与「恢复默认」同一节奏），
   * 让用户看一眼再点「保存配置」。识别不出的键 / 类型不符的字段忽略并在提示里列出。
   */
  const [importOpen, setImportOpen] = useState(false);
  const [importText, setImportText] = useState("");
  const [importError, setImportError] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const applyImport = (text: string) => {
    try {
      const parsed = parseConfigImport(text);
      setForm((f) => ({ ...f, ...parsed.cfg }));
      setImportOpen(false);
      setImportText("");
      setImportError(null);
      const skipped: string[] = [];
      if (parsed.unknown.length)
        skipped.push(
          `未知键 ${parsed.unknown.length} 个：${parsed.unknown.join("、")}`,
        );
      if (parsed.badType.length)
        skipped.push(
          `类型不符 ${parsed.badType.length} 个：${parsed.badType.join("、")}`,
        );
      toast.success(`已填入 ${parsed.applied.length} 项配置，尚未保存`, {
        description: ["确认无误后点「保存配置」", ...skipped].join("\n"),
      });
    } catch (e) {
      setImportError(msg(e));
    }
  };
  const onImportFile = async (file: File | undefined) => {
    if (!file) return;
    try {
      applyImport(await file.text());
    } catch (e) {
      setImportError("读取文件失败：" + msg(e));
    } finally {
      // 清掉选择，方便再次选同一个文件时仍触发 change
      if (fileInputRef.current) fileInputRef.current.value = "";
    }
  };

  // 飞书通知：发送测试消息（用表单里的值，不要求先保存）+ 运行状态。
  const [showSecret, setShowSecret] = useState(false);
  const notifyState = useQuery({
    queryKey: ["notify-status"],
    queryFn: notifyStatus,
    refetchInterval: 10_000,
    enabled: openState.isOpen("notify"),
  });
  const testNotify = useMutation({
    mutationFn: () => notifyTest({ ...form, feishu_enabled: true }),
    onSuccess: () => {
      toast.success("测试消息已发到飞书群", {
        description: "去群里看一眼；确认无误后记得「保存配置」",
      });
      void notifyState.refetch();
    },
    onError: (e) => toast.error("测试消息发送失败", { description: msg(e) }),
  });

  const section = (id: Exclude<SectionId, "cert">) => ({
    open: openState.isOpen(id),
    onOpenChange: (v: boolean) => openState.setOpen(id, v),
    dirty: isDirty(id),
  });

  return (
    <>
      {/* ── 数据上报 ── */}
      <CollapsibleSection
        icon={<Send />}
        title="数据上报"
        badge={
          form.report_enabled ? (
            <Badge variant="success">已开启</Badge>
          ) : (
            <Badge variant="muted">未开启</Badge>
          )
        }
        description="把采集结果推给你自己的 HTTP 服务：每个公众号列表采完后 POST 一次 JSON（巡检与历史抓取都报；没有新文章也报）。留空 / 关闭则只入本地库"
        summary={
          form.report_enabled
            ? `${form.report_url.trim() || "未填地址"} · 超时 ${form.report_timeout_secs}s`
            : "未开启"
        }
        {...section("report")}
      >
        <FieldGrid>
          <SwitchRow
            title="开启数据上报"
            hint="每个公众号列表采完后向下面的地址 POST 一次 JSON（Content-Type: application/json）；2xx 视为成功，失败记进任务列表并推飞书预警，不重试。载荷字段见 README「数据上报」"
            checked={form.report_enabled}
            onCheckedChange={(v) => set("report_enabled", v)}
          />
          <Field
            label="上报地址 report_url"
            hint="必须以 http:// 或 https:// 开头，例如 https://example.com/api/mp-articles"
          >
            <Input
              type="text"
              placeholder="https://example.com/api/mp-articles"
              value={form.report_url}
              onChange={(e) => set("report_url", e.target.value)}
            />
          </Field>
          <Field
            label="鉴权 token report_token"
            hint="可选；填了则带 Authorization: Bearer <token>。只存本机，不进日志"
          >
            <Input
              type="password"
              placeholder="可选"
              value={form.report_token}
              onChange={(e) => set("report_token", e.target.value)}
            />
          </Field>
          <Field
            label="请求超时 report_timeout_secs"
            hint="秒；超过即算上报失败。默认 15"
          >
            <Input
              type="number"
              min={1}
              value={form.report_timeout_secs}
              onChange={(e) =>
                set("report_timeout_secs", Number(e.target.value))
              }
            />
          </Field>
        </FieldGrid>
      </CollapsibleSection>

      {/* ── 飞书通知 ── */}
      <CollapsibleSection
        icon={<Bell />}
        title="飞书通知"
        badge={
          form.feishu_enabled ? (
            <Badge variant="success">已开启</Badge>
          ) : (
            <Badge variant="muted">未开启</Badge>
          )
        }
        description="只在出问题时推预警到飞书群：任务超时 / 出错、上报失败、限流退避 / 封号、巡检暂停（环境故障 / 整批失败 / 预算用完）、采集出错退出。正常运行不打扰"
        summary={
          form.feishu_enabled
            ? `已开启${form.feishu_webhook.trim() ? "" : " · 未填 Webhook"}${
                notifyState.data
                  ? ` · 已发 ${notifyState.data.sent} 条，失败 ${notifyState.data.failed} 条`
                  : ""
              }`
            : "未开启"
        }
        {...section("notify")}
      >
        <FieldGrid>
          <SwitchRow
            title="开启飞书通知"
            hint="开启后填下面的机器人 Webhook 与签名密钥。推送内容只有「发生了什么、影响是什么」，不含微信接口参数；同一文案 60 秒内只发一次，同类预警 10 分钟内合并成一条"
            checked={form.feishu_enabled}
            onCheckedChange={(v) => set("feishu_enabled", v)}
          />
          <Field
            label="机器人 Webhook feishu_webhook"
            hint="飞书群 → 设置 → 群机器人 → 添加「自定义机器人」→ 复制 Webhook 地址。只存本机，不进日志"
          >
            <Input
              type="text"
              placeholder="https://open.feishu.cn/open-apis/bot/v2/hook/…"
              value={form.feishu_webhook}
              onChange={(e) => set("feishu_webhook", e.target.value)}
              autoComplete="off"
            />
          </Field>
          <Field
            label="签名密钥 feishu_secret"
            hint="机器人安全设置里勾了「签名校验」就填这里的密钥；没勾留空"
          >
            <div className="flex gap-2">
              <Input
                type={showSecret ? "text" : "password"}
                value={form.feishu_secret}
                onChange={(e) => set("feishu_secret", e.target.value)}
                autoComplete="new-password"
              />
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="h-9"
                onClick={() => setShowSecret((v) => !v)}
              >
                {showSecret ? "隐藏" : "显示"}
              </Button>
            </div>
          </Field>
          <div className="flex flex-wrap items-center gap-3 md:col-span-2">
            <Button
              variant="outline"
              size="sm"
              loading={testNotify.isPending}
              disabled={!form.feishu_webhook.trim()}
              onClick={() => testNotify.mutate()}
            >
              发送测试消息
            </Button>
            <span className="text-xs text-muted-foreground">
              {notifyState.data
                ? `运行状态：${notifyState.data.enabled ? "已开启" : "未开启"} · 已发 ${notifyState.data.sent} 条 · 失败 ${
                    notifyState.data.failed
                  } 条${notifyState.data.last_error ? ` · 最近错误：${notifyState.data.last_error}` : ""}`
                : "用上面填的值直接发一条测试，不需要先保存"}
            </span>
          </div>
        </FieldGrid>
      </CollapsibleSection>

      {/* ── 抓包与系统代理 ── */}
      <CollapsibleSection
        icon={<Network />}
        title="抓包与系统代理"
        description={
          autoClick
            ? "本机 MITM 代理如何监听、种子入口服务的地址、是否自动切换系统代理，以及等凭证的时长"
            : "本机 MITM 代理如何监听、是否自动切换系统代理，以及等凭证的时长"
        }
        summary={`端口 ${form.capture_port ? form.capture_port : "自动"}${
          autoClick ? ` · 种子 ${form.seed_host}:${form.seed_port}` : ""
        } · ${form.set_sysproxy ? "自动设系统代理" : "不设系统代理"} · 等待 ${
          form.capture_wait_seconds
        }s`}
        {...section("capture")}
      >
        <FieldGrid>
          <Field label="抓包端口 capture_port" hint="0 = 系统自动分配">
            <Input
              type="number"
              min={0}
              max={65535}
              value={form.capture_port}
              onChange={(e) => set("capture_port", Number(e.target.value))}
            />
          </Field>
          {/* 种子服务只服务于「自动点种子」（Windows）；人工模式打开任意文章即可，没有种子链接这回事 */}
          {autoClick && (
            <>
              <Field
                label="种子服务地址 seed_host"
                hint="写进种子链接的 host；默认 127.0.0.1 只绑本机，填本机局域网 IP 则对局域网开放。改了要重发种子并重新框选"
              >
                <Input
                  type="text"
                  value={form.seed_host}
                  onChange={(e) => set("seed_host", e.target.value)}
                />
              </Field>
              <Field
                label="种子服务端口 seed_port"
                hint="固定端口（默认 8787）；文件助手里那条链接不会跟着变"
              >
                <Input
                  type="number"
                  min={1}
                  max={65535}
                  value={form.seed_port}
                  onChange={(e) => set("seed_port", Number(e.target.value))}
                />
              </Field>
            </>
          )}
          {/* 网络服务名只有 mac 的 networksetup 用得到；Windows 走 WinINET，不看这个字段 */}
          {isMac && (
            <Field
              label="网络服务名 sysproxy_service"
              hint="设系统代理时用，一般是「Wi-Fi」"
            >
              <Input
                type="text"
                value={form.sysproxy_service}
                onChange={(e) => set("sysproxy_service", e.target.value)}
              />
            </Field>
          )}
          <Field
            label="抓包等待 capture_wait_seconds"
            hint={
              autoClick
                ? "秒；等微信内置浏览器发出带凭证请求的上限"
                : "秒；等微信内置浏览器发出带凭证请求的上限。运行时会提醒你去微信打开一篇文章，所以最少等 10 分钟；凭证到位即结束，不会白等"
            }
          >
            <Input
              type="number"
              min={0}
              value={form.capture_wait_seconds}
              onChange={(e) =>
                set("capture_wait_seconds", Number(e.target.value))
              }
            />
          </Field>
          <SwitchRow
            title="运行时自动设置 / 复位系统代理"
            hint={
              autoClick
                ? "开启后，运行会真的修改本机代理，并会尝试启动微信（RPA）——这是抓取的正常步骤；结束自动复位"
                : "开启后，运行会真的修改本机代理——这是抓取的正常步骤；结束自动复位"
            }
            checked={form.set_sysproxy}
            onCheckedChange={(v) => set("set_sysproxy", v)}
          />
        </FieldGrid>
        <ManualCapturePanel form={form} />
      </CollapsibleSection>

      {/* ── 微信浏览器自动翻链接 ── */}
      <CollapsibleSection
        icon={<Route />}
        title="微信浏览器自动翻链接"
        description={
          autoClick
            ? "任务运行时会在微信内置浏览器里自动逐条打开文章链接来换取凭证：打开种子链接后跳到第一条、每篇看完再跳下一条。这里设置每篇看多久再跳，以及卡住时怎么自动恢复"
            : "你在微信内置浏览器打开一篇文章后，会自动逐条打开本批其余文章链接来换取凭证：每篇看完再跳下一条。这里设置每篇看多久再跳，以及卡住多久算没反应"
        }
        summary={
          autoClick
            ? `种子页等 ${secShort(form.seed_dwell_ms)}s 跳第一条 · 每篇看 ${fmtMsRange(form.relay_dwell_ms, form.relay_dwell_max_ms)} 再跳下一条 · 打不开等 ${
                form.relay_launch_timeout_seconds
              }s · 卡住等 ${form.relay_stall_seconds}s · 最多重开 ${form.relay_max_relaunch} 次`
            : `每篇看 ${fmtMsRange(form.relay_dwell_ms, form.relay_dwell_max_ms)} 再跳下一条 · 卡住等 ${form.relay_stall_seconds}s`
        }
        {...section("relay")}
      >
        <FieldGrid>
          {/* 种子页第一跳只在自动点种子的平台存在 */}
          {autoClick && (
            <Field
              label="种子链接打开后多久跳到第一条 seed_dwell_ms"
              hint="毫秒；固定值，可为 0（立即跳），默认 100。种子页是本机直出的入口页、没内容要看；只管第一跳"
            >
              <Input
                type="number"
                min={0}
                value={form.seed_dwell_ms}
                onChange={(e) => set("seed_dwell_ms", Number(e.target.value))}
              />
            </Field>
          )}
          <Field
            label="每篇最少看多久再跳下一条 relay_dwell_ms"
            hint="毫秒；从文章 1 起，每篇跳下一条都在 [最少, 最多] 之间随机等一段时间再跳（第一跳用上面的单独设置）"
          >
            <Input
              type="number"
              min={0}
              value={form.relay_dwell_ms}
              onChange={(e) => set("relay_dwell_ms", Number(e.target.value))}
            />
          </Field>
          <Field
            label="每篇最多看多久再跳下一条 relay_dwell_max_ms"
            hint="毫秒；不大于「最少」时每篇固定等「最少」这么久"
          >
            <Input
              type="number"
              min={0}
              value={form.relay_dwell_max_ms}
              onChange={(e) =>
                set("relay_dwell_max_ms", Number(e.target.value))
              }
            />
          </Field>
          {/* 点空重点只对自动点击有意义：人工模式没有「点空」，看门狗也不重点 */}
          {autoClick && (
            <Field
              label="种子链接打不开的等待时间 relay_launch_timeout_seconds"
              hint="秒；点了种子链接后这么久还没打开任何文章，就当没点上，再点一次"
            >
              <Input
                type="number"
                min={0}
                value={form.relay_launch_timeout_seconds}
                onChange={(e) =>
                  set("relay_launch_timeout_seconds", Number(e.target.value))
                }
              />
            </Field>
          )}
          <Field
            label="卡住多久算没反应 relay_stall_seconds"
            hint={
              autoClick
                ? "秒；自动翻链接途中这么久没打开新文章就算卡住：正在打开的那条先排到队尾，再卡一次就跳过它，然后重新点种子链接继续"
                : "秒；自动翻链接途中这么久没打开新文章就算卡住"
            }
          >
            <Input
              type="number"
              min={0}
              value={form.relay_stall_seconds}
              onChange={(e) =>
                set("relay_stall_seconds", Number(e.target.value))
              }
            />
          </Field>
          {autoClick && (
            <>
              <Field
                label="最多重新点几次种子链接 relay_max_relaunch"
                hint="没点上 / 浏览器窗口被关这类没进展的情况下，重新点种子链接的次数上限，超过就停止本批"
              >
                <Input
                  type="number"
                  min={0}
                  value={form.relay_max_relaunch}
                  onChange={(e) =>
                    set("relay_max_relaunch", Number(e.target.value))
                  }
                />
              </Field>
              <SwitchRow
                title="拉起浏览器后再按一次 Ctrl+F5 硬刷新 rpa_hard_refresh"
                hint="默认关。种子页由本机直出且禁缓存、文章页经代理也已禁缓存，硬刷只会把第一条文章白白重载一次（多等约 2.5 秒）；仅在怀疑内置浏览器命中缓存、代理抓不到请求时临时打开对照"
                checked={form.rpa_hard_refresh}
                onCheckedChange={(v) => set("rpa_hard_refresh", v)}
              />
            </>
          )}
        </FieldGrid>
      </CollapsibleSection>

      {/* ── 列表采集与节流 ── */}
      <CollapsibleSection
        icon={<Gauge />}
        title="列表采集与节流"
        badge={<Badge variant="destructive">防封号关键</Badge>}
        description="getmsg 列表请求按登录的微信账号累计计数：近 24 小时累计约 206–224 次就会触发账号级限制约 1 天（ret=-6），与间隔、出口 IP 无关。这里的间隔与每号预算对所有路径（巡检 / 单号重采 / 历史抓取）统一生效，依据见「常见问题」页"
        summary={`间隔 ${fmtMsRange(form.list_gap_min_ms, form.list_gap_max_ms)} · 每号 24 小时 ${
          form.list_daily_budget ? `${form.list_daily_budget} 次` : "不限"
        } · 最多翻 ${form.list_max_pages} 页`}
        {...section("list")}
      >
        <FieldGrid>
          <Field
            label="列表请求间隔下限 list_gap_min_ms"
            hint="毫秒；任意两次 getmsg（跨号 / 跨页 / 巡检与历史抓取共用）之间在 [下限, 上限] 内随机等待。微信按微信号计频，默认 8000"
          >
            <Input
              type="number"
              min={0}
              step={1000}
              value={form.list_gap_min_ms}
              onChange={(e) => set("list_gap_min_ms", Number(e.target.value))}
            />
          </Field>
          <Field
            label="列表请求间隔上限 list_gap_max_ms"
            hint="毫秒；默认 20000。不大于下限即固定间隔"
          >
            <Input
              type="number"
              min={0}
              step={1000}
              value={form.list_gap_max_ms}
              onChange={(e) => set("list_gap_max_ms", Number(e.target.value))}
            />
          </Field>
          <Field
            label="每号列表请求预算（近 24 小时）list_daily_budget"
            hint="次；当前微信号近 24 小时 getmsg 上限（所有路径合计，滚动窗口、换号从 0 计）；达到后巡检与历史抓取都暂停，等最早一次请求滑出窗口。0 = 不限。默认 180：测试中三个账号分别在累计第 224 / 223 / 206 次触发账号级限制（ret=-6），取最低值再留一批的余量，不建议超过 190"
          >
            <Input
              type="number"
              min={0}
              step={50}
              value={form.list_daily_budget}
              onChange={(e) => set("list_daily_budget", Number(e.target.value))}
            />
          </Field>
          <Field
            label="单号最多翻页数 list_max_pages"
            hint="安全阀：最后更新时间太早时防止无止境翻页"
          >
            <Input
              type="number"
              min={1}
              value={form.list_max_pages}
              onChange={(e) => set("list_max_pages", Number(e.target.value))}
            />
          </Field>
          <Field
            label="列表翻页随机等待下限 page_sleep_min_ms"
            hint="毫秒；有「最后更新时间」时两页 getmsg 之间在 [下限, 上限] 内随机等待（与全局间隔叠加）"
          >
            <Input
              type="number"
              min={0}
              step={500}
              value={form.page_sleep_min_ms}
              onChange={(e) => set("page_sleep_min_ms", Number(e.target.value))}
            />
          </Field>
          <Field
            label="列表翻页随机等待上限 page_sleep_max_ms"
            hint="毫秒；不大于下限即固定等待"
          >
            <Input
              type="number"
              min={0}
              step={500}
              value={form.page_sleep_max_ms}
              onChange={(e) => set("page_sleep_max_ms", Number(e.target.value))}
            />
          </Field>
        </FieldGrid>
      </CollapsibleSection>

      {/* ── 凭证续期 ── */}
      <CollapsibleSection
        icon={<RefreshCw />}
        title="凭证续期"
        description="翻页途中凭证过期（ret=-3）时，把过期的号集中起来在微信浏览器里重新打开一遍换新凭证；这里设置最多试几轮、每轮等多久"
        summary={`最多 ${form.cred_refresh_attempts} 轮 · 每轮等 ${form.cred_refresh_wait_seconds}s`}
        {...section("cred")}
      >
        <FieldGrid>
          <Field
            label="凭证续期重试轮数 cred_refresh_attempts"
            hint="翻页途中凭证过期 → 过期的号集中在微信浏览器里重新打开换新凭证；内置浏览器常有打开了但没加载的情况，至少 3 轮"
          >
            <Input
              type="number"
              min={3}
              value={form.cred_refresh_attempts}
              onChange={(e) =>
                set("cred_refresh_attempts", Number(e.target.value))
              }
            />
          </Field>
          <Field
            label="凭证续期每轮等待 cred_refresh_wait_seconds"
            hint="秒；等各号新 key 到位的上限，超时进入下一轮"
          >
            <Input
              type="number"
              min={0}
              value={form.cred_refresh_wait_seconds}
              onChange={(e) =>
                set("cred_refresh_wait_seconds", Number(e.target.value))
              }
            />
          </Field>
        </FieldGrid>
      </CollapsibleSection>

      {/* ── 定时巡检 ── */}
      <CollapsibleSection
        icon={<Radar />}
        title="定时巡检"
        description="控制面板「开始巡检」后按批自动更新库里全部公众号的最新文章列表；每批用微信内置浏览器打开各号最新一篇文章换凭证，再按上次记下的最新发布时间翻页。一轮跑完停留一段时间再来"
        summary={`每批 ${form.sweep_batch_size} 号 · 轮间 ${fmtDur(form.sweep_idle_seconds)} · 批距 ${fmtDur(
          form.sweep_batch_gap_seconds,
        )}`}
        {...section("sweep")}
      >
        <FieldGrid>
          <Field
            label="巡检轮间停留 sweep_idle_seconds"
            hint="秒；一轮（全库）跑完后停留多久再开始下一轮，默认 3600 = 1 小时"
          >
            <Input
              type="number"
              min={60}
              step={60}
              value={form.sweep_idle_seconds}
              onChange={(e) =>
                set("sweep_idle_seconds", Number(e.target.value))
              }
            />
          </Field>
          <Field
            label="巡检每批号数 sweep_batch_size"
            hint="1~100；一批在微信浏览器里打开链接换凭证 + 采集要在凭证 30 分钟有效期内跑完，默认 20"
          >
            <Input
              type="number"
              min={1}
              max={100}
              step={1}
              value={form.sweep_batch_size}
              onChange={(e) => set("sweep_batch_size", Number(e.target.value))}
            />
          </Field>
          <Field
            label="巡检批次间隔 sweep_batch_gap_seconds"
            hint="秒；两个批次之间至少隔多久，默认 60"
          >
            <Input
              type="number"
              min={0}
              step={10}
              value={form.sweep_batch_gap_seconds}
              onChange={(e) =>
                set("sweep_batch_gap_seconds", Number(e.target.value))
              }
            />
          </Field>
          <Field
            label="整批失败暂停 sweep_fail_pause_seconds"
            hint="秒；一批号无一成功且有号待重试时，整个巡检先停这么久再继续（防止被限后越打越密），默认 900"
          >
            <Input
              type="number"
              min={0}
              step={60}
              value={form.sweep_fail_pause_seconds}
              onChange={(e) =>
                set("sweep_fail_pause_seconds", Number(e.target.value))
              }
            />
          </Field>
        </FieldGrid>
      </CollapsibleSection>

      {/* ── 历史文章抓取 ── */}
      <CollapsibleSection
        icon={<History />}
        title="历史文章抓取"
        description="「公众号列表 → 抓取历史」按号从最新一页往旧翻，一次只抓一个号，且与巡检不能同时跑；页与页之间按这里的间隔等待，与巡检共用当前微信号的 24 小时预算"
        summary={`每页 ${form.history_page_count} 条 · 页间隔 ${fmtDur(form.history_gap_seconds)} · 为巡检保留 ${form.history_budget_reserve} 次`}
        {...section("history")}
      >
        <FieldGrid>
          <Field
            label="每页最多条数 history_page_count"
            hint="1~50；每次 getmsg 请求的条数上限。微信服务端通常按 10 封顶，填大了也只回 10，默认 10"
          >
            <Input
              type="number"
              min={1}
              max={50}
              step={1}
              value={form.history_page_count}
              onChange={(e) => set("history_page_count", Number(e.target.value))}
            />
          </Field>
          <Field
            label="页间隔 history_gap_seconds"
            hint={`秒；两页之间至少等多久（间隔是下限：巡检批次持锁时下一页会顺延）。不小于列表闸门上限 ${Math.ceil(
              (Number(form.list_gap_max_ms) || 0) / 1000,
            )} 秒，保存时自动抬到该值，默认 60`}
          >
            <Input
              type="number"
              min={0}
              step={10}
              value={form.history_gap_seconds}
              onChange={(e) => set("history_gap_seconds", Number(e.target.value))}
            />
          </Field>
          <Field
            label="为巡检保留的预算 history_budget_reserve"
            hint="次；历史任务不吃掉当前微信号 24 小时预算的最后这么多次，留给定时巡检。达到时历史任务自动暂停、额度腾出后继续，默认 30"
          >
            <Input
              type="number"
              min={0}
              step={10}
              value={form.history_budget_reserve}
              onChange={(e) => set("history_budget_reserve", Number(e.target.value))}
            />
          </Field>
        </FieldGrid>
      </CollapsibleSection>

      {/* ── 正文补采 ── */}
      <CollapsibleSection
        icon={<FileText />}
        title="正文补采"
        description="「补详情」走公开 /s 页抓正文的节奏：间隔按每个 worker 各自计，整体速率 ≈ 并发数 ÷ 间隔"
        summary={`间隔 ${form.detail_throttle_ms}ms · 并发 ${form.detail_workers}`}
        {...section("detail")}
      >
        <FieldGrid>
          <Field
            label="详情抓取间隔 detail_throttle_ms"
            hint="毫秒/请求（每个并发 worker 各自计）。被微信限流（验证页）后请调大再试"
          >
            <Input
              type="number"
              min={0}
              step={500}
              value={form.detail_throttle_ms}
              onChange={(e) =>
                set("detail_throttle_ms", Number(e.target.value))
              }
            />
          </Field>
          <Field
            label="详情并发数 detail_workers"
            hint="1~12。整体速率 ≈ 并发数 ÷ 间隔；1 = 严格串行最稳，被限流请保持 1"
          >
            <Input
              type="number"
              min={1}
              max={12}
              step={1}
              value={form.detail_workers}
              onChange={(e) => set("detail_workers", Number(e.target.value))}
            />
          </Field>
        </FieldGrid>
      </CollapsibleSection>

      {/* ── sticky 保存条 ── */}
      <div className="sticky bottom-0 z-10 flex items-center justify-between gap-3 rounded-xl border border-border bg-card/95 px-4 py-3 shadow-sm backdrop-blur">
        <div className="flex items-center gap-2 text-xs">
          {anyDirty ? (
            <>
              <span className="size-2 shrink-0 rounded-full bg-warning" />
              <span>
                有未保存的修改
                <span className="ml-1 text-xs text-muted-foreground">
                  （{dirtyKeys.size} 项）
                </span>
              </span>
            </>
          ) : (
            <>
              <span className="size-2 shrink-0 rounded-full bg-success" />
              <span className="text-muted-foreground">已是最新</span>
            </>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => void onExportFile()}
            disabled={exporting !== ""}
            title="把已保存的配置写成 JSON 文件（含上报 token / 飞书密钥，注意保管）"
          >
            <Download />
            {exporting === "file" ? "导出中…" : "导出文件"}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => void onExportClipboard()}
            disabled={exporting !== ""}
            title="把已保存的配置以 JSON 文本复制到剪贴板（含上报 token / 飞书密钥，注意保管）"
          >
            <Copy />
            {exporting === "clipboard" ? "复制中…" : "复制配置"}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              setImportError(null);
              setImportOpen(true);
            }}
            title="从导出的 JSON 文件或粘贴的文本导入配置，先填入表单，确认后再保存"
          >
            <Upload />
            导入
          </Button>
          <Button variant="outline" size="sm" onClick={onReset}>
            <RotateCcw />
            恢复默认
          </Button>
          <Button size="sm" onClick={onSave} disabled={!anyDirty}>
            保存配置
          </Button>
        </div>
      </div>

      {/* ── 导入配置对话框 ── */}
      <Dialog
        open={importOpen}
        onOpenChange={(open) => {
          setImportOpen(open);
          if (!open) setImportError(null);
        }}
      >
        <DialogContent className="max-w-xl">
          <DialogHeader>
            <DialogTitle className="text-sm">导入配置</DialogTitle>
            <DialogDescription>
              选一个由「导出文件」生成的 JSON
              文件，或把「复制配置」得到的文本粘贴进来。
              导入只填入表单，不会自动保存；识别不出的键会忽略并提示。
            </DialogDescription>
          </DialogHeader>
          <div className="flex flex-col gap-3">
            <div className="flex items-center gap-2">
              <input
                ref={fileInputRef}
                type="file"
                accept=".json,application/json,text/plain"
                className="hidden"
                onChange={(e) => void onImportFile(e.target.files?.[0])}
              />
              <Button
                variant="outline"
                size="sm"
                onClick={() => fileInputRef.current?.click()}
              >
                <Upload />
                选择文件…
              </Button>
              <span className="text-xs text-muted-foreground">
                或在下面粘贴文本
              </span>
            </div>
            <textarea
              className="min-h-[220px] w-full resize-y rounded-md border border-input bg-background px-3 py-2 font-mono text-xs leading-relaxed outline-none focus-visible:ring-2 focus-visible:ring-ring"
              placeholder={
                '{\n  "kind": "mpider-config",\n  "config": { ... }\n}'
              }
              value={importText}
              onChange={(e) => {
                setImportText(e.target.value);
                if (importError) setImportError(null);
              }}
              spellCheck={false}
            />
            {importError && (
              <div className="text-sm text-destructive">
                导入失败：{importError}
              </div>
            )}
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setImportOpen(false)}
            >
              取消
            </Button>
            <Button
              size="sm"
              onClick={() => applyImport(importText)}
              disabled={importText.trim() === ""}
            >
              导入到表单
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

// ============ 页面 ============

export function Settings() {
  const { refreshCert } = useApp();
  const openState = useSectionOpenState<SectionId>(
    OPEN_STATE_KEY,
    OPEN_DEFAULTS,
  );

  // 进入系统设置即实时复查证书。
  useEffect(() => {
    void refreshCert();
  }, [refreshCert]);

  return (
    <div>
      <div className="mb-3 flex items-end justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">系统设置</h1>
          <p className="mt-0.5 text-xs text-muted-foreground">
            证书安装与抓取运行参数，按模块折叠；改完在底部「保存配置」；底部还能导出
            / 复制 / 导入配置
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => openState.setAll(true)}
          >
            <ChevronsUpDown />
            全部展开
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => openState.setAll(false)}
          >
            <ChevronsDownUp />
            全部收起
          </Button>
        </div>
      </div>
      <div className="space-y-3">
        <CertWizard openState={openState} />
        <RunConfigForm openState={openState} />
      </div>
    </div>
  );
}
