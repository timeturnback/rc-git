import AppKit
import RcGitKit
import Testing

@testable import RcGitApp

/// The foreground color of every run in `text`, as (substring, color) pairs.
private func colorRuns(_ text: NSAttributedString) -> [(String, NSColor?)] {
    var runs: [(String, NSColor?)] = []
    text.enumerateAttribute(
        .foregroundColor, in: NSRange(location: 0, length: text.length)
    ) { value, range, _ in
        runs.append(((text.string as NSString).substring(with: range), value as? NSColor))
    }
    return runs
}

@Suite(.timeLimit(.minutes(1))) struct DiffRenderingTests {
    @Test func colorsEachLineByKindAcrossHunks() {
        let hunks = [
            DiffHunk(
                oldStart: 1, oldLines: 2, newStart: 1, newLines: 2,
                lines: [
                    DiffLine(kind: .context, text: "keep 🙂"),
                    DiffLine(kind: .removed, text: "old 🙂", eol: .crlf),
                    DiffLine(kind: .added, text: "new"),
                ]),
            DiffHunk(
                oldStart: 9, oldLines: 1, newStart: 9, newLines: 1,
                lines: [DiffLine(kind: .added, text: "tail", eol: .none)]),
        ]
        let crlf = LineEnd.crlf.marker ?? ""
        let noEOL = LineEnd.none.marker ?? ""

        let text = DiffRendering.attributed(hunks: hunks).text

        #expect(
            text.string
                == "@@ -1,2 +1,2 @@\n keep 🙂\n-old 🙂\(crlf)\n+new\n\n"
                + "@@ -9,1 +9,1 @@\n+tail\(noEOL)\n"
        )
        let runs = colorRuns(text)
        #expect(
            runs.map(\.0) == [
                "@@ -1,2 +1,2 @@", "\n keep 🙂\n", "-old 🙂", crlf, "\n", "+new", "\n\n",
                "@@ -9,1 +9,1 @@", "\n", "+tail", noEOL, "\n",
            ])
        #expect(
            runs.map(\.1) == [
                .tertiaryLabelColor, .labelColor, .systemRed, .tertiaryLabelColor, .labelColor,
                .systemGreen, .labelColor, .tertiaryLabelColor, .labelColor, .systemGreen,
                .tertiaryLabelColor, .labelColor,
            ])
    }

    @Test func noHunksBuildsAnEmptyString() {
        #expect(DiffRendering.attributed(hunks: []).text.length == 0)
    }
}
