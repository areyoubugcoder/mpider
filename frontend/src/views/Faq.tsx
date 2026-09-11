import { useState } from "react";
import { Gauge, ShieldAlert, Wrench } from "lucide-react";
import { useApp } from "@/store";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { CollapsibleSection } from "@/components/ui/collapsible-section";
import type { View } from "@/views/types";

/**
 * 常见问题（FAQ）：面向使用者解释微信公众号列表接口的访问限制、本软件的节制策略与使用者的责任。
 *
 * 措辞约定（项目开源，读者是外部用户）：
 * - 只描述「接口有什么限制、软件怎么自我节制、使用者该怎么做」，不写规避、绕过、对抗类表述；
 * - 不出现内部环境、账号编号等运维细节；观测数据以匿名样本呈现；
 * - 限流观测结论只维护在本页（不另立文档）。页面是静态文案，只从运行配置读当前预算 / 间隔用于对照。
 */

/** 观测样本（三个测试账号触发账号级限制时的累计列表请求数；账号匿名）。 */
const SAMPLES: Array<{
  label: string;
  hours: string;
  perHour: string;
  calls: number;
}> = [
  { label: "样本 A", hours: "约 20 小时", perHour: "约 20", calls: 224 },
  { label: "样本 B", hours: "约 7 小时", perHour: "27–40", calls: 223 },
  { label: "样本 C", hours: "约 6 小时", perHour: "20–45", calls: 206 },
];

function Q({
  title,
  badge,
  defaultOpen,
  forceOpen,
  children,
}: {
  title: string;
  badge?: string;
  defaultOpen?: boolean;
  /** 「全部展开」：外层重挂载后以此为初始状态。 */
  forceOpen?: boolean;
  children: React.ReactNode;
}) {
  return (
    <CollapsibleSection
      title={title}
      badge={badge ? <Badge variant="secondary">{badge}</Badge> : undefined}
      defaultOpen={forceOpen || defaultOpen}
      contentClassName="p-4 pt-0 text-xs leading-relaxed [&_p]:mb-2 [&_ul]:mb-2 [&_ul]:list-disc [&_ul]:pl-5 [&_ol]:mb-2 [&_ol]:list-decimal [&_ol]:pl-5 [&_li]:mb-1 [&_strong]:text-foreground [&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_code]:py-0.5 [&_code]:text-xs"
    >
      {children}
    </CollapsibleSection>
  );
}

export function Faq({ onNav }: { onNav: (v: View) => void }) {
  const { cfg } = useApp();
  const [openAll, setOpenAll] = useState(false);
  const budget = cfg.list_daily_budget;
  const gapMin = Math.round(cfg.list_gap_min_ms / 1000);
  const gapMax = Math.round(cfg.list_gap_max_ms / 1000);
  const perHourAllDay = budget > 0 ? Math.floor(budget / 24) : null;

  const link = (v: View, text: string) => (
    <button
      type="button"
      onClick={() => onNav(v)}
      className="text-link underline underline-offset-2 hover:text-link/80"
    >
      {text}
    </button>
  );

  return (
    <div className="mx-auto max-w-4xl">
      <Card className="mb-3">
        <CardHeader className="pb-2">
          <CardTitle className="flex items-center gap-2 text-base">
            <ShieldAlert className="size-4 text-foreground" />
            常见问题 · 访问频率与账号保护
          </CardTitle>
          <CardDescription>
            本软件只用于采集公开可见的公众号文章。微信对公众号列表接口有访问限制，超出后会暂时限制所登录的微信账号；
            这里说明限制是什么、软件如何自我节制、使用者该注意什么。当前配置：每账号 24 小时预算{" "}
            <strong>{budget > 0 ? `${budget} 次` : "不限"}</strong>、请求间隔{" "}
            <strong>
              {gapMin}–{gapMax} s
            </strong>
            。参数在 {link("settings", "「系统设置 → 列表采集与节流」")}，实时数据在 {link("ratelimit", "「限流分析」")}。
          </CardDescription>
        </CardHeader>
        <CardContent className="pt-0">
          <button
            type="button"
            onClick={() => setOpenAll((v) => !v)}
            className="text-xs text-muted-foreground underline underline-offset-2 hover:text-foreground"
          >
            {openAll ? "全部收起" : "全部展开"}
          </button>
        </CardContent>
      </Card>

      <div className="flex flex-col gap-2.5" key={openAll ? "open" : "closed"}>
        <Q
          forceOpen={openAll}
          title="列表接口自 2026 年 9 月 10 日起不再返回文章列表"
          badge="当前现状"
          defaultOpen
        >
          <p>
            <strong>这是本软件目前最重要的事实：列表接口（<code>getmsg</code>）已经取不到数据。</strong>
            请求依然返回 HTTP 200 与 <code>ret=0</code>、<code>errmsg="ok"</code>，但响应里<strong>没有文章列表字段</strong>
            （<code>general_msg_list</code>、<code>next_offset</code>、<code>real_type</code> 一并消失），<code>msg_count</code> 为 0。
            既不报错也不给数据。
          </p>
          <p>观测到的变化过程：</p>
          <ul>
            <li>9 月 10 日中午，同一账号、同一接口还能正常返回，单次拿到 10 条、响应体两万多字节。</li>
            <li>当天下午起，同一账号的同一请求开始返回上述空响应。</li>
            <li>
              随后用<strong>三个不同微信账号</strong>（其中一个此前从未触发过任何限制、本次是它的第一次列表请求）、
              在 <strong>Windows 与 macOS 两台机器</strong>上、对<strong>七个不同公众号</strong>各试一遍，响应完全一致。
            </li>
          </ul>
          <p>
            一个全新账号的第一次请求就是空响应，说明这不是账号级限制，也不是本机配置、证书或凭证的问题。
            指向的是<strong>服务端对该接口的调整</strong>：以这种方式回放公众号列表接口，已经拿不到数据。
          </p>
          <p>
            因此<strong>本软件的列表采集功能当前不可用</strong>。软件会把这种响应识别出来，在日志里以警告标出
            「首页返回成功却没有任何文章」，而不是误报成「没有更多历史文章」。凭证抓取、接力、正文解析等其余环节不受影响。
            本项目现已转为<strong>学习与经验分享</strong>用途，本页与
            {" "}<code>docs/how-it-works.md</code>{" "}保留完整的实现原理与这段时间的观测记录。
          </p>
          <p className="rounded-md border border-border bg-muted/40 p-3">
            <strong>如果你是刚需</strong>：要的是稳定拿到公众号文章数据，可以试试{" "}
            <a
              href="https://mp2rss.bugcode.dev/"
              target="_blank"
              rel="noreferrer noopener"
              className="text-link underline underline-offset-2 hover:text-link/80"
            >
              Mp2RSS
            </a>
            ：订阅公众号与 X 账号持续抓取，文章永久留存，Open API / RSS / OPML / CLI 任选取数，
            不必用自己的账号承担风险。
          </p>
        </Q>

        <Q forceOpen={openAll} title="使用前请先了解" badge="须知" defaultOpen>
          <ul>
            <li>本软件采集的是公众号<strong>公开发布</strong>的文章列表与正文，不涉及任何非公开数据。</li>
            <li>
              列表接口的访问需要登录微信账号，微信会对单个账号的访问量做限制。<strong>请使用自己的账号、遵守平台规则与当地法律法规</strong>，
              并接受账号可能被暂时限制的风险；本软件的节制策略只是尽量降低这种风险，不承诺不会触发。
            </li>
            <li>请把采集频率控制在自己实际需要的范围内，不要为了「跑满」而调高预算或调低间隔。</li>
          </ul>
        </Q>

        <Q forceOpen={openAll} title="列表接口的限制是怎样的？" badge="观测结论">
          <p className="text-muted-foreground">
            以下是接口还能返回数据时（2026 年 9 月 10 日之前）的观测记录，现在作为经验资料保留；
            当前接口状态见本页第一条。
          </p>
          <p>
            根据 2026 年 9 月的测试观测，列表接口（<code>getmsg</code>）的限制<strong>按登录的微信账号累计计数</strong>：
            大约 24 小时内累计 200 次出头就会返回 <code>ret=-6</code>，账号随后约一天内无法再访问列表接口。
          </p>
          <div className="mb-2 overflow-x-auto">
            <table className="w-full text-xs">
              <thead className="text-muted-foreground">
                <tr className="border-b">
                  <th className="py-1 pr-3 text-left font-medium">样本</th>
                  <th className="py-1 pr-3 text-right font-medium">持续时长</th>
                  <th className="py-1 pr-3 text-right font-medium">每小时请求</th>
                  <th className="py-1 text-right font-medium">触发时累计</th>
                </tr>
              </thead>
              <tbody>
                {SAMPLES.map((r) => (
                  <tr key={r.label} className="border-b last:border-0">
                    <td className="py-1 pr-3">{r.label}</td>
                    <td className="py-1 pr-3 text-right tabular-nums">{r.hours}</td>
                    <td className="py-1 pr-3 text-right tabular-nums">{r.perHour}</td>
                    <td className="py-1 text-right font-medium tabular-nums">{r.calls}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <ul>
            <li>三个样本的持续时长、每小时密度、出口 IP 都不同，触发时的累计次数却接近，说明起决定作用的是<strong>累计量</strong>而非速率，也与出口 IP 无关。</li>
            <li>计数不按自然日：有样本跨了两个日历日、触发当天只发了几十次；也不是 12 小时窗口。</li>
            <li>请求间隔全部在 8 秒以上，密度更高的样本触发略早，但差别不大。</li>
            <li>样本量很小（三个账号），数字只能作为量级参考，不是精确阈值。</li>
          </ul>
        </Q>

        <Q forceOpen={openAll} title="软件是怎么自我节制的？默认预算为什么是 180？" badge="策略">
          <p>四道闸，都可在 {link("settings", "「系统设置 → 列表采集与节流」")} 查看：</p>
          <ol>
            <li>
              <strong>每账号 24 小时预算 list_daily_budget（默认 180）</strong>：当前登录账号近 24 小时的列表请求达到预算后，
              软件<strong>停止领取任务、暂停巡检</strong>，等最早一次请求过了 24 小时再自动恢复。
              预算在每个任务或批次开始前检查，一批最多超出一批的号数（不超过 10），因此取观测最低值 206 再留出这段余量。
            </li>
            <li>
              <strong>全局请求间隔 {gapMin}–{gapMax} 秒</strong>：任意两次列表请求之间随机等待，避免短时间密集访问。它防的是「太密」，对累计量无效，
              但也不建议调到 5 秒以下。
            </li>
            <li>
              <strong>账号级退避</strong>：一旦收到 <code>ret=-6</code>，立即停止一切列表请求 6 小时，再次触发则 12 小时、24 小时。
            </li>
            <li>
              <strong>巡检节奏</strong>：批次间隔 60 秒、整批失败暂停 15 分钟，避免失败的号被反复重试。
            </li>
          </ol>
          <p>
            <strong>不建议调高预算。</strong>观测最低值是 206，预算超过 190 就没有余量了；「0 = 不限」只适合明确知道自己在做什么的调试场景。
          </p>
        </Q>

        <Q forceOpen={openAll} title="采集量该怎么规划？" badge="容量">
          <p>
            每个公众号的一次列表检查约等于一次列表请求（有「最后发布时间」时首页即可判断，很少翻页）。按每账号 24 小时{" "}
            {budget > 0 ? budget : 180} 次算：
          </p>
          <ul>
            <li>每小时检查 20 个号：约 9 个小时后额度用完，之后自动暂停到次日同一时段。</li>
            <li>要全天持续运行：每小时应不超过 {perHourAllDay ?? 7} 个号。</li>
            <li>
              额度是账号级的硬上限，请按它来安排关注的公众号数量与检查频率。多个微信号可在{" "}
              {link("wxaccounts", "「微信号管理」")}登记并切换，但同一时刻只用一个；请仍以自己的实际需要为准，不要为了跑满而堆账号。
            </li>
          </ul>
          <p>预算用完不是故障：控制面板与限流分析页会显示「已用 / 预算」和恢复时刻，日志「应用」环节会记一条暂停。</p>
        </Q>

        <Q forceOpen={openAll} title="收到 ret=-6 了，该怎么办？" badge="处置">
          <ul>
            <li>
              <strong>等待。</strong>限制通常在约一天后解除；软件已自动进入退避，期间不会再发列表请求。
            </li>
            <li>
              <strong>不要</strong>点「解除退避」「立即开始新一轮」「重新巡检」，每一次都会再发一次请求，只会延长限制。
            </li>
            <li>
              <strong>不要</strong>试图换凭证或重新接力：这是账号级限制，与公众号、凭证、出口 IP 都无关，新抓的凭证同样会返回 -6。
            </li>
            <li>限制解除后，先按默认预算运行，观察一两天再考虑是否需要调整。</li>
          </ul>
        </Q>

        <Q forceOpen={openAll} title="ret=-3、ret=-6、429 与验证页分别是什么？" badge="识别">
          <ul>
            <li>
              <strong>ret=-3 no session</strong>：访问凭证过期（约 30 分钟一换）。软件会集中续期后从断点继续，不是限制。
            </li>
            <li>
              <strong>ret=-6 unknown error</strong>：账号级访问限制，约一天。以上所有策略都是为它设计的。
            </li>
            <li>
              <strong>HTTP 429 / 5xx、接口返回网页</strong>：IP 级临时限制，分钟级。软件等 15 秒重试同一页一次，仍如此则退避 5 分钟起翻倍。测试中很少出现。
            </li>
            <li>
              <strong>打开文章时出现「环境异常，完成验证后继续」</strong>：多数是链接本身已失效（<code>sn</code> 参数过期），不是限制。
              软件会换一篇重试，连续两批都遇到才按限制处理。
            </li>
            <li>
              <strong>接口返回成功却一篇文章都没有</strong>：见本页第一条「当前现状」。这是 2026 年 9 月 10 日起
              服务端侧的变化，与账号、机器、公众号都无关，换号也不会恢复。
            </li>
          </ul>
        </Q>

        <Q forceOpen={openAll} title="多个微信号怎么用？换号后为什么旧凭证不复用？" badge="多账号">
          <ul>
            <li>
              每个抓到过凭证的微信号都会自动登记到{link("wxaccounts", "「微信号管理」")}（可改别名），各自「框选种子」标定文件传输助手里的种子链接位置；
              采集只用<strong>当前激活</strong>的那一个。换号时先在微信客户端切换登录，再在此激活对应条目；没登记过的新号会在抓到凭证时自动登记。
            </li>
            <li>
              记录按微信号的 <code>uin</code> 区分。若抓到的 <code>uin</code> 不属于当前激活条目，任务会中止并提示「登录的微信号与激活的不一致」，
              不会把凭证记到错误的号上；请在微信号管理里激活对应条目后重试。
            </li>
            <li>
              凭证（<code>key</code>）是按微信号签发的，换号后旧号的凭证即使没过期也不再复用，需要重新接力一次。
            </li>
            <li>每个微信号的 24 小时预算、退避与封禁状态分别记录，页面上会显示「正常 / 已用 n 次 / 受限至几点」。</li>
          </ul>
        </Q>

        <Q forceOpen={openAll} title="重启软件后为什么还在退避？" badge="退避">
          <p>
            退避与封禁状态会持久化到本地数据库，重启不清零。这是有意为之：限制是微信侧按账号计的，重启本机并不会让它消失，
            清零只会让软件在限制期内继续发请求、延长限制。确认是误判（例如坏链引起的验证页）时，可在控制面板手动「解除退避」。
          </p>
        </Q>

        <Q forceOpen={openAll} title="批量补正文时提示「命中验证页」，是被限流了吗？" badge="正文">
          <ul>
            <li>
              多数不是。正文页 <code>/s</code> 是公开页面，无效的 <code>sn</code>（尤其是从短链或旧链接添加的文章）会稳定返回验证页，
              这是链接本身的问题。
            </li>
            <li>软件命中验证页时会先抓一条已知有效的对照链接确认：对照也是验证页才判为限流并停止本批，否则只按该篇失败计数。</li>
            <li>测试中以 250 毫秒串行抓 100 多篇也没有触发过真正的正文限流，默认节流已足够保守。</li>
          </ul>
        </Q>

        <Q forceOpen={openAll} title="抓取历史文章会不会把额度用光？" badge="历史抓取">
          <ul>
            <li>历史抓取每翻一页就是一次列表请求，与巡检共用当前微信号的 24 小时预算；巡检开着时不能抓历史，先停止巡检。</li>
            <li>
              软件会为巡检保留一部分额度（系统设置「为巡检保留的预算」，默认 30 次）：历史抓取用到只剩这部分时自动暂停，
              等最早的请求滑出 24 小时窗口再自动继续，所以一个大任务可能分多天完成。
            </li>
            <li>同一时刻只抓一个号，页与页之间按设置的间隔等待（默认 60 秒），中途可以暂停、继续、取消，重启软件会从上次的位置续翻。</li>
            <li>设置了「截止日期」时，比它新的文章仍要翻过去才到得了，这些页同样计入预算。</li>
            <li>预算达到上限、微信号受限时会弹全局提醒，底部状态栏也会显示暂停原因与恢复时刻。</li>
          </ul>
        </Q>

        <Q forceOpen={openAll} title="怎么看当前用了多少额度？" badge="观察">
          <ul>
            <li>{link("dashboard", "控制面板")}：「当前号近 24 小时列表请求 已用 / 预算」。</li>
            <li>
              {link("ratelimit", "限流分析")}：首卡「当前号近 24 小时列表请求」；按账号统计表（只显示账号的短哈希，不含账号原文）可查看每个账号的累计次数；
              用量到 80% 时诊断结论会提前提示。
            </li>
            <li>{link("logs", "日志")}：「文章列表」环节的「首页响应 … ret=…」，以及预算暂停 / 恢复记录。</li>
          </ul>
        </Q>

        <Q forceOpen={openAll} title="这些结论有哪些不确定之处？" badge="待验证">
          <ul>
            <li>额度是滚动 24 小时还是自首次使用起累计，样本的持续时长都不足 24 小时，无法区分；软件按滚动 24 小时实现，是偏保守的取法。</li>
            <li>额度是否也把打开文章页、访问公众号主页算在内；样本中文章页打开与列表请求近乎一比一，无法区分。</li>
            <li>限制的精确解除时长（观测到不超过两天），以及其它错误码（如 <code>ret=-12</code>）的含义。</li>
            <li>
              2026 年 9 月 10 日的接口变化是否为永久调整、是否存在其它可用的公开访问方式，均未继续验证；
              本项目到此转为学习与经验分享用途，不再追查。
            </li>
          </ul>
          <p className="text-muted-foreground">
            <Wrench className="mr-1 inline size-3.5" />
            结论有更新时会同步本页。欢迎在遵守平台规则的前提下补充观测数据。
          </p>
        </Q>
      </div>

      <p className="mt-3 flex items-center gap-1.5 text-xs text-muted-foreground">
        <Gauge className="size-3.5" />
        预算与间隔对所有发出列表请求的路径统一生效（定时巡检 / 单号重新巡检 / 历史抓取 / 断点续采）。
      </p>
    </div>
  );
}
