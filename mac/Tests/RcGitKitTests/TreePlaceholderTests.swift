import Testing

@testable import RcGitKit

/// What the tree says while it has no root, and that `attach` reports the empty case.
@MainActor
@Suite(.timeLimit(.minutes(1))) struct TreePlaceholderTests {
    @Test func noRepositoryAsksToOpenOne() {
        let head = Head(refName: "refs/heads/main", target: oid(0x11))
        #expect(TreePlaceholder.text(attached: false, head: nil) == TreePlaceholder.noRepository)
        #expect(TreePlaceholder.text(attached: false, head: head) == TreePlaceholder.noRepository)
    }

    @Test func unbornHeadHasNoCommits() {
        let unborn = Head(refName: "refs/heads/main", target: nil)
        #expect(TreePlaceholder.text(attached: true, head: unborn) == TreePlaceholder.noCommits)
    }

    @Test func otherwiseThereIsNothingToBrowse() {
        let born = Head(refName: nil, target: oid(0x11))
        #expect(TreePlaceholder.text(attached: true, head: born) == TreePlaceholder.nothingToBrowse)
        #expect(TreePlaceholder.text(attached: true, head: nil) == TreePlaceholder.nothingToBrowse)
    }

    @Test func attachingAnEmptyRepositoryReportsNoCommits() async {
        let repo = FakeRepository()
        repo.workDir = "/checkout"
        repo.headValue = Head(refName: "refs/heads/main", target: nil)
        let model = BrowserModel()
        #expect(model.treePlaceholder == TreePlaceholder.noRepository)
        var placeholders: [String] = []
        model.onRootChanged = { [unowned model] in
            #expect(model.rootTree == nil)
            placeholders.append(model.treePlaceholder)
        }
        await model.attach(repo)
        #expect(placeholders == [TreePlaceholder.noCommits])
    }

    @Test func attachWhoseRefsFailReportsNothingToBrowse() async {
        let repo = FakeRepository()
        repo.failNextRefs = true
        let model = BrowserModel()
        var placeholders: [String] = []
        model.onRootChanged = { [unowned model] in placeholders.append(model.treePlaceholder) }
        await model.attach(repo)
        #expect(placeholders == [TreePlaceholder.nothingToBrowse])
    }
}
