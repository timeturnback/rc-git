import Dispatch

/// Runs blocking work — a call into the core, a keychain access, a network transfer — on a
/// dedicated dispatch queue and resumes the caller through a continuation.
///
/// Swift's cooperative pool has one thread per core, so blocking work run on it (inside
/// `Task.detached`, say) holds a thread no other task can use; enough of it waiting at once
/// starves every task, the ones that would unblock it included. This queue overcommits: GCD
/// adds a thread when one blocks. `scripts/check.sh` fails on a `Task.detached` in RcGitKit
/// that does not say why it never blocks.
///
/// Task cancellation does not reach `body`, which runs outside any task. Work that must stop
/// early takes a `CancelToken`, or splits into steps the caller awaits one at a time.
enum BlockingWork {
    private static let queue = DispatchQueue(
        label: "rc-git.blocking-work", attributes: .concurrent)

    /// Runs `body` off the cooperative pool and returns its value. `body` must capture only
    /// Sendable locals, never a model; wrap a throwing call in `Result` and `get()` it after.
    static func run<T: Sendable>(
        qos: DispatchQoS.QoSClass = .userInitiated, _ body: @escaping @Sendable () -> T
    ) async -> T {
        await withCheckedContinuation { (continuation: CheckedContinuation<T, Never>) in
            queue.async(qos: DispatchQoS(qosClass: qos, relativePriority: 0)) {
                continuation.resume(returning: body())
            }
        }
    }
}
