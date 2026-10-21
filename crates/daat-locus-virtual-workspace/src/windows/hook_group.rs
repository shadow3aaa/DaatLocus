use super::{HookReceipt, InputSurface, KeyModifiers, MouseAction, Window, hook::ThreadHook};
use crate::{Error, Point, Result};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// One controlled visual window with independently hosted input threads. The
/// frame remains the identity for screenshots, placement, lifecycle and handoff.
pub struct WindowHook {
    root: Window,
    dll: PathBuf,
    threads: Vec<ThreadHook>,
    stopped: bool,
    accessibility_guards: bool,
}
impl WindowHook {
    pub fn attach(target: &Window, dll: impl AsRef<Path>) -> Result<Self> {
        Self::attach_with_mode(target, dll.as_ref(), false)
    }
    pub(super) fn activation_guards(target: &Window, dll: &Path) -> Result<Self> {
        Self::attach_with_mode(target, dll, true)
    }
    fn attach_with_mode(target: &Window, dll: &Path, accessibility_guards: bool) -> Result<Self> {
        let mut group = Self {
            root: target.clone(),
            dll: dll.to_owned(),
            threads: Vec::new(),
            stopped: false,
            accessibility_guards,
        };
        group.sync_surfaces()?;
        Ok(group)
    }
    pub fn input_enabled(&self) -> bool {
        !self.stopped && self.threads.first().is_some_and(ThreadHook::input_enabled)
    }
    pub fn reobserve(&mut self) -> Result<Window> {
        if !self.input_enabled() {
            return Err(Error::InvalidState("window input has been revoked"));
        }
        let current = Window::inspect(self.root.handle)?;
        if current.process_id != self.root.process_id
            || current.process_started != self.root.process_started
            || current.thread_id != self.root.thread_id
        {
            self.revoke();
            return Err(Error::InvalidState("controlled window identity changed"));
        }
        self.root = current;
        let mut index = 0;
        while index < self.threads.len() {
            if index > 0
                && (self.threads[index].surface.validate(&self.root).is_err()
                    || !self.threads[index].input_enabled())
            {
                self.threads[index].stop()?;
                self.threads.remove(index);
            } else {
                self.threads[index].reobserve()?;
                index += 1;
            }
        }
        self.sync_surfaces()?;
        Ok(self.root.clone())
    }
    fn sync_surfaces(&mut self) -> Result<()> {
        let result = (|| {
            for surface in self.root.input_surfaces()? {
                let frame = surface.handle == self.root.handle;
                if let Err(error) = self.thread_for(surface) {
                    if self.accessibility_guards
                        && !frame
                        && super::input::signature_restriction(&error)
                    {
                        continue;
                    }
                    return Err(error);
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.revoke();
        }
        result
    }
    fn thread_for(&mut self, surface: InputSurface) -> Result<usize> {
        if self.stopped {
            return Err(Error::InvalidState("window input has been revoked"));
        }
        surface.validate(&self.root)?;
        if let Some(index) = self.threads.iter().position(|hook| {
            hook.surface.process_id == surface.process_id
                && hook.surface.thread_id == surface.thread_id
                && hook.surface.process_started == surface.process_started
        }) {
            if self.threads[index].surface.validate(&self.root).is_ok()
                && self.threads[index].input_enabled()
            {
                return Ok(index);
            }
            if index == 0 {
                return Err(Error::InvalidState("frame input hook is unavailable"));
            }
            self.threads[index].stop()?;
            self.threads.remove(index);
        }
        let hook =
            ThreadHook::attach(&self.root, surface.clone(), &self.dll).map_err(|source| {
                Error::InputSurface {
                    process_id: surface.process_id,
                    thread_id: surface.thread_id,
                    class: surface.class,
                    source: Box::new(source),
                }
            })?;
        self.threads.push(hook);
        Ok(self.threads.len() - 1)
    }
    fn route(
        &mut self,
        handle: usize,
        action: impl FnOnce(&mut ThreadHook) -> Result<HookReceipt>,
    ) -> Result<HookReceipt> {
        if !self.input_enabled() {
            return Err(Error::InvalidState("window input has been revoked"));
        }
        let index = self.prepare_route(handle)?;
        let result = action(&mut self.threads[index]);
        if !self.threads[index].input_enabled() {
            self.revoke();
        }
        result
    }
    /// Resolve and handshake a route before marking any application effect started.
    pub fn prepare(&mut self, handle: usize) -> Result<()> {
        self.prepare_route(handle).map(|_| ())
    }
    fn prepare_route(&mut self, handle: usize) -> Result<usize> {
        if !self.input_enabled() {
            return Err(Error::InvalidState("window input has been revoked"));
        }
        self.root.validate_discovery()?;
        let surface = InputSurface::inspect(&self.root, handle)?;
        match self.thread_for(surface) {
            Ok(index) => Ok(index),
            Err(error)
                if self.accessibility_guards && super::input::signature_restriction(&error) =>
            {
                // No input was dispatched. Only this surface needs UIA; retain
                // the frame's activation guards and other native routes.
                Err(error)
            }
            Err(error) => {
                self.revoke();
                Err(error)
            }
        }
    }
    pub fn request_close(&mut self, window: usize, timeout: Duration) -> Result<HookReceipt> {
        self.route(window, |hook| hook.request_close(window, timeout))
    }
    pub fn text(&mut self, control: usize, text: &str, timeout: Duration) -> Result<HookReceipt> {
        self.route(control, |hook| hook.text(control, text, timeout))
    }
    pub fn key(
        &mut self,
        control: usize,
        key: u16,
        modifiers: KeyModifiers,
        timeout: Duration,
    ) -> Result<HookReceipt> {
        self.route(control, |hook| hook.key(control, key, modifiers, timeout))
    }
    pub fn mouse(
        &mut self,
        control: usize,
        action: MouseAction,
        local: Point,
        timeout: Duration,
    ) -> Result<HookReceipt> {
        self.route(control, |hook| hook.mouse(control, action, local, timeout))
    }
    pub fn drag(
        &mut self,
        control: usize,
        path: &[Point],
        timeout: Duration,
    ) -> Result<HookReceipt> {
        self.route(control, |hook| hook.drag(control, path, timeout))
    }
    pub fn veto_counts(&self) -> (u32, u32) {
        self.threads
            .iter()
            .map(ThreadHook::veto_counts)
            .fold((0u32, 0u32), |a, b| {
                (a.0.saturating_add(b.0), a.1.saturating_add(b.1))
            })
    }
    fn revoke(&mut self) {
        self.stopped = true;
        for hook in &self.threads {
            hook.revoke();
        }
    }
    pub fn stop(&mut self) -> Result<()> {
        self.revoke();
        // Revoke every route before waiting on any target's in-flight handler.
        let mut failure = None;
        for hook in &mut self.threads {
            if let Err(error) = hook.stop()
                && failure.is_none()
            {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    pub fn close(mut self) -> Result<()> {
        self.stop()
    }
}
impl Drop for WindowHook {
    fn drop(&mut self) {
        self.revoke();
    }
}
