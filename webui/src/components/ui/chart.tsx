import * as React from "react"
import * as RechartsPrimitive from "recharts"

import { cn } from "@/lib/utils"

const THEMES = { light: "", dark: ".dark" } as const

type ChartTooltipPayloadItem = {
  color?: string
  dataKey?: string | number
  name?: string | number
  value?: React.ReactNode
}

export type ChartConfig = {
  [key: string]: {
    label?: React.ReactNode
    color?: string
  }
}

type ChartContextProps = {
  config: ChartConfig
}

const ChartContext = React.createContext<ChartContextProps | null>(null)

function useChart() {
  const context = React.useContext(ChartContext)

  if (!context) {
    throw new Error("useChart must be used within a <ChartContainer />")
  }

  return context
}

function ChartContainer({
  id,
  className,
  children,
  config,
  ...props
}: React.ComponentProps<"div"> & {
  config: ChartConfig
  children: React.ComponentProps<
    typeof RechartsPrimitive.ResponsiveContainer
  >["children"]
}) {
  const uniqueId = React.useId()
  const chartId = `chart-${id || uniqueId.replace(/:/g, "")}`

  return (
    <ChartContext.Provider value={{ config }}>
      <div
        data-slot="chart"
        data-chart={chartId}
        className={cn(
          "flex aspect-video justify-center text-xs text-muted-foreground [&_.recharts-cartesian-axis-tick_text]:fill-muted-foreground [&_.recharts-grid_line[stroke='#ccc']]:stroke-border/50 [&_.recharts-tooltip-cursor]:fill-muted [&_.recharts-tooltip-cursor]:opacity-40",
          className
        )}
        {...props}
      >
        <ChartStyle
          id={chartId}
          config={config}
        />
        <RechartsPrimitive.ResponsiveContainer>
          {children}
        </RechartsPrimitive.ResponsiveContainer>
      </div>
    </ChartContext.Provider>
  )
}

function ChartStyle({ id, config }: { id: string; config: ChartConfig }) {
  const colorConfig = Object.entries(config).filter(([, item]) => item.color)

  if (!colorConfig.length) {
    return null
  }

  return (
    <style
      dangerouslySetInnerHTML={{
        __html: Object.entries(THEMES)
          .map(
            ([_theme, prefix]) => `
${prefix} [data-chart=${id}] {
${colorConfig
  .map(([key, item]) => `  --color-${key}: ${item.color};`)
  .join("\n")}
}
`
          )
          .join("\n"),
      }}
    />
  )
}

function ChartTooltip({
  ...props
}: React.ComponentProps<typeof RechartsPrimitive.Tooltip>) {
  return <RechartsPrimitive.Tooltip {...props} />
}

function ChartTooltipContent({
  active,
  payload,
  className,
  label,
  valueFormatter,
}: React.ComponentProps<"div"> & {
    active?: boolean
    payload?: ChartTooltipPayloadItem[]
    label?: React.ReactNode
    valueFormatter?: (value: React.ReactNode) => React.ReactNode
  }) {
  const { config } = useChart()

  if (!active || !payload?.length) {
    return null
  }

  return (
    <div
      className={cn(
        "grid min-w-32 gap-1.5 rounded-lg border bg-background px-2.5 py-1.5 text-xs shadow-xl",
        className
      )}
    >
      {label ? (
        <div className="font-medium text-foreground">{label}</div>
      ) : null}
      <div className="grid gap-1">
        {payload.map((item, index) => {
          const key = String(item.dataKey ?? item.name ?? index)
          const itemConfig = config[key]
          const value = valueFormatter ? valueFormatter(item.value) : item.value

          return (
            <div
              key={key}
              className="flex min-w-0 items-center gap-2"
            >
              <span
                className="size-2 shrink-0 rounded-[2px]"
                style={{
                  backgroundColor:
                    item.color ?? itemConfig?.color ?? `var(--color-${key})`,
                }}
              />
              <span className="min-w-0 flex-1 text-muted-foreground">
                {itemConfig?.label ?? item.name}
              </span>
              <span className="font-mono font-medium tabular-nums text-foreground">
                {value as React.ReactNode}
              </span>
            </div>
          )
        })}
      </div>
    </div>
  )
}

export { ChartContainer, ChartTooltip, ChartTooltipContent }
