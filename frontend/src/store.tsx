import * as React from "react";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { msg } from "@/lib/utils";
import {
  articleCounts,
  certStatus,
  coreHealth,
  notifyApply,
  seedServerApply,
  sysproxyResetStale,
  installCert as invokeInstallCert,
  uninstallCert as invokeUninstallCert,
  loadCfg,
  persistCfg,
  rpaPickBegin as invokeRpaPickBegin,
  rpaPickCancel as invokeRpaPickCancel,
  rpaTestClick as invokeRpaTestClick,
  type SeedMark,
  type TestClick,
  rpaProxyProbe,
  rpaSelfCheck,
  sweepStart as invokeSweepStart,
  sweepStop as invokeSweepStop,
  loopStatus,
  runDetail as invokeRunDetail,
  systemStatus,
  historyStatus as invokeHistoryStatus,
  alertLatest as invokeAlertLatest,
  alertAck as invokeAlertAck,
  type LogEvent,
  type LogStage,
  type ArticleCounts,
  type CertInfo,
  type DetailProgress,
  type DetailSummary,
  type HealthInfo,
  type HistoryStatus,
  type Alert,
  type InterceptProbe,
  type RealRunConfig,
  type SelfCheck,
  type SystemStatus,
} from "@/lib/tauri";

/**
 * 全局应用 store —— 一切跨视图共享的状态与生命周期集中在这里，替代旧 main.ts 顶层的全局变量。
 *
 * 【证书态：单一数据源（务必保留这套设计）】
 * - `certInstalled: boolean | null`（null=检查中 / true=已装 / false=未装）是唯一真相；
 * - 任何地方只经 `setCertInstalled` 修改它；控制面板告警条 / 底部状态栏证书项 / 向导徽章
 *   三处都**读同一个** `certInstalled`，天然同源原子刷新，绝不各查各的；
 * - 权威来源＝`system_status.cert_installed`（定时轮询，平时 6s / 任务活跃 2s）+ `install_cert` 成功后乐观置 true；
 * - `cert_status` 只用于填静态明细（路径 / 命令 / can_auto），不作为 installed 的独立真相。
 */

/** 证书静态明细（路径 / 手动命令 / 是否可自动装）——只读展示，不参与 installed 判定。 */
interface CertDetail {
  ca_path: string;
  manual_command: string;
  can_auto: boolean;
}

interface AppStore {
  // 证书态（单一数据源）
  certInstalled: boolean | null;
  certDetail: CertDetail;
  certLog: string[];
  installing: boolean;
  /** 移除证书进行中（与 installing 互斥，按钮各自 loading）。 */
  uninstalling: boolean;
  refreshCert: () => Promise<void>;
  installCert: () => Promise<void>;
  /** 从系统信任库移除本工具证书（证书文件保留，可再装）。 */
  uninstallCert: () => Promise<void>;

  // 健康信息 / 系统状态
  health: HealthInfo | null;
  healthError: string | null;
  refreshHealth: () => Promise<void>;
  system: SystemStatus | null;
  refreshStatus: () => Promise<SystemStatus | null>;
  /**
   * 本平台能否自动点开种子链接（后端 `system_status.rpa_auto`）。false = 人工模式：运行时全局提醒用户去微信
   * 打开一篇文章即可，所有种子链接 / 框选标定 / 自检 / 测试点击 UI 按平台**直接隐藏**，不显示「不支持」。
   * 状态还没拉到时按 UA 猜（Windows 为真），避免 Windows 首屏闪一下空白。
   */
  autoClick: boolean;
  /** 是否 macOS（系统代理走 networksetup，「网络服务名」只在 mac 有意义）。 */
  isMac: boolean;

  // 定时巡检开关（后端主循环巡检模式；历史抓取由公众号列表单独拉起）
  cfg: RealRunConfig;
  saveCfg: (next: RealRunConfig) => void;
  /** 巡检开关是否打开（后端 `loop_status.sweep_on` 的镜像，含「停止中」；StatusBar/Dashboard 据此显示与置灰）。 */
  running: boolean;
  /** 已点「停止巡检」、后端还在收尾当前批次。 */
  stopping: boolean;
  /** 后端主循环在跑（巡检模式或历史模式）。 */
  loopRunning: boolean;
  /** 启停请求在途（用于按钮置灰防连点）。 */
  pollBusy: boolean;
  reportError: string | null;
  startSweep: () => Promise<void>;
  stopSweep: () => Promise<void>;

  // 文章计数（控制面板状态卡）
  counts: ArticleCounts | null;
  refreshCounts: () => Promise<void>;

  // 补采文章详情（全局/单号/单篇共用一个任务通道，后端互斥）
  detailRunning: boolean;
  detailProgress: DetailProgress | null;
  detailError: string | null;
  runDetail: (
    biz?: string,
    articleId?: number,
  ) => Promise<DetailSummary | null>;

  // RPA 自检 / 框选种子标定（启动即自动自检；横线 step 呈现）
  rpaCheck: SelfCheck | null;
  rpaChecking: boolean;
  refreshRpaCheck: () => Promise<void>;
  /** 框选层是否打开（等用户框选或取消）。 */
  rpaPicking: boolean;
  /** 最近一次框选的结论（成功提示 / 失败原因）。 */
  rpaMarkMsg: string | null;
  /** 最近一次成功的框选结果。 */
  rpaMark: SeedMark | null;
  /** 框选种子；`wxId` 指定标定落到哪个微信号（默认激活号）。 */
  beginSeedPick: (wxId?: number) => Promise<void>;
  cancelSeedPick: () => Promise<void>;
  // 测试点击（标定后验证：置顶 → 按标定点位点 → 看是否拉起内置浏览器）
  rpaTestClicking: boolean;
  rpaTestClickResult: TestClick | null;
  /** 测试点击；`wxId` 指定按哪个微信号的标定点（默认激活号）。 */
  testClickRpa: (wxId?: number) => Promise<void>;
  // 拦截探针（自检时真设一次代理→验证拦截→复位）
  proxyProbe: InterceptProbe | null;
  proxyProbing: boolean;
  refreshProxyProbe: () => Promise<void>;

  // 「公众号文章」页的公众号过滤（供「公众号列表 → 文章列表」带参跳转）
  articlesBiz: string | null;
  setArticlesBiz: (biz: string | null) => void;

  // 环节日志（`agent://log` 实时流；日志页可用入库历史替换整个列表）
  logs: LogEvent[];
  /** 清空视图（不动库）。 */
  clearLogs: () => void;
  /** 用一批事件替换视图（日志页「加载历史」把入库行转成事件后调用；之后实时事件继续追加）。 */
  replaceLogs: (items: LogEvent[]) => void;
  /** 前端本地产生的一行提示（不入库，如框选结果）。 */
  appendLocalLog: (stage: LogStage, message: string) => void;

  // —— 历史文章抓取 + 全局提醒（每 2 秒共享轮询；状态栏 / 公众号列表 / 横幅都读这里）——
  /** 当前活动的历史任务与当前微信号预算（轮询未返回前 null）。 */
  history: HistoryStatus | null;
  /** 立即重拉一次历史状态（操作后调用，别等下一轮）。 */
  refreshHistory: () => Promise<void>;
  /** 最近一条未确认的全局提醒。 */
  alert: Alert | null;
  /** 「知道了」：确认并清掉当前提醒。 */
  ackAlert: () => Promise<void>;
}

const AppContext = createContext<AppStore | null>(null);

/** 日志视图最多保留的行数（超出丢最早的；完整历史在 SQLite `app_log`，日志页可加载）。 */
const MAX_LOG_LINES = 3000;

/** 追加一条日志到视图：同一 seq 只保留一条（StrictMode 双订阅 / 重复事件防抖），并截断到上限。 */
function appendLog(prev: LogEvent[], ev: LogEvent): LogEvent[] {
  const last = prev[prev.length - 1];
  if (last && last.seq === ev.seq && last.transient === ev.transient)
    return prev;
  const next =
    prev.length >= MAX_LOG_LINES
      ? prev.slice(prev.length - MAX_LOG_LINES + 1)
      : prev.slice();
  next.push(ev);
  return next;
}

const DEFAULT_CERT_DETAIL: CertDetail = {
  ca_path: "",
  manual_command: "",
  can_auto: false,
};

export function AppProvider({ children }: { children: React.ReactNode }) {
  // ---- 证书态：单一数据源 ----
  const [certInstalled, setCertInstalled] = useState<boolean | null>(null);
  const [certDetail, setCertDetail] = useState<CertDetail>(DEFAULT_CERT_DETAIL);
  const [certLog, setCertLog] = useState<string[]>([]);
  const [installing, setInstalling] = useState(false);
  const [uninstalling, setUninstalling] = useState(false);

  // ---- 健康 / 系统状态 ----
  const [health, setHealth] = useState<HealthInfo | null>(null);
  const [healthError, setHealthError] = useState<string | null>(null);
  const [system, setSystem] = useState<SystemStatus | null>(null);
  const uaWindows = /Windows/i.test(navigator.userAgent);
  const uaMac = /Mac OS|Macintosh/i.test(navigator.userAgent);
  const autoClick = system ? system.rpa_auto : uaWindows;
  const isMac = system ? system.platform === "macos" : uaMac;

  // ---- 运行配置 / 运行一次 ----
  const [cfg, setCfg] = useState<RealRunConfig>(() => loadCfg());
  const cfgRef = useRef(cfg);
  useEffect(() => {
    cfgRef.current = cfg;
  }, [cfg]);

  const [running, setRunning] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [loopRunning, setLoopRunning] = useState(false);
  const runningRef = useRef(false);
  // 启停请求在途标志：驱动按钮置灰防连点，并配合 ref 做同步防抖。
  const [pollBusy, setPollBusy] = useState(false);
  const pollBusyRef = useRef(false);
  const [reportError, setReportError] = useState<string | null>(null);

  // ---- 文章计数 / 补详情任务 ----
  const [counts, setCounts] = useState<ArticleCounts | null>(null);
  const [detailRunning, setDetailRunning] = useState(false);
  const detailRunningRef = useRef(false);
  const [detailProgress, setDetailProgress] = useState<DetailProgress | null>(
    null,
  );
  const [detailError, setDetailError] = useState<string | null>(null);

  // ---- RPA 自检 / 框选标定 ----
  const [rpaCheck, setRpaCheck] = useState<SelfCheck | null>(null);
  const [rpaChecking, setRpaChecking] = useState(false);
  const [rpaPicking, setRpaPicking] = useState(false);
  const [rpaMarkMsg, setRpaMarkMsg] = useState<string | null>(null);
  const [rpaMark, setRpaMark] = useState<SeedMark | null>(null);
  const [rpaTestClicking, setRpaTestClicking] = useState(false);
  const [rpaTestClickResult, setRpaTestClickResult] =
    useState<TestClick | null>(null);
  const [proxyProbe, setProxyProbe] = useState<InterceptProbe | null>(null);
  const [proxyProbing, setProxyProbing] = useState(false);

  // ---- 「公众号文章」页过滤 ----
  const [articlesBiz, setArticlesBiz] = useState<string | null>(null);

  // ---- 环节日志（实时流，视图最多保留 MAX_LOG_LINES 条，早的丢弃；完整历史在库里）----
  const [logs, setLogs] = useState<LogEvent[]>([]);

  // —— 历史文章抓取 + 全局提醒 ——
  const [history, setHistory] = useState<HistoryStatus | null>(null);
  const [alert, setAlert] = useState<Alert | null>(null);
  // 已 toast 过的提醒 id：同一条提醒轮询到多次只弹一次。
  const toastedAlertRef = useRef<number | null>(null);

  const refreshHistory = useCallback(async () => {
    try {
      setHistory(await invokeHistoryStatus(cfgRef.current));
    } catch {
      // 后端未就绪 / 命令失败：保留上一次状态，等下一轮。
    }
  }, []);

  const refreshAlert = useCallback(async () => {
    try {
      const a = await invokeAlertLatest();
      setAlert(a);
      if (a && toastedAlertRef.current !== a.id) {
        toastedAlertRef.current = a.id;
        const title =
          a.kind === "budget"
            ? "列表请求预算已达上限"
            : a.kind === "blocked"
              ? "微信号被限制"
              : "历史抓取已暂停";
        toast.warning(title, { description: a.message, duration: 8000 });
      }
    } catch {
      // 同上
    }
  }, []);

  const ackAlert = useCallback(async () => {
    const a = alert;
    if (!a) return;
    try {
      await invokeAlertAck(a.id);
      setAlert(null);
    } catch (e) {
      toast.error("确认提醒失败", { description: msg(e) });
    }
  }, [alert]);

  useEffect(() => {
    void refreshHistory();
    void refreshAlert();
    const id = window.setInterval(() => {
      void refreshHistory();
      void refreshAlert();
    }, 2000);
    return () => window.clearInterval(id);
  }, [refreshHistory, refreshAlert]);
  const localSeq = useRef(0);

  /** 用 cert_status 的静态明细（路径/命令/can_auto）填 UI；installed 布尔经单一 setter 走。 */
  const applyCertInfo = useCallback((c: CertInfo) => {
    setCertInstalled(c.installed);
    setCertDetail({
      ca_path: c.ca_path,
      manual_command: c.manual_command,
      can_auto: c.can_auto,
    });
  }, []);

  const refreshCert = useCallback(async () => {
    try {
      applyCertInfo(await certStatus());
    } catch (e) {
      console.error("cert_status", msg(e));
    }
  }, [applyCertInfo]);

  const refreshHealth = useCallback(async () => {
    try {
      const h = await coreHealth();
      setHealth(h);
      setHealthError(null);
    } catch (e) {
      setHealthError("core_health 失败：" + msg(e));
    }
  }, []);

  /** 刷新系统状态；返回本次拿到的状态（失败为 null），供启动流程按平台决定后续步骤。 */
  const refreshStatus = useCallback(async (): Promise<SystemStatus | null> => {
    try {
      const s = await systemStatus(cfgRef.current.sysproxy_service);
      setSystem(s);
      // 证书态：system_status 是 6s 轮询的权威来源，统一经单一 setter。
      setCertInstalled(s.cert_installed);
      return s;
    } catch {
      // 查询失败不篡改证书态（保持单一数据源），等下一轮轮询自愈。
      return null;
    }
  }, []);

  const installCert = useCallback(async () => {
    setInstalling(true);
    setCertLog(["开始安装…系统可能弹出密码框，请授权。"]);
    try {
      const out = await invokeInstallCert();
      setCertLog(out.log);
      // 乐观隐藏：安装命令成功（out.ok）时，即便钥匙串还没索引到新信任
      // （out.info.installed 可能暂为 false），也按已安装呈现，消除告警条卡住的竞态。
      applyCertInfo(out.ok ? { ...out.info, installed: true } : out.info);
      void refreshStatus();
      // 兜底：稍后再实查一次，让状态与钥匙串最终一致。
      if (out.ok) {
        toast.success("证书已安装到系统信任库");
        setTimeout(() => {
          void refreshCert();
          void refreshStatus();
        }, 800);
      } else {
        toast.error("证书安装未成功", {
          description: "详情见下方安装日志，可尝试手动安装",
        });
      }
    } catch (e) {
      setCertLog(["安装失败：" + msg(e)]);
      toast.error("证书安装失败", { description: msg(e) });
    } finally {
      setInstalling(false);
    }
  }, [applyCertInfo, refreshCert, refreshStatus]);

  const uninstallCert = useCallback(async () => {
    setUninstalling(true);
    setCertLog(["开始移除…系统可能弹出密码框，请授权。"]);
    try {
      const out = await invokeUninstallCert();
      setCertLog(out.log);
      // 后端 installed 是删完后实查的结果，直接采信；再让状态栏同步一次。
      applyCertInfo(out.info);
      void refreshStatus();
      if (out.info.installed) {
        toast.error("证书未能移除", {
          description: "详情见下方日志，可到系统钥匙串里手动删除",
        });
      } else {
        toast.success("证书已从系统信任库移除");
      }
      setTimeout(() => {
        void refreshCert();
        void refreshStatus();
      }, 800);
    } catch (e) {
      setCertLog(["移除失败：" + msg(e)]);
      toast.error("证书移除失败", { description: msg(e) });
    } finally {
      setUninstalling(false);
    }
  }, [applyCertInfo, refreshCert, refreshStatus]);

  const refreshCounts = useCallback(async () => {
    try {
      setCounts(await articleCounts());
    } catch (e) {
      console.error("article_counts", msg(e));
    }
  }, []);

  /** RPA 自检：查环境就绪度（文件助手窗口 / 种子点位标定状态）。启动即自动跑，也可手动重跑。 */
  const refreshRpaCheck = useCallback(async () => {
    setRpaChecking(true);
    try {
      setRpaCheck(await rpaSelfCheck());
    } catch (e) {
      // 命令层异常（非「不支持」）：造一个失败态展示，不打断其它功能。
      setRpaCheck({
        ready: false,
        fh_found: false,
        fh_rect: null,
        screen: null,
        scale_pct: null,
        mark: "none",
        cached_point: null,
        cached_rect: null,
        message: "自检调用失败：" + msg(e),
      });
    } finally {
      setRpaChecking(false);
    }
  }, []);

  /**
   * 框选种子：置顶文件助手并弹出全屏框选层，之后等框选层提交（`rpa://seed-marked`）或取消
   * （`rpa://seed-pick-cancelled`）事件复位；两个事件在下方挂载 effect 里订阅。
   */
  const beginSeedPick = useCallback(async (wxId?: number) => {
    setRpaMarkMsg(null);
    setRpaPicking(true);
    try {
      await invokeRpaPickBegin(wxId);
    } catch (e) {
      setRpaPicking(false);
      setRpaMarkMsg("无法开始框选：" + msg(e));
      toast.error("无法开始框选", { description: msg(e) });
    }
  }, []);

  /** 取消框选：关掉框选层（主窗口侧的「取消」按钮；框选层内按 Esc 效果相同）。 */
  const cancelSeedPick = useCallback(async () => {
    try {
      await invokeRpaPickCancel();
    } catch (e) {
      console.error("rpa_pick_cancel", msg(e));
    } finally {
      setRpaPicking(false);
    }
  }, []);

  /**
   * 测试点击：不发种子，只「置顶文件助手 → 按标定点位点一次 → 等浏览器」。
   * 会真的动鼠标并拉起微信内置浏览器（窗口留着不关），结果回显。
   */
  const testClickRpa = useCallback(async (wxId?: number) => {
    setRpaTestClicking(true);
    setRpaTestClickResult(null);
    try {
      const r = await invokeRpaTestClick(wxId);
      setRpaTestClickResult(r);
      if (r.ok) {
        toast.success("测试点击成功：已拉起微信内置浏览器");
      } else {
        toast.warning("测试点击未拉起浏览器", { description: r.message });
      }
    } catch (e) {
      toast.error("测试点击调用失败", { description: msg(e) });
      setRpaTestClickResult({
        ok: false,
        fh_found: false,
        fh_rect: null,
        screen: null,
        scale_pct: null,
        point: null,
        clicked: false,
        opened: [],
        message: "测试点击调用失败：" + msg(e),
      });
    } finally {
      setRpaTestClicking(false);
    }
  }, []);

  /** 拦截探针：真设一次系统代理→经 MITM 验证拦截→复位。会短暂改动系统代理。 */
  const refreshProxyProbe = useCallback(async () => {
    setProxyProbing(true);
    try {
      setProxyProbe(await rpaProxyProbe(cfgRef.current.sysproxy_service));
    } catch (e) {
      setProxyProbe({
        proxy_set_ok: false,
        intercept_ok: false,
        message: "拦截探针失败：" + msg(e),
      });
    } finally {
      setProxyProbing(false);
    }
  }, []);

  /** 补采详情：不传 biz = 全局；biz 限单号；articleId 限单篇。后端互斥，重复触发会报错。 */
  const runDetail = useCallback(
    async (biz?: string, articleId?: number): Promise<DetailSummary | null> => {
      if (detailRunningRef.current) return null;
      detailRunningRef.current = true;
      setDetailRunning(true);
      setDetailError(null);
      setDetailProgress(null);
      try {
        // 间隔/并发从系统设置读（detail_throttle_ms / detail_workers）；限流时调大间隔、调小并发再试。
        const s = await invokeRunDetail(
          biz,
          articleId,
          undefined,
          cfgRef.current.detail_throttle_ms,
          cfgRef.current.detail_workers,
        );
        void refreshCounts();
        // 结果回显：没有候选 / 全部成功 / 有失败或撞限流（后端 feedback 是人类可读的补充说明）。
        if (s.candidates === 0) {
          toast.info("没有需要补采详情的文章");
        } else if (s.rate_limited) {
          toast.warning(`补采被限流中止：成功 ${s.done}，失败 ${s.failed}`, {
            description:
              s.feedback || "请调大「详情抓取间隔」、把并发数保持 1 后再试",
          });
        } else if (s.failed > 0) {
          toast.warning(
            `补采完成：成功 ${s.done}，失败 ${s.failed}，跳过 ${s.skipped}`,
            {
              description: s.feedback || undefined,
            },
          );
        } else {
          toast.success(
            `补采完成：成功 ${s.done} 篇${s.skipped > 0 ? `，跳过 ${s.skipped}` : ""}`,
          );
        }
        return s;
      } catch (e) {
        setDetailError("补采失败：" + msg(e));
        toast.error("补采详情失败", { description: msg(e) });
        return null;
      } finally {
        detailRunningRef.current = false;
        setDetailRunning(false);
      }
    },
    [refreshCounts],
  );

  const saveCfg = useCallback((next: RealRunConfig) => {
    setCfg(next);
    persistCfg(next);
    // 飞书通知 / 常驻种子入口服务按新配置即时生效（幂等；失败不影响保存，
    // 种子服务起不来的原因在「种子链接」卡里展示）。
    void notifyApply(next).catch(() => {});
    void seedServerApply(next).catch(() => {});
  }, []);

  // 应用启动即应用飞书通知配置（只在出问题时推预警，启动本身不推）。
  // 种子入口服务同样应用启动即常驻（没任务时只给说明页），文件助手里那条链接随时能点开。
  useEffect(() => {
    void notifyApply(cfgRef.current).catch(() => {});
    void seedServerApply(cfgRef.current).catch(() => {});
  }, []);

  const clearLogs = useCallback(() => setLogs([]), []);
  const replaceLogs = useCallback(
    (items: LogEvent[]) => setLogs(items.slice(-MAX_LOG_LINES)),
    [],
  );
  const appendLocalLog = useCallback((stage: LogStage, message: string) => {
    localSeq.current -= 1;
    const ev: LogEvent = {
      seq: localSeq.current,
      ts: Date.now() / 1000,
      level: "info",
      stage,
      job_id: null,
      message,
      transient: true,
    };
    setLogs((prev) => appendLog(prev, ev));
  }, []);

  /** 同步后端主循环 / 巡检开关状态（启动回填、3 秒轮询、启停后立即刷一次）。 */
  const refreshLoop = useCallback(async () => {
    try {
      const st = await loopStatus();
      runningRef.current = st.sweep_on;
      setRunning(st.sweep_on);
      setStopping(st.stopping);
      setLoopRunning(st.loop_running);
    } catch {
      /* 后端未就绪：保持现状 */
    }
  }, []);

  /** 开始巡检：后端长驻主循环按批巡检全库公众号。各环节日志经 agent://log 事件流入 logs。
   *  有活动的历史抓取任务时后端会拒绝（提示先取消）。幂等：已在跑则忽略。 */
  const startSweep = useCallback(async () => {
    if (runningRef.current || pollBusyRef.current) return;
    pollBusyRef.current = true;
    setPollBusy(true);
    setReportError(null);
    try {
      await invokeSweepStart(cfgRef.current);
      runningRef.current = true;
      setRunning(true);
      setStopping(false);
      setLoopRunning(true);
      void refreshStatus();
      toast.success("巡检已开始", {
        description: "按批打开各号最新一篇换凭证、采最新文章列表；一轮跑完停留后再来",
      });
    } catch (e) {
      // 以后端真实态为准（前端刷新 / HMR 丢了按钮态时，真在跑就不报红）。
      await refreshLoop();
      if (runningRef.current) {
        toast.info("巡检已在进行中");
      } else {
        setReportError("开始巡检失败：" + msg(e));
        toast.error("开始巡检失败", { description: msg(e) });
      }
    } finally {
      pollBusyRef.current = false;
      setPollBusy(false);
    }
  }, [refreshStatus, refreshLoop]);

  /** 停止巡检：发停止信号；后端当前批次跑完后退出，期间按钮显示「停止中」（据 loop_status.stopping）。 */
  const stopSweep = useCallback(async () => {
    if (!runningRef.current || pollBusyRef.current) return;
    pollBusyRef.current = true;
    setPollBusy(true);
    try {
      await invokeSweepStop();
      setStopping(true);
      toast.success("已发出停止信号", {
        description: "正在处理的批次会先跑完再停止；停完后才能抓取历史文章",
      });
    } catch (e) {
      setReportError("停止巡检失败：" + msg(e));
      toast.error("停止巡检失败", { description: msg(e) });
    } finally {
      pollBusyRef.current = false;
      setPollBusy(false);
      void refreshLoop();
      void refreshHealth();
      void refreshStatus();
      void refreshCounts();
    }
  }, [refreshLoop, refreshHealth, refreshStatus, refreshCounts]);

  // 一次性挂载：事件订阅 + 首拉 + 窗口聚焦自愈。对齐旧 main.ts 顶层逻辑（轮询在下个 effect）。
  useEffect(() => {
    // listen() 是异步注册：StrictMode 开发模式下 effect 会立即卸载再重挂，
    // 卸载时 Promise 可能还没 resolve —— 旧写法拿不到 unlisten，产生「僵尸监听器」，
    // 每条事件被处理两次（日志双打）。用 disposed 标记：resolve 时若已卸载则立刻退订。
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    const track = (p: Promise<() => void>) => {
      void p.then((f) => {
        if (disposed) f();
        else unlisteners.push(f);
      });
    };

    track(
      listen<LogEvent>("agent://log", (e) => {
        setLogs((prev) => appendLog(prev, e.payload));
      }),
    );
    // 补详情的结构化进度只驱动进度条；其文本行已由后端经 agent://log 推送，这里不再重复追加。
    track(
      listen<DetailProgress>("detail://progress", (e) => {
        setDetailProgress(e.payload);
      }),
    );
    // 框选层提交成功 / 取消：复位按钮态、回显结论，并重跑自检刷新「种子点位标定」一步。
    track(
      listen<SeedMark>("rpa://seed-marked", (e) => {
        setRpaPicking(false);
        setRpaMark(e.payload);
        setRpaMarkMsg(e.payload.message);
        appendLocalLog("rpa", "框选种子：" + e.payload.message);
        toast.success("种子点位已标定", { description: e.payload.message });
        void refreshRpaCheck();
      }),
    );
    track(
      listen("rpa://seed-pick-cancelled", () => {
        setRpaPicking(false);
      }),
    );

    // 启动即拉：健康 + 证书 + 文章计数（验证前端↔Rust↔core 打通）。
    void refreshHealth();
    void refreshCert();
    void refreshCounts();
    // 启动默认自动自检 RPA 环境（横线 step 呈现在控制面板）。
    void refreshRpaCheck();
    // 系统状态 → 清残留代理 → 拦截探针，三步**串行**：
    // 1. 启动自检：上次没正常退出（崩溃 / 强杀）留下的系统代理会让全机浏览器断网，后端发现即关掉，这里只负责告诉用户；
    // 2. 拦截探针会真设一次系统代理再复位，若与第 1 步并发，残留会被探针「顺手」复位而没有任何提示（2026-09-11 实测）；
    // 3. 探针结果只在 Windows 的自检卡里展示，mac（人工模式）不自动跑，避免每次启动都无谓改动系统代理。
    void (async () => {
      const s = await refreshStatus();
      try {
        const m = await sysproxyResetStale(cfgRef.current.sysproxy_service);
        if (m) {
          toast.warning("已清理上次残留的系统代理", { description: m });
          void refreshStatus();
        }
      } catch {
        // 查不了就算了，6s 轮询仍会显示当前代理态。
      }
      if (s?.rpa_auto) void refreshProxyProbe();
    })();
    // 恢复巡检按钮态：若后台主循环在跑（如前端刷新/重开窗口），据 loop_status 回填。
    void refreshLoop();

    const onFocus = () => {
      void refreshCert();
      void refreshStatus();
    };
    window.addEventListener("focus", onFocus);

    return () => {
      disposed = true;
      unlisteners.forEach((f) => f());
      window.removeEventListener("focus", onFocus);
    };
    // 这些回调都是稳定引用（useCallback 无易变依赖），仅挂载一次。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 主循环 / 巡检开关状态：3 秒轮询（停止收尾、历史模式退出、后端拒绝启动都靠它对齐按钮态）。
  useEffect(() => {
    const id = window.setInterval(() => void refreshLoop(), 3000);
    return () => window.clearInterval(id);
  }, [refreshLoop]);

  // 系统状态轮询：平时 6s；任务活跃（主循环在跑/抓包代理在跑）时 2s，
  // 让状态栏的运行期指示灯（全局代理/抓包代理）跟得上起停节奏。
  const taskActive = loopRunning || !!system?.capture_running;
  useEffect(() => {
    const id = window.setInterval(
      () => void refreshStatus(),
      taskActive ? 2000 : 6000,
    );
    return () => window.clearInterval(id);
  }, [taskActive, refreshStatus]);

  const value = useMemo<AppStore>(
    () => ({
      certInstalled,
      certDetail,
      certLog,
      installing,
      uninstalling,
      refreshCert,
      installCert,
      uninstallCert,
      health,
      healthError,
      refreshHealth,
      system,
      refreshStatus,
      cfg,
      saveCfg,
      running,
      stopping,
      loopRunning,
      pollBusy,
      reportError,
      startSweep,
      stopSweep,
      counts,
      refreshCounts,
      detailRunning,
      detailProgress,
      detailError,
      runDetail,
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
      articlesBiz,
      setArticlesBiz,
      logs,
      clearLogs,
      replaceLogs,
      appendLocalLog,
      history,
      refreshHistory,
      alert,
      ackAlert,
      autoClick,
      isMac,
    }),
    [
      certInstalled,
      certDetail,
      certLog,
      installing,
      uninstalling,
      refreshCert,
      installCert,
      uninstallCert,
      health,
      healthError,
      refreshHealth,
      system,
      refreshStatus,
      cfg,
      saveCfg,
      running,
      stopping,
      loopRunning,
      pollBusy,
      reportError,
      startSweep,
      stopSweep,
      counts,
      refreshCounts,
      detailRunning,
      detailProgress,
      detailError,
      runDetail,
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
      articlesBiz,
      logs,
      clearLogs,
      replaceLogs,
      appendLocalLog,
      history,
      refreshHistory,
      alert,
      ackAlert,
      autoClick,
      isMac,
    ],
  );

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

export function useApp(): AppStore {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error("useApp 必须在 <AppProvider> 内使用");
  return ctx;
}
