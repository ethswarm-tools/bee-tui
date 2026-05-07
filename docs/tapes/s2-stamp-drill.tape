# S2 stamp drill demo — Enter on a batch row → bucket histogram.
#
# Render: vhs s2-stamp-drill.tape
# Output: s2-stamp-drill.gif

Output s2-stamp-drill.gif

Set FontSize 14
Set Width 1200
Set Height 720
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Type "bee-tui"
Enter
Sleep 2s

# Jump to S2
Type ":stamps"
Enter
Sleep 2s

# Move the selection cursor (the `▶` in front of a row)
Down
Sleep 500ms
Down
Sleep 500ms

# Drill
Enter
Sleep 4s

# Linger on the histogram
Sleep 3s

# Close drill, quit
Escape
Sleep 1s
Type "q"
