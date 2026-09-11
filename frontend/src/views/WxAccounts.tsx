import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Crosshair,
  MousePointerClick,
  Pencil,
  Power,
  ShieldCheck,
  Trash2,
  Unlock,
  UserRound,
} from "lucide-react";
import { toast } from "sonner";
import { useApp } from "@/store";
import {
  fmtLogDateTime,
  wxActivate,
  wxDelete,
  wxList,
  wxUnblock,
  wxUpdate,
  type WxAccountView,
} from "@/lib/tauri";
import { msg } from "@/lib/utils";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Pager, usePaged } from "@/components/Pager";
import { InfoTip } from "@/components/InfoTip";
import type { View } from "@/views/types";

/**
 * 微信号管理：没有手动新增入口——接力抓到凭证时按 uin 自动登记；这里只能改别名 / 备注、切换激活、标定、解封、删除。
 * 采集同一时刻只用**激活**的那一个。
 *
 * 每个号各自：种子标定（框选 / 测试点击落到该号的标定文件）、近 24 小时列表请求预算、封号退避。
 * 抓到的 uin 不属于激活号会中止任务。换号 = 在微信客户端切换登录 → 这里激活对应条目。
 */

/** 秒数 → 「N 小时 M 分」。 */
function fmtRemain(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s >= 3600) return `${Math.floor(s / 3600)} 小时 ${Math.floor((s % 3600) / 60)} 分`;
  if (s >= 60) return `${Math.floor(s / 60)} 分`;
  return `${s} 秒`;
}

/** 限流状态单元格：正常 / 预算 / 受限。徽标只给结论，数字与解释收进说明气泡。 */
function LimitCell({ a, now }: { a: WxAccountView; now: number }) {
  if (a.status === "blocked" && a.blocked_until) {
    return (
      <div className="flex items-center gap-1 whitespace-nowrap">
        <Badge variant="destructive">受限至 {fmtLogDateTime(a.blocked_until)}</Badge>
        <InfoTip>
          剩余 {fmtRemain(a.blocked_until - now)}
          {a.blocked_reason ? ` · ${a.blocked_reason}` : ""}
          。微信号级限制（ret=-6）按号退避、不换 key 重试，到期自动恢复。
        </InfoTip>
      </div>
    );
  }
  const budgetText = a.budget > 0 ? `${a.budget_used_24h} / ${a.budget}` : `${a.budget_used_24h} / 不限`;
  if (a.status === "budget") {
    return (
      <div className="flex items-center gap-1 whitespace-nowrap">
        <Badge variant="warning">预算已用完</Badge>
        <InfoTip>
          近 24 小时列表请求已用 / 预算：{budgetText}。达到预算后巡检与历史抓取都暂停，等最早一次请求滑出 24 小时窗口自动恢复。
        </InfoTip>
      </div>
    );
  }
  return (
    <div className="flex items-center gap-1 whitespace-nowrap">
      <Badge variant="success">正常</Badge>
      <InfoTip>
        近 24 小时列表请求已用 / 预算：{budgetText}。按当前微信号滚动 24 小时累计，达到预算巡检与历史抓取自动暂停；预算在「系统设置」里调。
      </InfoTip>
    </div>
  );
}

/** 编辑别名 / 备注。 */
function EditDialog({
  open,
  initial,
  onClose,
}: {
  open: boolean;
  initial: WxAccountView;
  onClose: () => void;
}) {
  const qc = useQueryClient();
  const [alias, setAlias] = useState(initial.alias);
  const [note, setNote] = useState(initial.note ?? "");
  const save = useMutation({
    mutationFn: () => wxUpdate(initial.id, alias, note),
    onSuccess: () => {
      toast.success("已保存");
      void qc.invalidateQueries({ queryKey: ["wx-accounts"] });
      onClose();
    },
    onError: (e) => toast.error("保存失败", { description: msg(e) }),
  });
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>编辑微信号</DialogTitle>
          <DialogDescription>
            别名只用于在本软件里区分；账号本身在抓到凭证时已自动登记，不可更改。
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-3">
          <div className="grid gap-1.5">
            <Label htmlFor="wx-alias">别名</Label>
            <Input
              id="wx-alias"
              value={alias}
              placeholder="如：采集 1 号"
              autoFocus
              onChange={(e) => setAlias(e.target.value)}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="wx-note">备注（可选）</Label>
            <Input
              id="wx-note"
              value={note}
              placeholder="如：哪台机器 / 哪个手机号"
              onChange={(e) => setNote(e.target.value)}
            />
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" size="sm" onClick={onClose}>
            取消
          </Button>
          <Button
            size="sm"
            loading={save.isPending}
            disabled={!alias.trim()}
            onClick={() => save.mutate()}
          >
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function WxAccounts({ onNav }: { onNav: (v: View) => void }) {
  const qc = useQueryClient();
  // running = 后端主循环在跑（巡检 / 历史抓取），期间不能切换 / 删除激活的微信号。
  const { loopRunning: running, rpaPicking, rpaTestClicking, beginSeedPick, testClickRpa, autoClick } = useApp();
  const q = useQuery({
    queryKey: ["wx-accounts"],
    queryFn: wxList,
    refetchInterval: 5_000,
  });
  const list = q.data ?? [];
  const pg = usePaged(list, 10);
  const now = Date.now() / 1000;
  const [editing, setEditing] = useState<WxAccountView | null>(null);
  const [pendingDelete, setPendingDelete] = useState<WxAccountView | null>(null);
  /** 正在为哪个号做框选 / 测试点击（按钮 loading 态用）。 */
  const [busyId, setBusyId] = useState<number | null>(null);

  const refresh = () => void qc.invalidateQueries({ queryKey: ["wx-accounts"] });
  const activate = useMutation({
    mutationFn: wxActivate,
    onSuccess: () => toast.success("已切换激活的微信号", { description: "请确认微信客户端登录的正是这个号" }),
    onError: (e) => toast.error("切换失败", { description: msg(e) }),
    onSettled: refresh,
  });
  const unblock = useMutation({
    mutationFn: wxUnblock,
    onSuccess: () => toast.success("已解除该微信号的封号退避"),
    onError: (e) => toast.error("解封失败", { description: msg(e) }),
    onSettled: refresh,
  });
  const del = useMutation({
    mutationFn: wxDelete,
    onSuccess: () => toast.success("已删除"),
    onError: (e) => toast.error("删除失败", { description: msg(e) }),
    onSettled: () => {
      setPendingDelete(null);
      refresh();
    },
  });

  const pick = async (a: WxAccountView) => {
    setBusyId(a.id);
    try {
      await beginSeedPick(a.id);
    } finally {
      setBusyId(null);
      refresh();
    }
  };
  const testClick = async (a: WxAccountView) => {
    setBusyId(a.id);
    try {
      await testClickRpa(a.id);
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div>
      <div className="mb-3">
        <h1 className="text-xl font-semibold tracking-tight">微信号管理</h1>
        <p className="mt-0.5 text-xs text-muted-foreground">
          抓到凭证的微信号会自动登记到这里；采集同一时刻只用一个微信号，换号请先在微信客户端切换登录，再在此激活对应条目
        </p>
      </div>

      <Card className="mb-3">
        <CardHeader className="pb-2">
          <CardTitle className="flex items-center gap-2 text-sm">
            <ShieldCheck className="size-4 text-foreground" />
            每个微信号各自记录
          </CardTitle>
          <CardDescription className="text-xs">
            {autoClick ? "种子标定位置（各号的文件传输助手里都要发一次种子链接并「框选」）、" : ""}
            近 24 小时列表请求预算、封号退避。
            抓到的凭证不属于激活的号时任务会中止并提示。采集运行中不能切换或删除激活的号。
          </CardDescription>
        </CardHeader>
      </Card>

      <Card>
        <CardContent className="p-0">
          {list.length === 0 ? (
            <div className="px-4 py-10 text-center text-xs text-muted-foreground">
              还没有登记的微信号。先在
              <button
                type="button"
                onClick={() => onNav("dashboard")}
                className="mx-1 text-link underline underline-offset-2 hover:text-link/80"
              >
                控制面板
              </button>
              {autoClick ? "框选种子并开始巡检" : "开始巡检"}，抓到凭证后会自动登记，你可以在这里改别名、切换激活。
            </div>
          ) : (
            <>
            <Table className="min-w-[980px]">
              <TableHeader>
                <TableRow>
                  <TableHead>别名</TableHead>
                  <TableHead>标识</TableHead>
                  {autoClick && <TableHead>标定</TableHead>}
                  <TableHead>激活</TableHead>
                  <TableHead>限流状态</TableHead>
                  <TableHead>最近抓凭证</TableHead>
                  <TableHead className="text-right">操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {pg.slice.map((a) => {
                  const busy = busyId === a.id && (rpaPicking || rpaTestClicking);
                  return (
                    <TableRow key={a.id} className={a.is_active ? "bg-primary/[0.04]" : undefined}>
                      <TableCell className="whitespace-nowrap">
                        <div className="flex items-center gap-1.5 font-medium">
                          <UserRound className="size-3.5 text-muted-foreground" />
                          {a.alias}
                        </div>
                        {a.note && (
                          <div className="text-[11px] text-muted-foreground">{a.note}</div>
                        )}
                      </TableCell>
                      <TableCell>
                        <span title="账号标识（只显示 uin 的短哈希）">
                          <Badge variant="secondary">{a.uin_hash?.slice(0, 6) ?? "—"}</Badge>
                        </span>
                      </TableCell>
                      {autoClick && (
                        <TableCell>
                          {a.calibrated === "ok" && <Badge variant="success">已标定</Badge>}
                          {a.calibrated === "stale" && (
                            <span title="文件助手窗口尺寸或屏幕分辨率变了，请重新框选">
                              <Badge variant="warning">已失效</Badge>
                            </span>
                          )}
                          {a.calibrated === "none" && <Badge variant="muted">未标定</Badge>}
                        </TableCell>
                      )}
                      <TableCell>
                        {a.is_active ? (
                          <Badge>当前激活</Badge>
                        ) : (
                          <Button
                            variant="outline"
                            size="sm"
                            className="h-7 px-2 text-xs"
                            disabled={running || activate.isPending}
                            title={running ? "采集进行中（巡检 / 历史抓取），先停止再切换" : "把它设为采集用的微信号"}
                            onClick={() => activate.mutate(a.id)}
                          >
                            <Power className="size-3" />
                            激活
                          </Button>
                        )}
                      </TableCell>
                      <TableCell className="whitespace-nowrap">
                        <LimitCell a={a} now={now} />
                      </TableCell>
                      <TableCell className="whitespace-nowrap text-xs text-muted-foreground">
                        {a.last_captured_at ? fmtLogDateTime(a.last_captured_at) : "—"}
                      </TableCell>
                      <TableCell className="whitespace-nowrap">
                        <div className="flex justify-end gap-1">
                          {autoClick && (
                            <>
                              <Button
                                variant="outline"
                                size="sm"
                                className="h-7 px-2 text-xs"
                                loading={busy && rpaPicking}
                                disabled={rpaPicking || rpaTestClicking}
                                title="在该号登录的微信里框住文件传输助手中的种子链接消息"
                                onClick={() => void pick(a)}
                              >
                                {!(busy && rpaPicking) && <Crosshair className="size-3" />}
                                框选种子
                              </Button>
                              <Button
                                variant="outline"
                                size="sm"
                                className="h-7 px-2 text-xs"
                                loading={busy && rpaTestClicking}
                                disabled={rpaPicking || rpaTestClicking || a.calibrated !== "ok"}
                                title={
                                  a.calibrated === "ok"
                                    ? "按该号的标定点位点一次，看能否拉起内置浏览器"
                                    : "先框选种子"
                                }
                                onClick={() => void testClick(a)}
                              >
                                {!(busy && rpaTestClicking) && <MousePointerClick className="size-3" />}
                                测试点击
                              </Button>
                            </>
                          )}
                          {a.status === "blocked" && (
                            <Button
                              variant="outline"
                              size="sm"
                              className="h-7 px-2 text-xs"
                              disabled={unblock.isPending}
                              title="确认限制已解除（或误判）时手动解封；限制期内解封只会延长限制"
                              onClick={() => unblock.mutate(a.id)}
                            >
                              <Unlock className="size-3" />
                              解封
                            </Button>
                          )}
                          <Button
                            variant="ghost"
                            size="sm"
                            className="h-7 px-2 text-xs"
                            onClick={() => setEditing(a)}
                          >
                            <Pencil className="size-3" />
                            编辑
                          </Button>
                          <Button
                            variant="ghost"
                            size="sm"
                            className="h-7 px-2 text-xs text-destructive hover:text-destructive"
                            disabled={a.is_active && running}
                            title={a.is_active && running ? "采集进行中，不能删除激活的号" : "删除该微信号及其标定"}
                            onClick={() => setPendingDelete(a)}
                          >
                            <Trash2 className="size-3" />
                            删除
                          </Button>
                        </div>
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
            <Pager page={pg.page} pageCount={pg.pageCount} total={pg.total} unit="个" onPage={pg.setPage} />
            </>
          )}
        </CardContent>
      </Card>

      {editing !== null && (
        <EditDialog key={editing.id} open initial={editing} onClose={() => setEditing(null)} />
      )}

      <Dialog open={pendingDelete !== null} onOpenChange={(o) => !o && setPendingDelete(null)}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>删除微信号</DialogTitle>
            <DialogDescription>
              删除『{pendingDelete?.alias}』及其种子标定。已抓到的凭证与公众号数据不受影响。
              {pendingDelete?.is_active ? " 它是当前激活的号，删除后需要另选一个激活。" : ""}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setPendingDelete(null)}>
              取消
            </Button>
            <Button
              variant="destructive"
              size="sm"
              loading={del.isPending}
              onClick={() => pendingDelete && del.mutate(pendingDelete.id)}
            >
              删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
