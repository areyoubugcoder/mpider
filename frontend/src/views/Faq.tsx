/** 当前工作台 FAQ：与公开文档共用内容，离线解释采集限制、数据隐私及账号边界。 */
import { useState } from "react";
import ReactMarkdown from "react-markdown";
import faq from "../../../docs/faq.md?raw";
import { Button } from "@/components/ui/button";
import { CollapsibleSection } from "@/components/ui/collapsible-section";
import type { View } from "@/views/types";

const questions = faq.split(/^## /m).slice(1).map(section => {
  const [title, ...body] = section.trim().split("\n");
  return { title, body: body.join("\n").trim() };
});

export function Faq({ onNav }: { onNav: (view: View) => void }) {
  const [expanded, setExpanded] = useState(() => new Set(questions.slice(0, 3).map(question => question.title)));
  const allOpen = expanded.size === questions.length;
  return <div className="mx-auto max-w-4xl space-y-5">
    <header className="space-y-2">
      <h1 className="text-xl font-semibold">常见问题（FAQ）</h1>
      <p className="text-sm text-muted-foreground">当前 Windows 工作台的采集限制、运行环境与数据隐私。区分已验证范围和未确认结论，不承诺免封控。</p>
      <div className="flex flex-wrap items-center gap-2 pt-1">
        <Button variant="outline" size="sm" onClick={() => setExpanded(new Set(allOpen ? [] : questions.map(question => question.title)))}>{allOpen ? "全部收起" : "全部展开"}</Button>
        <Button variant="ghost" size="sm" onClick={() => onNav("settings")}>系统设置</Button>
        <Button variant="ghost" size="sm" onClick={() => onNav("ratelimit")}>限流配置</Button>
        <Button variant="ghost" size="sm" onClick={() => onNav("logs")}>系统日志</Button>
      </div>
    </header>
    <div className="space-y-3">{questions.map(({ title, body }) => <CollapsibleSection key={title} title={title} open={expanded.has(title)} onOpenChange={open => setExpanded(current => {
      const next = new Set(current);
      if (open) next.add(title); else next.delete(title);
      return next;
    })} contentClassName="prose prose-sm max-w-none p-4 pt-0 text-sm dark:prose-invert prose-p:my-2 prose-strong:text-foreground">
      <ReactMarkdown>{body}</ReactMarkdown>
    </CollapsibleSection>)}</div>
  </div>;
}
