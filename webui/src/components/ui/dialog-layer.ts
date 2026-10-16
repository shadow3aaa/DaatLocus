import { createContext } from "react"

export type DialogLayer = {
  parent: DialogLayer | null
  childDismissals: WeakSet<Event>
}

export const DialogLayerContext = createContext<DialogLayer | null>(null)

// Radix dialogs defer outside dismissal until click, while menus close on
// pointerdown. Remember that same gesture even after the menu has unmounted.
export function dismissDialogPopup(layer: DialogLayer | null, event: Event) {
  for (let owner = layer; owner; owner = owner.parent) {
    owner.childDismissals.add(event)
  }
}
