# Cold-start demo — first 60 seconds of a fresh launch.
#
# Render: vhs cold-start.tape
# Output: cold-start.gif
#
# Operator tour: launch, watch the syncing warn, tab through the
# fourteen screens, observe the gates settle as Bee finishes warmup.

Output docs/tapes/cold-start.gif

Set FontSize 14
Set Width 1200
Set Height 720
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Type "bee-tui --bee-bin ../bee/dist/bee --bee-config ../testnet.yaml"
Enter
Sleep 8s

# Cycle through all fourteen screens. Tab moves to the next; we
# press it 13 times to land on the last one (Pubsub).
#
# Order: Health → Stamps → Swap → Lottery → Peers → Network →
#        Warmup → API → Tags → Pins → Manifest → Watchlist →
#        FeedTimeline → Pubsub
Tab
Sleep 1500ms
Tab
Sleep 1500ms
Tab
Sleep 1500ms
Tab
Sleep 1500ms
Tab
Sleep 1500ms
Tab
Sleep 1500ms
Tab
Sleep 1500ms
Tab
Sleep 1500ms
Tab
Sleep 1500ms
Tab
Sleep 1500ms
Tab
Sleep 1500ms
Tab
Sleep 1500ms
Tab
Sleep 2s

# Pop the help overlay to advertise the keymap.
Type "?"
Sleep 4s
Escape

# Quit
Type "q"
Sleep 1s
