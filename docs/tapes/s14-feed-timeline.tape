# S14 Feed Timeline demo — :feed-timeline <owner> <topic> [N]
# walks a feed's history (newest first), one row per update.
#
# Render: vhs s14-feed-timeline.tape
# Output: s14-feed-timeline.gif
#
# NOTE: substitute a real (owner, topic) pair from your test
# node. Without one, the timeline shows "no updates found" — a
# valid render but less interesting than the populated case.
# The topic accepts either 64-hex or any plain string (which is
# keccak256-hashed at runtime via Topic::from_string).

Output s14-feed-timeline.gif

Set FontSize 14
Set Width 1200
Set Height 720
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Type "bee-tui"
Enter
Sleep 2s

# Walk the timeline of an example feed. The verb jumps to S14
# on dispatch; rows fill in as the bounded-parallel SOC fetches
# complete (8 in flight at once, default 50 entries).
Type ":feed-timeline 0x1234567890abcdef1234567890abcdef12345678 my-feed"
Sleep 1s
Enter
Sleep 6s

# Scroll the cursor down. Each row's `index`, `payload bytes`
# and `timestamp` appear on the `selected:` detail line.
Down
Sleep 500ms
Down
Sleep 500ms
Down
Sleep 500ms
PageDown
Sleep 1s
PageDown
Sleep 1s

# Linger on the populated table.
Sleep 3s

Type "q"
Sleep 1s
