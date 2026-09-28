---
title: "Define a shifted-punctuation key equivalent as the unshifted key plus .shift, and test it with a CGEvent"
date: 2026-09-28
category: best-practices
module: RcGitApp
problem_type: best_practice
component: frontend
severity: low
applies_when:
  - "Adding an NSMenuItem key equivalent on a shifted punctuation key, such as Finder's ⇧⌘. for Show Hidden Files"
  - Checking that a key equivalent fires without launching the app
  - A synthetic key event and the menu disagree about whether a shortcut matches
  - Choosing a shortcut whose base key another item already uses
symptoms:
  - "charactersIgnoringModifiers keeps Shift's effect, so ⇧⌘. reports a greater-than sign and an item defined as period plus .shift looks unmatchable"
  - A hand-built NSEvent.keyEvent matched only the item that fit the characters typed into it
root_cause: wrong_api
related_components: [RcGitKit]
tags:
  [
    appkit,
    nsmenu,
    key-equivalent,
    keyboard-shortcut,
    nsevent,
    cgevent,
    headless-probe,
  ]
---

# Define a shifted-punctuation key equivalent as the unshifted key plus .shift, and test it with a CGEvent

## Context

View ▸ Show Hidden Files needed Finder's shortcut, ⇧⌘. (Command-Shift-period). The question
was how to write it as an `NSMenuItem` key equivalent. `NSEvent.charactersIgnoringModifiers`
keeps the effect of Shift, so on a US layout the keystroke reports `>`. That raised the worry
that an item defined as `"."` with `.shift` in its mask would never match, and that it had to
be `">"` with `.command` instead.

The first probe seemed to confirm the worry, and it was wrong. It built the event by hand:
`NSEvent.keyEvent(with: .keyDown, ..., modifierFlags: [.command, .shift], characters: ">",
charactersIgnoringModifiers: ">", keyCode: 47)` sent to `NSMenu.performKeyEquivalent(with:)`
fired the `">"` + `.command` item and missed the `"."` + `[.command, .shift]` item. A second
hand-built event with `"."` in both character fields fired the `"."` + shift item instead.
`NSEvent.keyEvent(with:...)` takes the characters as arguments and does not derive them from
the key code and the keyboard layout, so each probe only restated the characters its author
guessed.

The repo forbids launching the app to verify a change, so the question had to be answered
headless.

## Guidance

- **Define shifted punctuation as the unshifted key plus `.shift` in the mask.** `"."` with
  `[.command, .shift]` catches ⇧⌘., the keystroke Finder uses for the same toggle, and the menu
  shows it as ⇧⌘.. Letters work either way, uppercase alone or lowercase plus `.shift`.
  Punctuation has no uppercase form, so the unshifted key plus `.shift` is its only form that
  shows the shortcut as the user types it.
- **Test key-equivalent matching with a layout-derived event, never a hand-built one.** Build
  the event from `CGEvent(keyboardEventSource:virtualKey:keyDown:)` and wrap it with
  `NSEvent(cgEvent:)`. The current keyboard layout then fills in `characters` and
  `charactersIgnoringModifiers` the way a real keystroke does, and the event carries what else
  AppKit matches on.
- **Answer it headless.** A script compiled with `xcrun swiftc` that touches
  `NSApplication.shared`, builds an `NSMenu` of items with a `target` and `action`, and calls
  `performKeyEquivalent(with:)` runs in seconds. Put the candidate definitions in one menu and
  also one at a time, and test the neighbouring shortcut to rule out a collision.

## Why This Matters

A hand-built event makes a wrong definition look correct and a correct one look broken. The
first probe here pointed at rewriting the committed `"."` + shift item as `">"` + `.command`,
which would have shown ⌘> in the menu instead of ⇧⌘., the keystroke Finder uses.

The layout-derived event answers what the machine actually does. Re-run on 2026-09-28
(macOS 26.6.2, Swift 6.4, ABC layout, which matches US for this key), a real ⇧⌘. (key code 47
with command and shift flags)
reported `characters` `"."` and `charactersIgnoringModifiers` `">"`, and:

| menu contents | real ⇧⌘. fires |
|---|---|
| `"."` + `[.command, .shift]`, then `">"` + `.command` | the `"."` + shift item |
| `">"` + `.command`, then `"."` + `[.command, .shift]` | the `"."` + shift item (order does not matter) |
| only `"."` + `[.command, .shift]` | that item |
| only `">"` + `.command` | that item |
| only `"."` + `.command` | nothing |

A real ⌘. (command flag only) fires `"."` + `.command` and does not fire `"."` +
`[.command, .shift]`. So both definitions catch ⇧⌘., AppKit prefers the unshifted-plus-shift
item when both exist, and the shortcut does not collide with ⌘. on the same key.
`performKeyEquivalent(with:)` matches on more than the two character fields. A hand-built
event that copies the real event's `characters` (`"."`), `charactersIgnoringModifiers` (`">"`),
key code, and flags still misses the `"."` + shift item and fires `">"` + `.command`. So a
hand-built event cannot stand in for a real keystroke even when you copy its fields.

Key code 47 is the period key's position on an ANSI keyboard. On another layout the same key
code produces other characters, and the CGEvent probe reports them, which is the point: it
answers for the layout the machine has.

## When to Apply

- Adding or changing a menu shortcut on a shifted punctuation key (`.`, `,`, `/`, `[`, `]`, `;`,
  `'`, `=`, `-`, `` ` ``).
- Adding a shortcut whose base key another menu item already uses with different modifiers.
- Any time a synthetic key event is the evidence for how a shortcut behaves.

## Examples

In the tree, Show Hidden Files is defined as `"."` with `[.command, .shift]`
(`mac/Sources/RcGitApp/MainMenu.swift:115-119`). The `item(_:_:_:_:)` helper passes the string
to `NSMenuItem(title:action:keyEquivalent:)` and sets `keyEquivalentModifierMask`, defaulting
to `.command` (`MainMenu.swift:29-36`). Cancel Sync owns ⌘. on the same key with `[.command]`
(`MainMenu.swift:170-173`). The Window menu already uses the same punctuation pattern: Show
Previous Tab and Show Next Tab are `"["` and `"]"` with `[.command, .shift]`
(`MainMenu.swift:182-189`). Letters in the file use both forms: Find Previous is uppercase
`"G"` (`MainMenu.swift:96`), and Go to File is lowercase `"o"` with `[.command, .shift]`
(`MainMenu.swift:76-79`). The action, `AppDelegate.toggleHiddenFiles(_:)`, flips the
`HiddenFiles.preferenceKey` default and pushes it to every window controller
(`mac/Sources/RcGitApp/AppDelegate.swift:82-88`); `validateMenuItem(_:)` sets the checkmark
from the same default (`AppDelegate.swift:90-94`).

Before — a hand-built event that only echoes the author's guess:

```swift
let guessed = NSEvent.keyEvent(
    with: .keyDown, location: .zero, modifierFlags: [.command, .shift], timestamp: 0,
    windowNumber: 0, context: nil, characters: ">", charactersIgnoringModifiers: ">",
    isARepeat: false, keyCode: 47)!
```

After — the probe that answers the question (write it to a scratch directory, then
`xcrun swiftc probe.swift -o probe && ./probe`):

```swift
import AppKit

final class Sink: NSObject {
    @objc func fire(_ sender: Any?) { print("fired:", (sender as? NSMenuItem)?.title ?? "?") }
}

_ = NSApplication.shared
let sink = Sink()
let menu = NSMenu()
for (title, key, mask) in [
    ("shift-dot", ".", NSEvent.ModifierFlags([.command, .shift])), ("greater", ">", .command),
] {
    let item = NSMenuItem(title: title, action: #selector(Sink.fire(_:)), keyEquivalent: key)
    item.keyEquivalentModifierMask = mask
    item.target = sink
    menu.addItem(item)
}

// 47 = period key (ANSI). The layout supplies the characters; do not hand-build them.
let cg = CGEvent(keyboardEventSource: nil, virtualKey: 47, keyDown: true)!
cg.flags = [.maskCommand, .maskShift]
let event = NSEvent(cgEvent: cg)!
print("chars:", event.characters ?? "nil", "ignoring:", event.charactersIgnoringModifiers ?? "nil")
print("matched:", menu.performKeyEquivalent(with: event))
```

Output on 2026-09-28: `chars: . ignoring: >`, `fired: shift-dot`, `matched: true`.

## Related

- `docs/solutions/ui-bugs/nstextblock-without-content-width-collapses.md` — the same approach of
  measuring an AppKit behavior headless instead of launching the app.
- `docs/solutions/ui-bugs/nsstackview-hidden-at-attach-never-gets-width-alignment.md` — a
  standalone AppKit probe that reproduced a layout bug without launching the app.
- `docs/solutions/integration-issues/foundation-markdown-parser-cannot-back-a-preview.md` —
  probe a platform API's real behavior before trusting it.
- `docs/solutions/runtime-errors/sync-hangs-with-no-cancel-and-no-idle-timeout.md` — where
  Cancel Sync, the ⌘. neighbour of this shortcut, comes from.
