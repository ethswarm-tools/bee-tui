# S12 Manifest browser demo — :manifest <ref> opens the tree
# rooted at a Mantaray reference; ↵ expands a fork.
#
# Render: vhs s12-manifest.tape
# Output: s12-manifest.gif
#
# NOTE: replace the placeholder reference below with a Mantaray
# manifest that exists on the Bee node you're rendering against.
# The visual works even if the ref 404s — the cockpit shows
# `error: not a manifest` or `error: chunk not found` clearly —
# but a resolvable ref produces the more interesting tree-walk
# rendering.

Output s12-manifest.gif

Set FontSize 14
Set Width 1200
Set Height 720
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Type "bee-tui"
Enter
Sleep 2s

# Open the command bar and load a manifest. The screen jumps to
# S12 immediately; the root chunk fetch runs async.
Type ":manifest f8aa0f76e9f37ebf76b8e33f69ce0fa3a04a1d3e1aa1a0ce6c2eaff29e9e3e4d"
Sleep 1s
Enter
Sleep 4s

# Move the cursor down a couple of rows.
Down
Sleep 500ms
Down
Sleep 500ms

# Toggle expand on the cursored fork — children either appear
# (if already loaded) or kick off an async chunk fetch.
Enter
Sleep 3s

# Walk a child fork.
Down
Sleep 500ms
Down
Sleep 500ms
Enter
Sleep 3s

# Pop the help overlay so viewers see the S12-specific keys.
Type "?"
Sleep 3s
Escape

Type "q"
Sleep 1s
