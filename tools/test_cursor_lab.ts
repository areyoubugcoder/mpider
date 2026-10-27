/** 实验室展示回归：原游标证据、缺测、会话故障和计划时间不得混淆。 */
import test from "node:test";
import assert from "node:assert/strict";
import { experimentSummary, plannedChecks, parseSchedule, phaseText, outcomeNames } from "../frontend/src/lib/cursor-lab.ts";
import type { Experiment, Probe } from "../frontend/src/lib/workbench.ts";
const experiment = (extra: Partial<Experiment> = {}): Experiment => ({ id: 1, cursor_id: 8, session_id: "synthetic-session", schedule_hours: [0, 1, 2], next_index: 1, state: "running", started_at: 1000, deadline_at: 87400, conclusion: null, report_path: null, ...extra });
const probe = (role: string, outcome: string, age: number, ts = 1000): Probe => ({ id: 1, experiment_id: 1, request_id: "synthetic-request", role, outcome, scheduled_at: ts, actual_at: ts, cursor_age_seconds: age, evidence_path: null, detail: {} });
test("新游标对照的年龄不能被计入原游标的可用下界", () => {
  const result = experimentSummary(experiment(), [probe("scheduled", "valid", 7200), probe("control_page", "valid", 999999)]);
  assert.equal(result.lastSuccessAge, 7200); assert.match(result.title, /2 小时/);
});
test("会话认证失败不能显示游标已过期", () => {
  const result = experimentSummary(experiment({ state: "done" }), [probe("scheduled", "authentication_failed", 8000)]);
  assert.match(result.title, /无法判断/); assert.equal(result.lastSuccessAge, null); assert.equal(result.tone, "warning");
});
test("服务端会话过期单独分类，同样不显示游标已过期", () => {
  const result = experimentSummary(experiment({ state: "done" }), [probe("scheduled", "session_timeout", 8000)]);
  assert.match(result.title, /无法判断/); assert.equal(result.lastSuccessAge, null); assert.equal(result.tone, "warning");
  assert.equal(outcomeNames.session_timeout, "会话已过期");
});
test("会话过期截断观察时，已证实的可用年龄不被合掉", () => {
  const result = experimentSummary(experiment({ state: "done", verdict: "at_least_valid" }), [
    probe("scheduled", "valid", 1800),
    probe("scheduled", "session_timeout", 10800),
  ]);
  assert.match(result.title, /游标年龄 30 分 0 秒 时仍可用/); assert.equal(result.tone, "success");
  assert.match(result.explanation, /会话过期或不可用停止/);
});
test("只有后端明确确认不可复用时显示该结论", () => {
  const uncertain = experimentSummary(experiment({ state: "done" }), [probe("scheduled", "business_error", 8000)]);
  assert.doesNotMatch(uncertain.title, /已确认/);
  const confirmed = experimentSummary(experiment({ state: "done", verdict: "confirmed_unusable" }), [probe("scheduled", "business_error", 8000)]);
  assert.match(confirmed.title, /已确认原游标不可复用/);
});
test("计划包括未执行时点，结束后的未来时点不能显示为正在等待", () => {
  const e = experiment({ state: "done" }); const rows = plannedChecks(e, [probe("scheduled", "authentication_failed", 100)]);
  assert.equal(rows.length, 3); assert.equal(rows[1].scheduledAt, 4600); assert.equal(rows[1].status, "not_run");
  assert.equal(phaseText(e, 1100), "实验已结束");
});
test("缺测保留登记记录，但不成为成功证据", () => {
  const p = { ...probe("scheduled", "missed", 9000, 4600), request_id: null };
  const rows = plannedChecks(experiment(), [p]);assert.equal(rows[1].status, "missed");
  assert.equal(experimentSummary(experiment(), [p]).lastSuccessAge, null);
});
test("闸门等待和三步对照有独立进度文案", () => {
  assert.match(phaseText(experiment({ phase: "gate_wait", next_action_at: 1060 }), 1000), /等待列表间隔、预算或冷却.*1 分 0 秒后/);
  assert.match(phaseText(experiment({ phase: "control_page", next_action_at: null }), 1000), /新游标分页对照/);
});
test("自定义计划去重排序，拒绝空值、非数值及超出24小时的时点", () => {
  assert.deepEqual(parseSchedule("2,1,1,0.5"), [0.5, 1, 2]);
  for (const invalid of ["", "0", "-1", "25", "NaN", "Infinity"]) assert.equal(parseSchedule(invalid), null);
});
