import * as React from "react"
import { ContextMenu as ContextMenuPrimitive } from "radix-ui"
import { cn } from "@/lib/utils"
import { DialogLayerContext, dismissDialogPopup } from "@/components/ui/dialog-layer"

function ContextMenu(props: React.ComponentProps<typeof ContextMenuPrimitive.Root>) {
  return <ContextMenuPrimitive.Root data-slot="context-menu" {...props} />
}

function ContextMenuTrigger(props: React.ComponentProps<typeof ContextMenuPrimitive.Trigger>) {
  return <ContextMenuPrimitive.Trigger data-slot="context-menu-trigger" {...props} />
}

function ContextMenuGroup(props: React.ComponentProps<typeof ContextMenuPrimitive.Group>) {
  return <ContextMenuPrimitive.Group data-slot="context-menu-group" {...props} />
}

function ContextMenuContent({ className, onPointerDownOutside, ...props }: React.ComponentProps<typeof ContextMenuPrimitive.Content>) {
  const dialog = React.useContext(DialogLayerContext)
  return <ContextMenuPrimitive.Portal>
    <ContextMenuPrimitive.Content
      data-slot="context-menu-content"
      className={cn("z-50 max-h-(--radix-context-menu-content-available-height) min-w-40 origin-(--radix-context-menu-content-transform-origin) overflow-x-hidden overflow-y-auto rounded-lg bg-popover p-1 text-popover-foreground shadow-md ring-1 ring-foreground/10 duration-100 data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95 data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95", className)}
      {...props}
      onPointerDownOutside={event => {
        dismissDialogPopup(dialog, event.detail.originalEvent)
        onPointerDownOutside?.(event)
      }}
    />
  </ContextMenuPrimitive.Portal>
}

function ContextMenuItem({ className, ...props }: React.ComponentProps<typeof ContextMenuPrimitive.Item>) {
  return <ContextMenuPrimitive.Item
    data-slot="context-menu-item"
    className={cn("relative flex cursor-default items-center gap-1.5 rounded-md px-3 py-2.5 text-sm outline-hidden select-none focus:bg-accent focus:text-accent-foreground data-disabled:pointer-events-none data-disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4", className)}
    {...props}
  />
}

export { ContextMenu, ContextMenuTrigger, ContextMenuContent, ContextMenuGroup, ContextMenuItem }
