/** 当前工作台实际写入的日志模块；旧流程仅保留历史日志读取，不列入业务筛选。 */
export const SYSTEM_LOG_MODULE_LABELS: Record<string, string> = {
  app: "应用",
  discovery: "主页识别",
  push_monitor: "推送监测",
  history: "文章采集",
  experiment: "游标实验",
  archive: "正文下载",
  export: "数据导出",
  storage: "数据存储",
  policy: "请求策略",
  list: "列表请求",
};
