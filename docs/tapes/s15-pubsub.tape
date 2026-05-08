# S15 Pubsub watch demo — :pubsub-pss <topic> opens a WebSocket
# subscription and surfaces every received frame on the merged
# timeline. :pubsub-stop cancels.
#
# Render: vhs s15-pubsub.tape
# Output: s15-pubsub.gif
#
# NOTE: the timeline only fills in if a publisher hits
# /pss/send/<topic>/<target> while the subscription is open.
# This recording shows the empty state by default — operators
# running a publisher in another shell will see live frames
# scroll in.

Output s15-pubsub.gif

Set FontSize 14
Set Width 1200
Set Height 720
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Type "bee-tui"
Enter
Sleep 2s

# Open a PSS subscription on a string topic. The cockpit
# keccak-hashes the string into the 32-byte topic; the verb
# jumps to S15 immediately.
Type ":pubsub-pss demo-channel"
Sleep 1s
Enter
Sleep 4s

# Open a second subscription so the header shows
# "2 active subs" — demonstrates the merged-timeline model.
Type ":pubsub-pss heartbeat"
Sleep 1s
Enter
Sleep 4s

# Show the filter feature. With no live messages, the filter
# header chip becomes the visual story.
Type ":pubsub-filter heart"
Sleep 1s
Enter
Sleep 3s

Type ":pubsub-filter-clear"
Sleep 1s
Enter
Sleep 2s

# Stop everything. Header drops back to "0 active subs".
Type ":pubsub-stop"
Sleep 1s
Enter
Sleep 3s

Type "q"
Sleep 1s
