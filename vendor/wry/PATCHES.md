# Daat Locus Wry patch

Based on crates.io `wry` 0.57.0 (Apache-2.0 / MIT), without examples or build artifacts.

The opt-in Windows `native-composition` feature replaces windowed WebView2 controllers
with CompositionController. Views in one native parent share a DirectComposition
visual tree; the first surface (the transparent shell) stays above later webpage
surfaces. Input HWNDs follow that order. DOM foreground rectangles only route input;
they never clip or redraw a webpage. Native mouse, touch, pen, focus, accessibility and external drag/drop
are forwarded through WebView2's composition APIs. Resize and reparent update native
geometry synchronously, without hiding the view or copying frames. Edge insets are
applied directly inside the parent HWND sizing loop, independently of JavaScript
and Tauri event scheduling.

The upstream controller path and non-Windows backends remain available. Reparent
also keeps child-view parent bookkeeping current for bounds queries. The app
explicitly enables the feature on Windows. Keep this patch narrow when updating Wry;
the opt-in native integration tests live in `clients/app/src-tauri/tests/browser_live.rs`.
