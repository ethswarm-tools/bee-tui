# Cold-start demo — first 30 seconds of a fresh launch.
#
# Render: vhs cold-start.tape
# Output: cold-start.gif
#
# Operator tour: launch, watch the syncing warn, tab through the
# nine screens, observe the gates settle as Bee finishes warmup.

Output cold-start.gif

Set FontSize 14
Set Width 1200
Set Height 720
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Type "bee-tui"
Enter
Sleep 3s

# Cycle through the screens quickly
Tab
Sleep 2s
Tab
Sleep 2s
Tab
Sleep 2s
Tab
Sleep 2s
Tab
Sleep 2s
Tab
Sleep 2s
Tab
Sleep 2s
Tab
Sleep 2s

# Pop the help overlay
Type "?"
Sleep 4s
Escape

# Quit
Type "q"
Sleep 1s
