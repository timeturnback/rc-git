import Foundation
import Testing

@testable import RcGitKit

/// What the update window says and enables, per bundle and state.
@Suite struct UpdateCopyTests {
    private static func build(
        variant: AppVariant = .release, name: String = "rc-git", sourceRoot: String? = "/checkout"
    ) -> AppBuild {
        AppBuild(
            variant: variant, name: name, version: "0.1.0", build: "42",
            sourceRoot: sourceRoot, commit: "a3f9", dirty: false, buildDate: nil)
    }

    private static let release = build()
    private static let dev = build(variant: .dev, name: "rc-git (dev)")
    private static let unstamped = build(sourceRoot: nil)

    @Test func theTitleNamesTheBundleInBothModes() {
        #expect(UpdateCopy(build: Self.release, state: .idle).title == "Update rc-git")
        #expect(UpdateCopy(build: Self.dev, state: .idle).title == "Rebuild rc-git (dev)")
    }

    @Test func theHeadingAndSourceFollowTheMode() {
        let release = UpdateCopy(build: Self.release, state: .idle)
        #expect(release.heading == "Installed")
        #expect(release.source == "Builds from /checkout, fast-forwarded from its upstream first.")
        let dev = UpdateCopy(build: Self.dev, state: .idle)
        #expect(dev.heading == "Running")
        #expect(dev.source.hasPrefix("Rebuilds /checkout as it stands now"))
        let unstamped = UpdateCopy(build: Self.unstamped, state: .idle)
        #expect(unstamped.source.contains("cannot rebuild itself"))
    }

    @Test func idleOffersTheModesAction() {
        let release = UpdateCopy(build: Self.release, state: .idle)
        #expect(release.status == "Fetch, rebuild, install, relaunch. Tabs come back.")
        #expect(release.actionTitle == "Update and Relaunch")
        #expect(release.actionEnabled)
        #expect(!release.spinning)
        let dev = UpdateCopy(build: Self.dev, state: .idle)
        #expect(dev.status == "Rebuild and relaunch. Tabs come back.")
        #expect(dev.actionTitle == "Rebuild and Relaunch")
        #expect(dev.actionEnabled)
    }

    @Test func anUnstampedBundleCannotAct() {
        #expect(!UpdateCopy(build: Self.unstamped, state: .idle).actionEnabled)
        let failed = UpdateState.failed(step: .preflight, message: "no checkout")
        #expect(!UpdateCopy(build: Self.unstamped, state: failed).actionEnabled)
    }

    @Test func runningSpinsAndDisablesTheAction() {
        for build in [Self.release, Self.dev] {
            for step in [UpdateStep.preflight, .sync, .install] {
                let copy = UpdateCopy(build: build, state: .running(step))
                #expect(copy.spinning, "\(build.variant) \(step)")
                #expect(!copy.actionEnabled, "\(build.variant) \(step)")
                #expect(copy.statusToolTip == nil, "\(build.variant) \(step)")
            }
        }
        #expect(
            UpdateCopy(build: Self.release, state: .running(.install)).status
                == "Building and installing — this takes a few minutes…")
        #expect(
            UpdateCopy(build: Self.dev, state: .running(.install)).status
                == "Building the dev bundle — this takes a moment…")
        #expect(
            UpdateCopy(build: Self.release, state: .running(.sync)).status
                == "Fetching and fast-forwarding the checkout…")
    }

    @Test func upToDateOffersARebuildAnyway() {
        let copy = UpdateCopy(build: Self.release, state: .upToDate)
        #expect(copy.status == "Already at the commit this build came from.")
        #expect(copy.actionTitle == "Rebuild Anyway")
        #expect(copy.actionEnabled)
        #expect(!copy.spinning)
    }

    @Test func installedSpinsWhileRelaunching() {
        let release = UpdateCopy(build: Self.release, state: .installed)
        #expect(release.status == "Installed. Relaunching…")
        #expect(release.spinning)
        #expect(!release.actionEnabled)
        #expect(UpdateCopy(build: Self.dev, state: .installed).status == "Built. Relaunching…")
    }

    @Test func aFailureNamesTheStepAndCarriesTheFullMessage() {
        let state = UpdateState.failed(step: .install, message: "linker error")
        let release = UpdateCopy(build: Self.release, state: state)
        #expect(release.status == "Install failed: linker error")
        #expect(release.statusToolTip == "linker error")
        #expect(release.actionTitle == "Try Again")
        #expect(release.actionEnabled)
        #expect(!release.spinning)
        #expect(UpdateCopy(build: Self.dev, state: state).status == "Build failed: linker error")
        let fetch = UpdateCopy(build: Self.release, state: .failed(step: .sync, message: "x"))
        #expect(fetch.status == "Fetch failed: x")
        let preflight = UpdateCopy(
            build: Self.release, state: .failed(step: .preflight, message: "y"))
        #expect(preflight.status == "Preflight failed: y")
    }

    /// The tooltip belongs to the failure: every other state clears it.
    @Test func noStateButAFailureHasATooltip() {
        for state in [UpdateState.idle, .upToDate, .installed, .running(.preflight)] {
            #expect(UpdateCopy(build: Self.release, state: state).statusToolTip == nil, "\(state)")
        }
    }
}
