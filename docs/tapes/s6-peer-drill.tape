# S6 peer drill demo — Enter on a peer → balance / cheques /
# settlement / ping fan-out.
#
# Render: vhs s6-peer-drill.tape
# Output: s6-peer-drill.gif

Output s6-peer-drill.gif

Set FontSize 14
Set Width 1200
Set Height 720
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Type "bee-tui"
Enter
Sleep 2s

Type ":peers"
Enter
Sleep 2s

# Move down a few rows to find a peer with cheques exchanged
Down
Sleep 300ms
Down
Sleep 300ms
Down
Sleep 300ms

# Drill — fan-out lands within ~1s
Enter
Sleep 5s

Escape
Sleep 1s
Type "q"
