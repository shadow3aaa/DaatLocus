//! A service-owned screen outlives individual window/Actor leases. All topology
//! work stays on its owning thread; ordinary reservations never reconfigure it.
use super::{DpiContext, VirtualDisplay, last_error};
use crate::{Display, DisplayOptions, Error, Point, Rect, Result};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock, Weak, mpsc},
    thread::JoinHandle,
};

enum Request {
    Inspect(mpsc::SyncSender<Result<Option<Display>>>),
    Capture(mpsc::SyncSender<Result<Option<super::Frame>>>),
    Configure(
        bool,
        DisplayOptions,
        mpsc::SyncSender<Result<Option<Display>>>,
    ),
    Initialize(mpsc::SyncSender<Result<Display>>),
    Reserve(i32, i32, mpsc::SyncSender<Result<(u64, Display, Rect)>>),
    Release(u64),
    Validate(u64, mpsc::SyncSender<Result<()>>),
    Replace(
        u64,
        Option<(i32, i32)>,
        Option<Point>,
        mpsc::SyncSender<Result<(u64, Display, Rect)>>,
    ),
}
struct Inner {
    requests: Option<mpsc::SyncSender<Request>>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Keep a handle in service infrastructure, independently of App instances.
/// Explicit initialization or the first reservation creates the monitor; releasing
/// the last region leaves it active. The last service/region handle removes it.
#[derive(Clone)]
pub struct SharedDisplay(Arc<Inner>);
impl SharedDisplay {
    /// Observe only an existing owned output. Never creates or recovers a screen.
    pub fn capture(&self) -> Result<Option<super::Frame>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.0
            .requests
            .as_ref()
            .expect("live owner")
            .send(Request::Capture(reply))
            .map_err(|_| Error::InvalidState("shared display owner stopped"))?;
        response
            .recv()
            .map_err(|_| Error::InvalidState("shared display owner stopped"))?
    }
    /// Read the current output without creating or recovering a monitor.
    pub fn inspect(&self) -> Result<Option<Display>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.0
            .requests
            .as_ref()
            .expect("live owner")
            .send(Request::Inspect(reply))
            .map_err(|_| Error::InvalidState("shared display owner stopped"))?;
        response
            .recv()
            .map_err(|_| Error::InvalidState("shared display owner stopped"))?
    }
    /// Reconfigure the same output after all window regions have been returned.
    /// Disabling removes the output and fences subsequent reservations.
    pub fn configure(&self, enabled: bool, options: DisplayOptions) -> Result<Option<Display>> {
        options.validate()?;
        let (reply, response) = mpsc::sync_channel(1);
        self.0
            .requests
            .as_ref()
            .expect("live owner")
            .send(Request::Configure(enabled, options, reply))
            .map_err(|_| Error::InvalidState("shared display owner stopped"))?;
        response
            .recv()
            .map_err(|_| Error::InvalidState("shared display owner stopped"))?
    }
    /// One default screen shared by service owners in this process. Separate
    /// Windows processes/profiles are separate services, not a cross-process broker.
    pub fn global() -> Result<Self> {
        static GLOBAL: OnceLock<Mutex<Weak<Inner>>> = OnceLock::new();
        let mut global = GLOBAL
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| Error::InvalidState("shared display registry is unavailable"))?;
        if let Some(inner) = global.upgrade() {
            return Ok(Self(inner));
        }
        let service = Self::new(DisplayOptions::default())?;
        *global = Arc::downgrade(&service.0);
        Ok(service)
    }

    /// An independently owned service for native hosts/tests. Does not create a
    /// monitor until initialize() or a window reservation requests it.
    pub fn new(options: DisplayOptions) -> Result<Self> {
        options.validate()?;
        let (requests, receiver) = mpsc::sync_channel(16);
        let worker = std::thread::Builder::new()
            .name("dl-shared-display".into())
            .spawn(move || run(receiver, options))
            .map_err(|source| Error::Windows {
                operation: "start shared display owner",
                source,
            })?;
        Ok(Self(Arc::new(Inner {
            requests: Some(requests),
            worker: Some(worker),
        })))
    }

    /// Create the output and settle its work area before accepting desktop work.
    /// Repeated calls reuse the existing monitor without reserving window space.
    pub fn initialize(&self) -> Result<Display> {
        let (reply, response) = mpsc::sync_channel(1);
        self.0
            .requests
            .as_ref()
            .expect("live owner")
            .send(Request::Initialize(reply))
            .map_err(|_| Error::InvalidState("shared display owner stopped"))?;
        response
            .recv()
            .map_err(|_| Error::InvalidState("shared display owner stopped"))?
    }

    /// Reserve a placement lease, retaining it until the window is physically
    /// returned. Prefer free space, then cascade without shrinking existing windows.
    /// Oversized windows fit this fixed screen; reservations never add outputs.
    pub fn reserve(&self, width: i32, height: i32) -> Result<DisplayRegion> {
        if width <= 0 || height <= 0 {
            return Err(Error::InvalidState("invalid shared window size"));
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.0
            .requests
            .as_ref()
            .expect("live owner")
            .send(Request::Reserve(width, height, reply))
            .map_err(|_| Error::InvalidState("shared display owner stopped"))?;
        let (token, display, bounds) = response
            .recv()
            .map_err(|_| Error::InvalidState("shared display owner stopped"))??;
        Ok(DisplayRegion {
            owner: self.clone(),
            token,
            display,
            bounds,
        })
    }
}

/// Window placement lease on the shared output; bounds may overlap. Keep this guard while a window
/// is controlled or its handoff is pending. It does not grant input authority.
pub struct DisplayRegion {
    owner: SharedDisplay,
    token: u64,
    display: Display,
    bounds: Rect,
}
impl DisplayRegion {
    /// Plan a new region while retaining the old guard. None size selects the
    /// screen work area with margins. Origin, when supplied, is local to the display.
    /// Keep both guards until native placement has been observed; a timeout must
    /// not free either possible position while the result remains uncertain.
    pub fn replacement(&self, size: Option<(i32, i32)>, origin: Option<Point>) -> Result<Self> {
        let (reply, response) = mpsc::sync_channel(1);
        self.owner
            .0
            .requests
            .as_ref()
            .expect("live owner")
            .send(Request::Replace(self.token, size, origin, reply))
            .map_err(|_| Error::InvalidState("shared display owner stopped"))?;
        let (token, display, bounds) = response
            .recv()
            .map_err(|_| Error::InvalidState("shared display owner stopped"))??;
        Ok(Self {
            owner: self.owner.clone(),
            token,
            display,
            bounds,
        })
    }
    pub fn display(&self) -> &Display {
        &self.display
    }
    pub fn bounds(&self) -> Rect {
        self.bounds
    }
    pub fn validate(&self) -> Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.owner
            .0
            .requests
            .as_ref()
            .expect("live owner")
            .send(Request::Validate(self.token, reply))
            .map_err(|_| Error::InvalidState("shared display owner stopped"))?;
        response
            .recv()
            .map_err(|_| Error::InvalidState("shared display owner stopped"))?
    }
}
impl Drop for DisplayRegion {
    fn drop(&mut self) {
        let _ = self
            .owner
            .0
            .requests
            .as_ref()
            .expect("live owner")
            .send(Request::Release(self.token));
    }
}

fn run(receiver: mpsc::Receiver<Request>, mut options: DisplayOptions) {
    let mut enabled = true;
    let mut display: Option<VirtualDisplay> = None;
    let mut snapshot: Option<Display> = None;
    let mut regions = BTreeMap::new();
    let mut next = 0u64;
    while let Ok(request) = receiver.recv() {
        match request {
            Request::Inspect(reply) => {
                let _ = reply.send(display.as_ref().map(VirtualDisplay::display).transpose());
            }
            Request::Capture(reply) => {
                let result = (|| {
                    let Some(output) = display.as_ref().filter(|_| enabled) else {
                        return Ok(None);
                    };
                    let observed = output.display()?;
                    if snapshot.as_ref() != Some(&observed) {
                        return Err(Error::InvalidState(
                            "shared display changed; observation unavailable",
                        ));
                    }
                    let frame = super::capture::capture(observed.bounds)?;
                    if output.display()? != observed {
                        return Err(Error::InvalidState("shared display changed during capture"));
                    }
                    Ok(Some(frame))
                })();
                let _ = reply.send(result);
            }
            Request::Configure(next_enabled, next_options, reply) => {
                let result = (|| {
                    if !regions.is_empty() {
                        return Err(Error::InvalidState(
                            "return all windows before configuring the shared display",
                        ));
                    }
                    if !next_enabled {
                        enabled = false;
                        if let Some(owned) = display.take() {
                            owned.remove()?;
                        }
                        snapshot = None;
                        options = next_options;
                        return Ok(None);
                    }
                    if let Some(owned) = display.as_mut() {
                        let observed = owned.display()?;
                        if observed.bounds.width()? as u32 != next_options.width
                            || observed.bounds.height()? as u32 != next_options.height
                        {
                            let mut requested = next_options;
                            requested.origin = requested.origin.or(Some(Point {
                                x: observed.bounds.left,
                                y: observed.bounds.top,
                            }));
                            owned.configure(requested)?;
                            let observed = owned.display()?;
                            settle_work_area(observed.bounds)?;
                            snapshot = Some(observed);
                        }
                    }
                    let observed =
                        prepare_display(&mut display, &mut snapshot, next_options, false)?;
                    options = next_options;
                    enabled = true;
                    Ok(Some(observed))
                })();
                let _ = reply.send(result);
            }
            Request::Initialize(reply) => {
                let _ = reply.send(if !enabled {
                    Err(Error::InvalidState("GUI operation is disabled"))
                } else {
                    prepare_display(&mut display, &mut snapshot, options, !regions.is_empty())
                });
            }
            Request::Replace(token, size, origin, reply) => {
                let result = (|| {
                    let old = *regions
                        .get(&token)
                        .ok_or(Error::InvalidState("shared display region was released"))?;
                    let observed = display
                        .as_ref()
                        .ok_or(Error::InvalidState("shared display is unavailable"))?
                        .display()?;
                    if snapshot.as_ref() != Some(&observed) {
                        return Err(Error::InvalidState(
                            "shared display changed; release existing windows before reacquiring",
                        ));
                    }
                    let area = work_area(observed.bounds)?;
                    let origin = origin
                        .map(|point| {
                            Ok(Point {
                                x: observed
                                    .bounds
                                    .left
                                    .checked_add(point.x)
                                    .ok_or(Error::InvalidState("layout coordinate overflow"))?,
                                y: observed
                                    .bounds
                                    .top
                                    .checked_add(point.y)
                                    .ok_or(Error::InvalidState("layout coordinate overflow"))?,
                            })
                        })
                        .transpose()?;
                    let occupied: Vec<_> = regions
                        .iter()
                        .filter(|(id, _)| **id != token)
                        .map(|(_, bounds)| *bounds)
                        .collect();
                    let bounds = layout(area, old, size, origin, &occupied)?;
                    next = next
                        .checked_add(1)
                        .ok_or(Error::InvalidState("shared region token exhausted"))?;
                    regions.insert(next, bounds);
                    Ok((next, observed, bounds))
                })();
                if let Err(mpsc::SendError(Ok((token, _, _)))) = reply.send(result) {
                    regions.remove(&token);
                }
            }
            Request::Release(token) => {
                regions.remove(&token);
            }
            Request::Validate(token, reply) => {
                let result = (|| {
                    if !regions.contains_key(&token) {
                        return Err(Error::InvalidState("shared display region was released"));
                    }
                    let observed = display
                        .as_ref()
                        .ok_or(Error::InvalidState("shared display is unavailable"))?
                        .display()?;
                    if snapshot.as_ref() != Some(&observed) {
                        return Err(Error::InvalidState(
                            "shared display changed; release existing windows before reacquiring",
                        ));
                    }
                    Ok(())
                })();
                let _ = reply.send(result);
            }
            Request::Reserve(width, height, reply) => {
                let result = (|| {
                    if !enabled {
                        return Err(Error::InvalidState("GUI operation is disabled"));
                    }
                    let current =
                        prepare_display(&mut display, &mut snapshot, options, !regions.is_empty())?;
                    let bounds = allocate(
                        work_area(current.bounds)?,
                        width,
                        height,
                        regions.values().copied(),
                    )?;
                    next = next
                        .checked_add(1)
                        .ok_or(Error::InvalidState("shared region token exhausted"))?;
                    regions.insert(next, bounds);
                    Ok((next, current, bounds))
                })();
                // A caller disappearing before receipt cannot strand a region.
                if let Err(mpsc::SendError(Ok((token, _, _)))) = reply.send(result) {
                    regions.remove(&token);
                }
            }
        }
    }
    if let Some(display) = display {
        let _ = display.remove();
    }
}

fn prepare_display(
    display: &mut Option<VirtualDisplay>,
    snapshot: &mut Option<Display>,
    options: DisplayOptions,
    has_regions: bool,
) -> Result<Display> {
    if let Some(owned) = display.as_ref() {
        match owned.display() {
            Ok(observed) => {
                if has_regions && snapshot.as_ref() != Some(&observed) {
                    return Err(Error::InvalidState(
                        "shared display changed; release existing windows before reacquiring",
                    ));
                }
                *snapshot = Some(observed);
            }
            Err(error) if has_regions => return Err(error),
            Err(_) => {
                display.take();
                *snapshot = None;
            }
        }
    }
    if display.is_none() {
        let owned = VirtualDisplay::create(options)?;
        let observed = owned.display()?;
        // Explorer may establish the secondary taskbar after the mode is active.
        // Neither startup readiness nor window leases use that transient work area.
        settle_work_area(observed.bounds)?;
        *snapshot = Some(observed);
        *display = Some(owned);
    }
    Ok(snapshot.as_ref().expect("active display").clone())
}

fn allocate(
    area: Rect,
    width: i32,
    height: i32,
    occupied: impl Iterator<Item = Rect>,
) -> Result<Rect> {
    const GAP: i32 = 20;
    if area.width()? <= 2 * GAP || area.height()? <= 2 * GAP {
        return Err(Error::InvalidState("shared screen work area is too small"));
    }
    let width = width.min(area.width()? - 2 * GAP);
    let height = height.min(area.height()? - 2 * GAP);
    let occupied: Vec<_> = occupied.collect();
    let mut xs = vec![area.left + GAP];
    let mut ys = vec![area.top + GAP];
    xs.extend(occupied.iter().filter_map(|r| r.right.checked_add(GAP)));
    ys.extend(occupied.iter().filter_map(|r| r.bottom.checked_add(GAP)));
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();
    for top in ys {
        for &left in &xs {
            let (Some(right), Some(bottom)) = (left.checked_add(width), top.checked_add(height))
            else {
                continue;
            };
            let bounds = Rect {
                left,
                top,
                right,
                bottom,
            };
            if bounds.right <= area.right - GAP
                && bounds.bottom <= area.bottom - GAP
                && occupied.iter().all(|r| !overlaps(bounds, *r))
            {
                return Ok(bounds);
            }
        }
    }
    // Free rectangles are a placement preference, not an admission limit. Cycle
    // a bounded diagonal offset; even a screen-sized window can share the origin.
    let room_x = area.width()? - 2 * GAP - width;
    let room_y = area.height()? - 2 * GAP - height;
    let slots = (room_x.min(room_y) / 32 + 1) as usize;
    let offset = (occupied.len() % slots) as i32 * 32;
    Ok(Rect {
        left: area.left + GAP + offset,
        top: area.top + GAP + offset,
        right: area.left + GAP + offset + width,
        bottom: area.top + GAP + offset + height,
    })
}
fn work_area(bounds: Rect) -> Result<Rect> {
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromRect},
    };
    let _dpi = DpiContext::enter()?;
    let rectangle = RECT {
        left: bounds.left,
        top: bounds.top,
        right: bounds.right,
        bottom: bounds.bottom,
    };
    let monitor = unsafe { MonitorFromRect(&rectangle, MONITOR_DEFAULTTONULL) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if monitor.is_null() || unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return Err(last_error("query shared display work area"));
    }
    let area = Rect {
        left: info.rcWork.left,
        top: info.rcWork.top,
        right: info.rcWork.right,
        bottom: info.rcWork.bottom,
    };
    if !bounds.contains_rect(area) {
        return Err(Error::InvalidState("shared display work area changed"));
    }
    Ok(area)
}
fn settle_work_area(bounds: Rect) -> Result<()> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut previous = work_area(bounds)?;
    let mut since = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let current = work_area(bounds)?;
        if current != previous {
            previous = current;
            since = Instant::now();
        }
        if since.elapsed() >= Duration::from_secs(1) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Timeout("shared display work area did not settle"));
        }
    }
}
fn overlaps(a: Rect, b: Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

fn layout(
    area: Rect,
    old: Rect,
    size: Option<(i32, i32)>,
    origin: Option<Point>,
    occupied: &[Rect],
) -> Result<Rect> {
    const GAP: i32 = 20;
    let usable = Rect {
        left: area.left + GAP,
        top: area.top + GAP,
        right: area.right - GAP,
        bottom: area.bottom - GAP,
    };
    if let Some((width, height)) = size {
        if width <= 0 || height <= 0 || width > usable.width()? || height > usable.height()? {
            return Err(Error::InvalidState(
                "requested window size does not fit; use fit or a smaller size",
            ));
        }
        let at = |point: Point| -> Option<Rect> {
            let bounds = Rect {
                left: point.x,
                top: point.y,
                right: point.x.checked_add(width)?,
                bottom: point.y.checked_add(height)?,
            };
            usable.contains_rect(bounds).then_some(bounds)
        };
        if let Some(origin) = origin {
            return at(origin).ok_or(Error::InvalidState(
                "requested position leaves the work area",
            ));
        }
        if let Some(bounds) = at(Point {
            x: old.left,
            y: old.top,
        }) {
            return Ok(bounds);
        }
        return allocate(area, width, height, occupied.iter().copied());
    }
    if origin.is_some() {
        return Err(Error::InvalidState("fit does not accept a position"));
    }
    // Fit is independent of other windows: each window can use the full viewport.
    usable.width()?;
    usable.height()?;
    Ok(usable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observation_of_an_uninitialized_owner_never_creates_an_output() {
        let owner = SharedDisplay::new(Default::default()).unwrap();
        assert!(owner.capture().unwrap().is_none());
        assert!(owner.inspect().unwrap().is_none());
        owner.configure(false, Default::default()).unwrap();
        assert!(owner.capture().unwrap().is_none());
        assert!(owner.inspect().unwrap().is_none());
    }
    #[test]
    fn replacement_preserves_position_allows_overlap_and_fits_the_work_area() {
        let area = Rect {
            left: 0,
            top: 0,
            right: 1000,
            bottom: 800,
        };
        let old = Rect {
            left: 20,
            top: 20,
            right: 220,
            bottom: 220,
        };
        let other = Rect {
            left: 600,
            top: 20,
            right: 980,
            bottom: 780,
        };
        assert_eq!(
            layout(area, old, Some((500, 400)), None, &[other]).unwrap(),
            Rect {
                left: 20,
                top: 20,
                right: 520,
                bottom: 420
            }
        );
        assert_eq!(
            layout(area, old, None, None, &[other]).unwrap(),
            Rect {
                left: 20,
                top: 20,
                right: 980,
                bottom: 780
            }
        );
        assert_eq!(
            layout(area, old, None, None, &[]).unwrap(),
            Rect {
                left: 20,
                top: 20,
                right: 980,
                bottom: 780
            }
        );
        for (size, origin) in [
            ((961, 100), None),
            ((100, 761), None),
            ((0, 100), None),
            ((300, 300), Some(Point { x: -1, y: 20 })),
            ((300, 300), Some(Point { x: i32::MAX, y: 20 })),
        ] {
            assert!(layout(area, old, Some(size), origin, &[other]).is_err());
        }
        assert_eq!(
            layout(
                area,
                old,
                Some((200, 200)),
                Some(Point { x: 300, y: 400 }),
                &[other]
            )
            .unwrap(),
            Rect {
                left: 300,
                top: 400,
                right: 500,
                bottom: 600
            }
        );
        let below = Rect {
            left: 20,
            top: 20,
            right: 980,
            bottom: 400,
        };
        assert_eq!(
            layout(area, old, None, None, &[below]).unwrap(),
            Rect {
                left: 20,
                top: 20,
                right: 980,
                bottom: 780
            }
        );
        assert_eq!(
            layout(
                area,
                old,
                None,
                None,
                &[Rect {
                    left: 20,
                    top: 20,
                    right: 980,
                    bottom: 780
                }]
            )
            .unwrap(),
            Rect {
                left: 20,
                top: 20,
                right: 980,
                bottom: 780
            }
        );
        assert!(
            layout(
                area,
                old,
                Some((300, 300)),
                Some(Point { x: 500, y: 20 }),
                &[other]
            )
            .is_ok()
        );
    }
    #[test]
    fn reservations_prefer_free_space_then_cascade_without_resizing_peers() {
        let area = Rect {
            left: -1920,
            top: -1080,
            right: 0,
            bottom: 0,
        };
        let a = allocate(area, 900, 1000, [].into_iter()).unwrap();
        let b = allocate(area, 900, 1000, [a].into_iter()).unwrap();
        assert_eq!(
            a,
            Rect {
                left: -1900,
                top: -1060,
                right: -1000,
                bottom: -60
            }
        );
        assert_eq!(
            b,
            Rect {
                left: -980,
                top: -1060,
                right: -80,
                bottom: -60
            }
        );
        let c = allocate(area, 900, 1000, [a, b].into_iter()).unwrap();
        assert_eq!((c.width().unwrap(), c.height().unwrap()), (900, 1000));
        assert!(area.contains_rect(c));
        assert!(overlaps(c, a) || overlaps(c, b));
        let mut occupied = vec![a, b, c];
        for _ in 0..100 {
            let next = allocate(area, 900, 1000, occupied.iter().copied()).unwrap();
            assert!(area.contains_rect(next));
            assert_eq!((next.width().unwrap(), next.height().unwrap()), (900, 1000));
            occupied.push(next);
        }
        assert_eq!(allocate(area, 900, 1000, [b].into_iter()).unwrap(), a);
        assert_eq!(
            allocate(area, 5000, 5000, [].into_iter()).unwrap(),
            Rect {
                left: -1900,
                top: -1060,
                right: -20,
                bottom: -20
            }
        );
    }
}
