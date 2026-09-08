import CoreServices
import Foundation

/// Decides which filesystem events under the working directory mean the change list may be
/// stale. Everything inside `.git` is dropped: the ref watcher owns that side, and object or
/// index churn never changes what the working tree looks like. Pure, so the filter is
/// testable without FSEvents.
public enum WorktreeWatchFilter {
    /// Whether an event at `path` can change the outcome of `worktreeStatus`.
    public static func isRelevant(_ path: String, workDir: String, gitDir: String) -> Bool {
        let root = normalized(workDir)
        guard path.hasPrefix(root) else { return false }
        let git = normalized(gitDir)
        if path == git || path.hasPrefix(git + "/") { return false }
        return true
    }

    private static func normalized(_ dir: String) -> String {
        dir.hasSuffix("/") ? String(dir.dropLast()) : dir
    }
}

/// Watches a repository's working directory with FSEvents and reports when the change list
/// may be stale. Events are coalesced by FSEvents' latency window and filtered through
/// `WorktreeWatchFilter`, so one `git checkout` or editor save produces one callback.
///
/// `onChange` fires on a private queue; hop to the main actor before touching models.
public final class WorktreeWatcher: @unchecked Sendable {
    private let workDir: String
    private let gitDir: String
    private let latency: TimeInterval
    private let onChange: @Sendable () -> Void
    private let queue = DispatchQueue(label: "rc-git.worktree-watcher")
    private var stream: FSEventStreamRef?

    public init(
        workDir: String, gitDir: String, latency: TimeInterval = 0.5,
        onChange: @escaping @Sendable () -> Void
    ) {
        self.workDir = workDir
        self.gitDir = gitDir
        self.latency = latency
        self.onChange = onChange
    }

    deinit {
        stop()
    }

    /// Starts delivering events. Safe to call once; later calls do nothing.
    public func start() {
        queue.sync {
            guard stream == nil else { return }
            var context = FSEventStreamContext(
                version: 0,
                info: Unmanaged.passUnretained(self).toOpaque(),
                retain: nil, release: nil, copyDescription: nil
            )
            let flags = UInt32(
                kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagUseCFTypes
                    | kFSEventStreamCreateFlagNoDefer)
            guard
                let created = FSEventStreamCreate(
                    kCFAllocatorDefault, worktreeEventCallback, &context,
                    [workDir] as CFArray, FSEventStreamEventId(kFSEventStreamEventIdSinceNow),
                    latency, flags)
            else { return }
            FSEventStreamSetDispatchQueue(created, queue)
            FSEventStreamStart(created)
            stream = created
        }
    }

    /// Stops delivering events and releases the stream.
    public func stop() {
        let stopped: FSEventStreamRef? = queue.sync {
            defer { stream = nil }
            return stream
        }
        guard let stopped else { return }
        FSEventStreamStop(stopped)
        FSEventStreamInvalidate(stopped)
        FSEventStreamRelease(stopped)
    }

    fileprivate func handle(paths: [String]) {
        if paths.contains(where: {
            WorktreeWatchFilter.isRelevant($0, workDir: workDir, gitDir: gitDir)
        }) {
            onChange()
        }
    }
}

private func worktreeEventCallback(
    _ stream: ConstFSEventStreamRef,
    _ info: UnsafeMutableRawPointer?,
    _ count: Int,
    _ eventPaths: UnsafeMutableRawPointer,
    _ flags: UnsafePointer<FSEventStreamEventFlags>,
    _ ids: UnsafePointer<FSEventStreamEventId>
) {
    guard let info else { return }
    let watcher = Unmanaged<WorktreeWatcher>.fromOpaque(info).takeUnretainedValue()
    guard let paths = unsafeBitCast(eventPaths, to: CFArray.self) as? [String] else { return }
    watcher.handle(paths: paths)
}
