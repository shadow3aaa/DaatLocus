import { Activity, Boxes, CheckCircle2, Server } from "lucide-react";

import { Button } from "@/components/ui/button";

const foundations = [
  "Vite + React + TypeScript",
  "Yarn v4 Plug'n'Play",
  "Tailwind CSS + shadcn/ui 基础",
  "可独立开发，也可由 daemon serve 静态资源",
];

const nextMilestones = [
  "定义 daemon WebUI API 边界",
  "接入运行状态与任务列表",
  "补充日志流 SSE/WebSocket",
];

export default function App() {
  return (
    <main className="min-h-screen bg-background text-foreground">
      <section className="mx-auto flex min-h-screen w-full max-w-6xl flex-col px-6 py-10">
        <div className="mb-10 flex items-center gap-3 text-sm text-muted-foreground">
          <div className="flex size-9 items-center justify-center rounded-lg border bg-card text-card-foreground shadow-sm">
            <Activity className="size-4" />
          </div>
          <span>Daat Locus WebUI foundation</span>
        </div>

        <div className="grid flex-1 items-center gap-10 lg:grid-cols-[1.1fr_0.9fr]">
          <div className="space-y-8">
            <div className="space-y-5">
              <div className="inline-flex items-center rounded-full border bg-card px-3 py-1 text-sm text-muted-foreground shadow-sm">
                WebUI skeleton initialized
              </div>
              <h1 className="max-w-3xl text-4xl font-semibold tracking-tight sm:text-6xl">
                轻量、可独立运行、可内置到 daemon 的管理界面。
              </h1>
              <p className="max-w-2xl text-lg leading-8 text-muted-foreground">
                当前页面用于验证 Vite / React / TypeScript / Tailwind / shadcn/ui
                基础链路。后续会逐步接入 daemon 的状态、任务、日志和控制接口。
              </p>
            </div>

            <div className="flex flex-col gap-3 sm:flex-row">
              <Button size="lg">查看当前状态</Button>
              <Button size="lg" variant="outline">
                规划下一步
              </Button>
            </div>
          </div>

          <div className="rounded-2xl border bg-card p-6 text-card-foreground shadow-sm">
            <div className="mb-6 flex items-center justify-between gap-4">
              <div>
                <p className="text-sm text-muted-foreground">Implementation status</p>
                <h2 className="mt-1 text-2xl font-semibold tracking-tight">基础已就绪</h2>
              </div>
              <div className="rounded-full bg-primary/10 p-3 text-primary">
                <Boxes className="size-5" />
              </div>
            </div>

            <div className="space-y-3">
              {foundations.map((item) => (
                <div key={item} className="flex items-start gap-3 rounded-xl border bg-background/60 p-3">
                  <CheckCircle2 className="mt-0.5 size-5 shrink-0 text-primary" />
                  <span className="text-sm leading-6">{item}</span>
                </div>
              ))}
            </div>

            <div className="mt-6 rounded-xl border bg-muted/40 p-4">
              <div className="mb-3 flex items-center gap-2 font-medium">
                <Server className="size-4" />
                daemon integration next
              </div>
              <ul className="space-y-2 text-sm text-muted-foreground">
                {nextMilestones.map((item) => (
                  <li key={item} className="flex gap-2">
                    <span className="text-primary">•</span>
                    <span>{item}</span>
                  </li>
                ))}
              </ul>
            </div>
          </div>
        </div>
      </section>
    </main>
  );
}
