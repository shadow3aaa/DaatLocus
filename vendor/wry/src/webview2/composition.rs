//! Window-local GPU composition. HWNDs are input receivers, never visual cutouts.
use std::{
  cell::{Cell, RefCell},
  collections::HashMap,
  rc::{Rc, Weak},
};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::{
  core::{Interface, Result},
  Win32::{
    Foundation::*,
    Graphics::{DirectComposition::*, Gdi::ClientToScreen},
    UI::{
      Controls::WM_MOUSELEAVE,
      Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus},
      Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
      WindowsAndMessaging::*,
    },
  },
};

use windows::Win32::System::{
  Com::IDataObject,
  Ole::{IDropTarget, IDropTarget_Impl, RegisterDragDrop, RevokeDragDrop, DROPEFFECT},
  SystemServices::MODIFIERKEYS_FLAGS,
};

const SUBCLASS: usize = 0x444c434f;
thread_local! {
    static DEVICE:RefCell<Option<IDCompositionDevice>>=const{RefCell::new(None)};
    static ROOTS:RefCell<HashMap<isize,Weak<Root>>>=RefCell::new(HashMap::new());
    static VIEWS:RefCell<HashMap<isize,Rc<RefCell<View>>>>=RefCell::new(HashMap::new());
}
struct Root {
  device: IDCompositionDevice,
  _target: IDCompositionTarget,
  visual: IDCompositionVisual,
  resizing: Cell<bool>,
}
struct View {
  environment: ICoreWebView2Environment3,
  controller: ICoreWebView2Controller,
  composition: ICoreWebView2CompositionController,
  visual: IDCompositionVisual,
  root: Rc<Root>,
  parent: HWND,
  _drop_target: IDropTarget,
  passthrough: Option<InputRect>,
  foreground: Vec<InputRect>,
  tracking: bool,
  insets: (i32, i32, i32, i32),
  input_insets: Option<(i32, i32, i32, i32)>,
}
/// Physical input geometry. This never changes rendering, opacity or clipping.
#[derive(Clone, Copy, Debug)]
pub struct InputRect {
  pub left: i32,
  pub top: i32,
  pub right: i32,
  pub bottom: i32,
}
impl InputRect {
  fn contains(self, x: i32, y: i32) -> bool {
    x >= self.left && x < self.right && y >= self.top && y < self.bottom
  }
}

fn device() -> Result<IDCompositionDevice> {
  DEVICE.with(|slot| {
    if let Some(device) = slot.borrow().as_ref() {
      return Ok(device.clone());
    }
    let device: IDCompositionDevice =
      unsafe { DCompositionCreateDevice(None::<&windows::Win32::Graphics::Dxgi::IDXGIDevice>) }?;
    *slot.borrow_mut() = Some(device.clone());
    Ok(device)
  })
}
fn root(parent: HWND) -> Result<Rc<Root>> {
  ROOTS.with(|roots| {
    if let Some(root) = roots
      .borrow()
      .get(&(parent.0 as isize))
      .and_then(Weak::upgrade)
    {
      return Ok(root);
    }
    let device = device()?;
    let target = unsafe { device.CreateTargetForHwnd(parent, true) }?;
    let visual = unsafe { device.CreateVisual() }?;
    unsafe {
      target.SetRoot(&visual)?;
    }
    let root = Rc::new(Root {
      device,
      _target: target,
      visual,
      resizing: Cell::new(false),
    });
    roots
      .borrow_mut()
      .insert(parent.0 as isize, Rc::downgrade(&root));
    unsafe {
      SetWindowSubclass(parent, Some(parent_proc), SUBCLASS, 0).ok()?;
    }
    Ok(root)
  })
}

pub(super) fn attach(
  hwnd: HWND,
  controller: &ICoreWebView2Controller,
  environment: &ICoreWebView2Environment,
) -> Result<()> {
  let parent = unsafe { GetParent(hwnd) }?;
  let root = root(parent)?;
  let visual = unsafe { root.device.CreateVisual() }?;
  let composition: ICoreWebView2CompositionController = controller.cast()?;
  unsafe {
    root
      .visual
      .AddVisual(&visual, true, None::<&IDCompositionVisual>)?;
    composition.SetRootVisualTarget(&visual)?;
    SetWindowPos(
      hwnd,
      Some(HWND_BOTTOM),
      0,
      0,
      0,
      0,
      SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
    )?;
  }
  let drop_target: IDropTarget = CompositionDropTarget {
    hwnd,
    composition: composition.cast()?,
  }
  .into();
  unsafe {
    RegisterDragDrop(hwnd, &drop_target)?;
  }
  let environment = environment.cast()?;
  let insets = insets(hwnd)?;
  VIEWS.with(|views| {
    views.borrow_mut().insert(
      hwnd.0 as isize,
      Rc::new(RefCell::new(View {
        environment,
        controller: controller.clone(),
        composition,
        visual,
        root,
        parent,
        _drop_target: drop_target,
        passthrough: None,
        foreground: Vec::new(),
        tracking: false,
        insets,
        input_insets: None,
      })),
    )
  });
  unsafe {
    SetWindowSubclass(hwnd, Some(input_proc), SUBCLASS, 0).ok()?;
  }
  sync(hwnd)
}
pub(super) fn detach(hwnd: HWND) {
  if let Some(view) = VIEWS.with(|views| views.borrow_mut().remove(&(hwnd.0 as isize))) {
    let view = view.borrow();
    unsafe {
      let _ = RevokeDragDrop(hwnd);
      let _ = view.root.visual.RemoveVisual(&view.visual);
      let _ = view
        .composition
        .SetRootVisualTarget(None::<&windows::core::IUnknown>);
      let _ = view.root.device.Commit();
      let _ = RemoveWindowSubclass(hwnd, Some(input_proc), SUBCLASS);
    }
  }
}
pub(super) fn sync(hwnd: HWND) -> Result<()> {
  let Some(view) = VIEWS.with(|views| views.borrow().get(&(hwnd.0 as isize)).cloned()) else {
    return Ok(());
  };
  let mut view = view.borrow_mut();
  let mut reordered = false;
  unsafe {
    let parent = GetParent(hwnd)?;
    if parent != view.parent {
      view.root.visual.RemoveVisual(&view.visual)?;
      view.root.device.Commit()?;
      let next = root(parent)?;
      next
        .visual
        .AddVisual(&view.visual, true, None::<&IDCompositionVisual>)?;
      view.root = next;
      view.parent = parent;
      view.insets = insets(hwnd)?;
      reordered = true;
    }
    let mut origin = POINT::default();
    ClientToScreen(parent, &mut origin).ok()?;
    let mut rect = RECT::default();
    GetWindowRect(hwnd, &mut rect)?;
    view.visual.SetOffsetX2((rect.left - origin.x) as f32)?;
    view.visual.SetOffsetY2((rect.top - origin.y) as f32)?;
    if !view.root.resizing.get() {
      view.root.device.Commit()?;
    }
  }
  drop(view);
  if reordered {
    unsafe {
      SetWindowPos(
        hwnd,
        Some(HWND_BOTTOM),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
      )?;
    }
  }
  Ok(())
}
// Retain edge insets from explicit layout. Native WM_SIZE applies them inside
// the modal sizing loop, before JS or a Tauri async command can run.
fn insets(hwnd: HWND) -> Result<(i32, i32, i32, i32)> {
  unsafe {
    let parent = GetParent(hwnd)?;
    let mut size = RECT::default();
    GetClientRect(parent, &mut size)?;
    let mut point = POINT::default();
    ClientToScreen(parent, &mut point).ok()?;
    let mut rect = RECT::default();
    GetWindowRect(hwnd, &mut rect)?;
    Ok((
      rect.left - point.x,
      rect.top - point.y,
      size.right - (rect.right - point.x),
      size.bottom - (rect.bottom - point.y),
    ))
  }
}
pub(super) fn layout(hwnd: HWND, x: i32, y: i32, width: i32, height: i32) -> Result<()> {
  if let Some(view) = VIEWS.with(|views| views.borrow().get(&(hwnd.0 as isize)).cloned()) {
    let mut view = view.borrow_mut();
    let mut size = RECT::default();
    unsafe {
      GetClientRect(view.parent, &mut size)?;
    }
    view.insets = (x, y, size.right - x - width, size.bottom - y - height);
  }
  Ok(())
}
unsafe extern "system" fn parent_proc(
  hwnd: HWND,
  message: u32,
  wparam: WPARAM,
  lparam: LPARAM,
  _id: usize,
  _data: usize,
) -> LRESULT {
  if message == WM_NCDESTROY {
    unsafe {
      let _ = RemoveWindowSubclass(hwnd, Some(parent_proc), SUBCLASS);
    }
  }
  if message == WM_SIZE && wparam.0 != SIZE_MINIMIZED as usize {
    let root = ROOTS.with(|roots| {
      roots
        .borrow()
        .get(&(hwnd.0 as isize))
        .and_then(Weak::upgrade)
    });
    if let Some(root) = root {
      let mut size = RECT::default();
      if unsafe { GetClientRect(hwnd, &mut size) }.is_ok() {
        let views: Vec<_> = VIEWS.with(|views| {
          views
            .borrow()
            .iter()
            .filter_map(|(&handle, view)| {
              let mut view = view.borrow_mut();
              if view.parent != hwnd {
                return None;
              }
              if let Some((left, top, right, bottom)) = view.input_insets {
                view.passthrough = Some(InputRect {
                  left,
                  top,
                  right: size.right - right,
                  bottom: size.bottom - bottom,
                });
              }
              Some((HWND(handle as _), view.controller.clone(), view.insets))
            })
            .collect()
        });
        root.resizing.set(true);
        for (handle, controller, (left, top, right, bottom)) in views {
          unsafe {
            let width = (size.right - left - right).max(1);
            let height = (size.bottom - top - bottom).max(1);
            let _ = controller.SetBounds(RECT {
              left: 0,
              top: 0,
              right: width,
              bottom: height,
            });
            let _ = SetWindowPos(
              handle,
              None,
              left,
              top,
              width,
              height,
              SWP_NOACTIVATE | SWP_NOZORDER,
            );
          }
        }
        root.resizing.set(false);
        unsafe {
          let _ = root.device.Commit();
        }
      }
    }
  }
  unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
/// Configure the transparent shell's input routing above a native page.
/// Foreground rectangles come from ordinary DOM hit testing, not widget names.
pub fn set_input_regions(
  hwnd: isize,
  viewport: Option<InputRect>,
  foreground: Vec<InputRect>,
) -> Result<()> {
  if let Some(view) = VIEWS.with(|views| views.borrow().get(&hwnd).cloned()) {
    let mut view = view.borrow_mut();
    view.passthrough = viewport;
    let mut parent = RECT::default();
    unsafe {
      GetClientRect(view.parent, &mut parent)?;
    }
    view.input_insets = viewport.map(|r| {
      (
        r.left,
        r.top,
        parent.right - r.right,
        parent.bottom - r.bottom,
      )
    });
    view.foreground = foreground;
  }
  Ok(())
}
pub fn is_composed(hwnd: isize) -> bool {
  VIEWS.with(|views| views.borrow().contains_key(&hwnd))
}

unsafe extern "system" fn input_proc(
  hwnd: HWND,
  message: u32,
  wparam: WPARAM,
  lparam: LPARAM,
  _id: usize,
  _data: usize,
) -> LRESULT {
  let Some(view) = VIEWS.with(|views| views.borrow().get(&(hwnd.0 as isize)).cloned()) else {
    return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
  };
  if message == WM_NCDESTROY {
    detach(hwnd);
    return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
  }
  if message == WM_WINDOWPOSCHANGED {
    let _ = sync(hwnd);
  }
  if message == WM_NCHITTEST {
    let mut point = POINT {
      x: (lparam.0 as u16 as i16) as i32,
      y: ((lparam.0 >> 16) as u16 as i16) as i32,
    };
    unsafe {
      let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point);
    }
    let view = view.borrow();
    if view
      .passthrough
      .is_some_and(|area| area.contains(point.x, point.y))
      && !view
        .foreground
        .iter()
        .any(|area| area.contains(point.x, point.y))
    {
      return LRESULT(HTTRANSPARENT as isize);
    }
  }
  if message == WM_SETFOCUS {
    let controller = view.borrow().controller.clone();
    let _ = unsafe { controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC) };
    // The windowed Wry default proc focuses its first child. Composition owns
    // its keyboard/IME focus target through MoveFocus instead.
    return LRESULT(0);
  }
  if message == WM_SETCURSOR {
    let mut cursor = windows::Win32::UI::WindowsAndMessaging::HCURSOR::default();
    if unsafe { view.borrow().composition.Cursor(&mut cursor) }.is_ok() {
      unsafe {
        SetCursor(Some(cursor));
      }
      return LRESULT(1);
    }
  }
  if message == WM_GETOBJECT
    && lparam.0 as i32 == windows::Win32::UI::Accessibility::UiaRootObjectId
  {
    let composition = view.borrow().composition.clone();
    if let Ok(provider) = composition
      .cast::<ICoreWebView2CompositionController2>()
      .and_then(|controller| unsafe { controller.AutomationProvider() })
      .and_then(|provider| {
        provider.cast::<windows::Win32::UI::Accessibility::IRawElementProviderSimple>()
      })
    {
      return unsafe {
        windows::Win32::UI::Accessibility::UiaReturnRawElementProvider(
          hwnd, wparam, lparam, &provider,
        )
      };
    }
  }
  if matches!(
    message,
    WM_POINTERDOWN
      | WM_POINTERUP
      | WM_POINTERUPDATE
      | WM_POINTERENTER
      | WM_POINTERLEAVE
      | WM_POINTERACTIVATE
  ) {
    let (environment, composition) = {
      let view = view.borrow();
      (view.environment.clone(), view.composition.clone())
    };
    if message == WM_POINTERACTIVATE {
      return LRESULT(PA_ACTIVATE as isize);
    }
    if unsafe { pointer_input(hwnd, message, wparam, &environment, &composition) }.is_ok() {
      return LRESULT(0);
    }
  }
  let kind = match message {
    WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK
    | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_RBUTTONDBLCLK | WM_MBUTTONDOWN | WM_MBUTTONUP
    | WM_MBUTTONDBLCLK | WM_MOUSEWHEEL | WM_MOUSEHWHEEL | WM_XBUTTONDOWN | WM_XBUTTONUP
    | WM_XBUTTONDBLCLK => Some(COREWEBVIEW2_MOUSE_EVENT_KIND(message as i32)),
    _ => None,
  };
  if let Some(kind) = kind {
    let (controller, composition, track_mouse) = {
      let mut view = view.borrow_mut();
      let track = message == WM_MOUSEMOVE && !view.tracking;
      if track {
        view.tracking = true;
      }
      if message == WM_MOUSELEAVE {
        view.tracking = false;
      }
      (view.controller.clone(), view.composition.clone(), track)
    };
    let mut point = POINT {
      x: (lparam.0 as u16 as i16) as i32,
      y: ((lparam.0 >> 16) as u16 as i16) as i32,
    };
    let data = if matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
      (wparam.0 >> 16) as i16 as i32 as u32
    } else if matches!(message, WM_XBUTTONDOWN | WM_XBUTTONUP | WM_XBUTTONDBLCLK) {
      ((wparam.0 >> 16) & 0xffff) as u32
    } else {
      0
    };
    unsafe {
      if matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point);
      }
      if matches!(
        message,
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
      ) {
        let _ = SetFocus(Some(hwnd));
        let _ = SetCapture(hwnd);
        let _ = controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
      }
      if matches!(
        message,
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP
      ) {
        let _ = ReleaseCapture();
      }
      if track_mouse {
        let mut track = windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT {
          cbSize: std::mem::size_of::<windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT>(
          ) as u32,
          dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::TME_LEAVE,
          hwndTrack: hwnd,
          dwHoverTime: 0,
        };
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::TrackMouseEvent(&mut track);
      }
      if message == WM_MOUSELEAVE {
        point = POINT::default();
      }
      let _ = composition.SendMouseInput(
        kind,
        COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS((wparam.0 & 0xffff) as i32),
        data,
        point,
      );
    }
    return LRESULT(0);
  }
  unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

// Preserve native touch/pen geometry and pressure. No synthetic page JS input.
unsafe fn pointer_input(
  hwnd: HWND,
  message: u32,
  wparam: WPARAM,
  environment: &ICoreWebView2Environment3,
  composition: &ICoreWebView2CompositionController,
) -> Result<()> {
  use windows::Win32::UI::Input::Pointer::*;
  unsafe {
    let id = (wparam.0 & 0xffff) as u32;
    let mut native = POINTER_INFO::default();
    GetPointerInfo(id, &mut native)?;
    let info = environment.CreateCoreWebView2PointerInfo()?;
    let local = |mut point: POINT| {
      let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point);
      point
    };
    let rect = |value: RECT| {
      let p = local(POINT {
        x: value.left,
        y: value.top,
      });
      RECT {
        left: p.x,
        top: p.y,
        right: p.x + value.right - value.left,
        bottom: p.y + value.bottom - value.top,
      }
    };
    info.SetPointerKind(native.pointerType.0 as u32)?;
    info.SetPointerId(id)?;
    info.SetFrameId(native.frameId)?;
    info.SetPointerFlags(native.pointerFlags.0)?;
    let (mut device, mut display) = (RECT::default(), RECT::default());
    GetPointerDeviceRects(native.sourceDevice, &mut device, &mut display)?;
    info.SetPointerDeviceRect(device)?;
    info.SetDisplayRect(display)?;
    info.SetPixelLocation(local(native.ptPixelLocation))?;
    info.SetPixelLocationRaw(local(native.ptPixelLocationRaw))?;
    info.SetHimetricLocation(native.ptHimetricLocation)?;
    info.SetHimetricLocationRaw(native.ptHimetricLocationRaw)?;
    info.SetTime(native.dwTime)?;
    info.SetHistoryCount(native.historyCount)?;
    info.SetInputData(native.InputData)?;
    info.SetKeyStates(native.dwKeyStates)?;
    info.SetPerformanceCount(native.PerformanceCount)?;
    info.SetButtonChangeKind(native.ButtonChangeType.0)?;
    if native.pointerType == PT_PEN {
      let mut pen = POINTER_PEN_INFO::default();
      GetPointerPenInfo(id, &mut pen)?;
      info.SetPenFlags(pen.penFlags)?;
      info.SetPenMask(pen.penMask)?;
      info.SetPenPressure(pen.pressure)?;
      info.SetPenRotation(pen.rotation)?;
      info.SetPenTiltX(pen.tiltX)?;
      info.SetPenTiltY(pen.tiltY)?;
    } else if native.pointerType == PT_TOUCH {
      let mut touch = POINTER_TOUCH_INFO::default();
      GetPointerTouchInfo(id, &mut touch)?;
      info.SetTouchFlags(touch.touchFlags)?;
      info.SetTouchMask(touch.touchMask)?;
      info.SetTouchContact(rect(touch.rcContact))?;
      info.SetTouchContactRaw(rect(touch.rcContactRaw))?;
      info.SetTouchOrientation(touch.orientation)?;
      info.SetTouchPressure(touch.pressure)?;
    }
    composition.SendPointerInput(COREWEBVIEW2_POINTER_EVENT_KIND(message as i32), &info)
  }
}

#[windows::core::implement(IDropTarget)]
struct CompositionDropTarget {
  hwnd: HWND,
  composition: ICoreWebView2CompositionController3,
}
impl CompositionDropTarget {
  fn local(&self, point: &POINTL) -> POINT {
    let mut point = POINT {
      x: point.x,
      y: point.y,
    };
    unsafe {
      let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut point);
    }
    point
  }
}
#[allow(non_snake_case)]
impl IDropTarget_Impl for CompositionDropTarget_Impl {
  fn DragEnter(
    &self,
    data: windows::core::Ref<'_, IDataObject>,
    keys: MODIFIERKEYS_FLAGS,
    point: &POINTL,
    effect: *mut DROPEFFECT,
  ) -> Result<()> {
    unsafe {
      self
        .composition
        .DragEnter(data.as_ref(), keys.0, self.local(point), effect.cast())
    }
  }
  fn DragOver(
    &self,
    keys: MODIFIERKEYS_FLAGS,
    point: &POINTL,
    effect: *mut DROPEFFECT,
  ) -> Result<()> {
    unsafe {
      self
        .composition
        .DragOver(keys.0, self.local(point), effect.cast())
    }
  }
  fn DragLeave(&self) -> Result<()> {
    unsafe { self.composition.DragLeave() }
  }
  fn Drop(
    &self,
    data: windows::core::Ref<'_, IDataObject>,
    keys: MODIFIERKEYS_FLAGS,
    point: &POINTL,
    effect: *mut DROPEFFECT,
  ) -> Result<()> {
    unsafe {
      self
        .composition
        .Drop(data.as_ref(), keys.0, self.local(point), effect.cast())
    }
  }
}
