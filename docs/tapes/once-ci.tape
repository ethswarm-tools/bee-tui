# --once CI mode demo — single-shot verbs without launching the
# full TUI. One human line on stdout (or JSON with --json), exit
# code 0 / 1 / 2 for ok / unhealthy / usage-error.
#
# Render: vhs once-ci.tape
# Output: once-ci.gif
#
# Showcases:
#   * pure-local verbs that need no Bee node (depth-table,
#     pss-target)
#   * Bee-API verbs that gate on node state (readiness)
#   * the --json toggle for CI parsing
#   * the exit-code surface

Output docs/tapes/once-ci.gif

Set FontSize 14
Set Width 1200
Set Height 480
Set Theme "Catppuccin Mocha"
Set TypingSpeed 50ms

# Pure-local: depth → capacity table. Always 0 exit, no Bee.
Type "bee-tui --once depth-table"
Enter
Sleep 4s

# Pure-local: extract the 4-hex target prefix Bee accepts on
# /pss/send from a full overlay.
Type "bee-tui --once pss-target 0x1234567890abcdef1234567890abcdef12345678"
Enter
Sleep 3s

# Bee-API: readiness gate. Exit 0 on healthy, 1 on not-ready.
Type "bee-tui --once readiness"
Enter
Sleep 4s

# Same thing as JSON for CI parsing. The shape
# {verb, status, message, data} is the v1.3.0 stable contract.
Type "bee-tui --once readiness --json"
Enter
Sleep 4s

# Exit code is part of the contract too — a CI gate can simply
# trust the process exit status without parsing stdout.
Type "echo exit code: $?"
Enter
Sleep 3s

Type "clear"
Enter
Sleep 500ms
