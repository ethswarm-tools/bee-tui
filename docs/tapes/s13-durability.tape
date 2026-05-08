# S13 Durability Watchlist demo — :durability-check <ref> walks
# the chunk graph rooted at <ref> and records the result on S13.
#
# Render: vhs s13-durability.tape
# Output: s13-durability.gif
#
# NOTE: substitute a reference that exists on your test node.
# A 404 still produces a clear S13 row (the count buckets show
# the failure mode) but a resolvable manifest gives the more
# satisfying "X chunks total · 0 lost · BMT" green line.

Output docs/tapes/s13-durability.gif

Set FontSize 14
Set Width 1200
Set Height 720
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Type "bee-tui --bee-bin ../bee/dist/bee --bee-config ../testnet.yaml"
Enter
Sleep 6s

# Jump to S13 first so viewers see the empty state.
Type ":watchlist"
Enter
Sleep 2s

# Run a durability check. The verb walks the chunk graph in the
# background; the result lands as a new S13 row when complete.
Type ":durability-check f8aa0f76e9f37ebf76b8e33f69ce0fa3a04a1d3e1aa1a0ce6c2eaff29e9e3e4d"
Sleep 1s
Enter
Sleep 8s

# Linger so the row is readable. The cursored row's full ref
# appears on the `selected:` detail line — operators can
# click-drag to copy.
Sleep 2s

# Run a second check on a different ref so the screen shows the
# rolling history, not just one row.
Type ":durability-check 0011223344556677889900112233445566778899001122334455667788990011"
Sleep 1s
Enter
Sleep 8s

Sleep 2s

Type "q"
Sleep 1s
