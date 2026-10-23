---
name: macos-gui-use
description: Complete macOS GUI tasks with ComputerUse on the virtual display, including window layout, controls and handoffs.
---

Discover the intended app and window before acquiring it. Prefer launch_app for
an app that is not running; it acquires only a confirmed new window. For a running
app, use list_windows and explicitly open the intended target. Do not take a
user's working window merely because its app name matches.

Acquired windows move onto the device's shared virtual display without activating
the app or moving the user's pointer. Work there throughout the task; overlapping
windows can still be captured individually. Keep the returned window references
when working across apps rather than switching foreground apps.

Inspect the acquired window, choose an action from its current contents and
supported_actions, then check the returned observation for the intended result.
For content outside the viewport, scroll or use adjust_window to fit or resize
the window on the virtual display. Move and resize arguments use logical points;
click, scroll and drag coordinates use pixels in the latest returned image.
Use a cropped, magnified get_window_state for small visible details. After a
geometry change, inspect the fresh image and select the editable control again
before entering text. Check the actual value before submitting it.

When an app opens a dialog or child window, discover its windows again and open
the returned child as a separate target. Resolve a blocking dialog there, then
observe the parent before resuming its task. Use a visible supported button when
the selected control cannot perform the needed key. A virtual display does not
make custom surfaces accept native background input; if the required action is
unsupported, return the window to the user and explain the remaining step.

Keep intermediate work on the virtual display. Use give_window when a result is
ready for the user, choosing original, center, left_half or right_half placement
on a user display. Use release_window to return an acquired window to its original
placement without closing the app. Each window, including separately acquired
dialogs, needs its own handoff. Old references are revoked; discover and explicitly
open again to reacquire. If a return is unconfirmed, input stays revoked; use
release_window to finish cleanup before reacquiring it. Display settings belong
to the Owner; applying them returns acquired windows and requires fresh discovery.

Refresh the observation after geometry changes. After an uncertain launch or
input, timeout or capture failure, inspect actual windows and their current state
before deciding the next action. Do not replay the uncertain effect. A minimized
window can be restored with adjust_window; a null state_id permits geometry
recovery after capture failure.
