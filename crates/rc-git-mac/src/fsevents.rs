//! Ref and worktree watchers on `FSEvents` (`RefWatcher.swift`, `WorktreeWatcher.swift`), with
//! the same flags (`FileEvents | UseCFTypes | NoDefer`), latencies and exclusion paths. Each
//! stream delivers on a private dispatch queue and filters through the kit's
//! [`RefFilter`]/[`WorktreeFilter`].
//!
//! `FSEvents` delivers a burst such as one `git fetch` as several callbacks: one at once
//! (`NoDefer`), then the rest after the latency window, possibly in more than one chunk.
//! Consumers coalesce.

use std::ffi::{CStr, c_void};
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex, PoisonError};

use rc_git_kit::platform::watch::{
    OnChange, REF_LATENCY, RefFilter, WORKTREE_LATENCY, Watcher, WatcherFactory, WatcherStartError,
    WorktreeDirs, WorktreeFilter,
};

use crate::cf::{self, CFTypeRef, Owned};

type CFIndex = isize;
const FLAG_USE_CF_TYPES: u32 = 0x0000_0001;
const FLAG_NO_DEFER: u32 = 0x0000_0002;
const FLAG_FILE_EVENTS: u32 = 0x0000_0010;
const EVENT_ID_SINCE_NOW: u64 = u64::MAX;

#[repr(C)]
struct StreamContext {
    version: CFIndex,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    copy_description: Option<unsafe extern "C" fn(*const c_void) -> CFTypeRef>,
}

type StreamCallback = unsafe extern "C" fn(
    stream: *const c_void,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
);

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(
        allocator: CFTypeRef,
        callback: StreamCallback,
        context: *mut StreamContext,
        paths: CFTypeRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> *mut c_void;
    fn FSEventStreamSetDispatchQueue(stream: *mut c_void, queue: *mut c_void);
    fn FSEventStreamSetExclusionPaths(stream: *mut c_void, paths: CFTypeRef) -> u8;
    fn FSEventStreamStart(stream: *mut c_void) -> u8;
    fn FSEventStreamStop(stream: *mut c_void);
    fn FSEventStreamInvalidate(stream: *mut c_void);
    fn FSEventStreamRelease(stream: *mut c_void);
}

// libdispatch is part of libSystem, which every binary links.
unsafe extern "C" {
    fn dispatch_queue_create(
        label: *const std::ffi::c_char,
        attributes: *const c_void,
    ) -> *mut c_void;
    fn dispatch_release(object: *mut c_void);
}

/// What a stream's callback filters with.
enum Filter {
    Refs(RefFilter),
    Worktree(WorktreeFilter),
}

/// The state a stream's callback reaches through its `info` pointer. Shared by an `Arc`
/// whose count the stream's retain and release callbacks move.
struct Handler {
    filter: Filter,
    on_change: OnChange,
}

impl Handler {
    fn handle(&self, paths: &[String]) {
        let relevant = match &self.filter {
            Filter::Refs(filter) => filter.any_relevant(paths),
            Filter::Worktree(filter) => filter.any_relevant(paths),
        };
        if relevant {
            (self.on_change)();
        }
    }

    fn watched_paths(&self) -> Vec<String> {
        match &self.filter {
            Filter::Refs(filter) => vec![filter.watched_path().to_owned()],
            Filter::Worktree(filter) => filter.watched_paths(),
        }
    }

    fn excluded_paths(&self) -> Vec<String> {
        match &self.filter {
            Filter::Refs(_) => Vec::new(),
            Filter::Worktree(filter) => filter.excluded_paths(),
        }
    }
}

unsafe extern "C" fn retain_handler(info: *const c_void) -> *const c_void {
    // SAFETY: `info` came from `Arc::as_ptr` on a live `Arc<Handler>`, and FSEvents balances
    // this retain with `release_handler`.
    unsafe { Arc::increment_strong_count(info.cast::<Handler>()) };
    info
}

unsafe extern "C" fn release_handler(info: *const c_void) {
    // SAFETY: balances one `retain_handler` on the same pointer.
    unsafe { Arc::decrement_strong_count(info.cast::<Handler>()) };
}

unsafe extern "C" fn stream_callback(
    _stream: *const c_void,
    info: *mut c_void,
    _count: usize,
    paths: *mut c_void,
    _flags: *const u32,
    _ids: *const u64,
) {
    if info.is_null() || paths.is_null() {
        return;
    }
    // SAFETY: the stream holds a retained `Arc<Handler>` behind `info` while it may call back,
    // and with `UseCFTypes` the paths are a CF array of CF strings owned by the stream.
    unsafe {
        let handler = &*info.cast_const().cast::<Handler>();
        let Some(rc_git_kit::platform::settings::Value::Array(items)) = cf::to_value(paths) else {
            return;
        };
        let mut strings = Vec::with_capacity(items.len());
        for item in items {
            if let Some(text) = item.as_str() {
                strings.push(text.to_owned());
            }
        }
        handler.handle(&strings);
    }
}

/// A started stream, released when stopped.
struct Stream(NonNull<c_void>);

// SAFETY: an FSEventStreamRef may be stopped, invalidated and released from any thread; this
// crate only does so under the watcher's lock.
unsafe impl Send for Stream {}

/// A private serial dispatch queue the stream delivers on.
struct Queue(NonNull<c_void>);

// SAFETY: dispatch queues are thread-safe objects.
unsafe impl Send for Queue {}
// SAFETY: as above; `&Queue` is only passed to dispatch calls.
unsafe impl Sync for Queue {}

impl Drop for Queue {
    fn drop(&mut self) {
        // SAFETY: releases the one reference `dispatch_queue_create` returned.
        unsafe { dispatch_release(self.0.as_ptr()) };
    }
}

/// An `FSEvents` watcher. Dropping it stops the stream.
pub struct FsEventsWatcher {
    handler: Arc<Handler>,
    latency: std::time::Duration,
    stream: Mutex<Option<Stream>>,
    queue: Option<Queue>,
}

impl FsEventsWatcher {
    fn new(
        filter: Filter,
        latency: std::time::Duration,
        on_change: OnChange,
        label: &CStr,
    ) -> Self {
        // SAFETY: `label` is a NUL-terminated string the call copies; null attributes make a
        // serial queue.
        let queue = NonNull::new(unsafe { dispatch_queue_create(label.as_ptr(), ptr::null()) });
        Self {
            handler: Arc::new(Handler { filter, on_change }),
            latency,
            stream: Mutex::new(None),
            queue: queue.map(Queue),
        }
    }

    /// A watcher reporting when the refs may have changed, rooted at the common dir.
    #[must_use]
    pub fn refs(git_dir: &str, common_dir: &str, on_change: OnChange) -> Self {
        let filter = Filter::Refs(RefFilter::new(git_dir, common_dir));
        Self::new(filter, REF_LATENCY, on_change, c"rc-git.ref-watcher")
    }

    /// A watcher reporting when the change list may be stale.
    #[must_use]
    pub fn worktree(dirs: &WorktreeDirs, on_change: OnChange) -> Self {
        let filter = Filter::Worktree(WorktreeFilter::new(dirs));
        Self::new(
            filter,
            WORKTREE_LATENCY,
            on_change,
            c"rc-git.worktree-watcher",
        )
    }

    /// The directories the stream is rooted at.
    #[must_use]
    pub fn watched_paths(&self) -> Vec<String> {
        self.handler.watched_paths()
    }

    fn path_array(paths: &[String]) -> Option<Owned> {
        let mut strings = Vec::with_capacity(paths.len());
        for path in paths {
            strings.push(cf::string(path)?);
        }
        cf::array(&strings)
    }

    fn create(
        &self,
        failed: &dyn Fn(&'static str) -> WatcherStartError,
    ) -> Result<Stream, WatcherStartError> {
        let paths =
            Self::path_array(&self.watched_paths()).ok_or_else(|| failed("CFArrayCreate"))?;
        let mut context = StreamContext {
            version: 0,
            info: Arc::as_ptr(&self.handler).cast_mut().cast(),
            retain: Some(retain_handler),
            release: Some(release_handler),
            copy_description: None,
        };
        // SAFETY: `paths` is a live CF array of strings, the context is copied by the call and
        // retains the handler through `retain_handler`; the result is owned by us.
        let created = unsafe {
            FSEventStreamCreate(
                ptr::null(),
                stream_callback,
                &raw mut context,
                paths.as_ptr(),
                EVENT_ID_SINCE_NOW,
                self.latency.as_secs_f64(),
                FLAG_FILE_EVENTS | FLAG_USE_CF_TYPES | FLAG_NO_DEFER,
            )
        };
        NonNull::new(created)
            .map(Stream)
            .ok_or_else(|| failed("FSEventStreamCreate"))
    }

    fn release(stream: &Stream) {
        // SAFETY: `stream` was created by `create` and is released exactly once, here.
        unsafe {
            FSEventStreamStop(stream.0.as_ptr());
            FSEventStreamInvalidate(stream.0.as_ptr());
            FSEventStreamRelease(stream.0.as_ptr());
        }
    }
}

impl Watcher for FsEventsWatcher {
    fn start(&self) -> Result<(), WatcherStartError> {
        let mut running = self.stream.lock().unwrap_or_else(PoisonError::into_inner);
        if running.is_some() {
            return Ok(());
        }
        let root = self.watched_paths().first().cloned().unwrap_or_default();
        let failed = |call| WatcherStartError {
            call,
            path: root.clone(),
        };
        let queue = self
            .queue
            .as_ref()
            .ok_or_else(|| failed("dispatch_queue_create"))?;
        let stream = self.create(&failed)?;
        let excluded = self.handler.excluded_paths();
        if !excluded.is_empty()
            && let Some(excluded) = Self::path_array(&excluded)
        {
            // A refused exclusion list only costs filtered callbacks: the filter drops every
            // `.git` path the stream would have excluded.
            // SAFETY: the stream is live and not started; `excluded` is a CF array of strings.
            let _ = unsafe { FSEventStreamSetExclusionPaths(stream.0.as_ptr(), excluded.as_ptr()) };
        }
        // SAFETY: the stream and the queue are live; the stream retains the queue.
        let started = unsafe {
            FSEventStreamSetDispatchQueue(stream.0.as_ptr(), queue.0.as_ptr());
            FSEventStreamStart(stream.0.as_ptr())
        };
        if started == 0 {
            // SAFETY: the stream never started; invalidate and release it once.
            unsafe {
                FSEventStreamInvalidate(stream.0.as_ptr());
                FSEventStreamRelease(stream.0.as_ptr());
            }
            return Err(failed("FSEventStreamStart"));
        }
        *running = Some(stream);
        Ok(())
    }

    fn stop(&self) {
        let stopped = self
            .stream
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(stream) = stopped {
            Self::release(&stream);
        }
    }

    fn set_pruned_dirs(&self, ignored: &[String], nested: &[String]) {
        if let Filter::Worktree(filter) = &self.handler.filter {
            filter.set_pruned_dirs(ignored, nested);
        }
    }
}

impl Drop for FsEventsWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The [`WatcherFactory`] that makes [`FsEventsWatcher`]s.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsEventsWatchers;

impl WatcherFactory for FsEventsWatchers {
    fn ref_watcher(
        &self,
        git_dir: &str,
        common_dir: &str,
        on_change: OnChange,
    ) -> Box<dyn Watcher> {
        Box::new(FsEventsWatcher::refs(git_dir, common_dir, on_change))
    }

    fn worktree_watcher(&self, dirs: &WorktreeDirs, on_change: OnChange) -> Box<dyn Watcher> {
        Box::new(FsEventsWatcher::worktree(dirs, on_change))
    }
}

#[cfg(test)]
mod tests;
