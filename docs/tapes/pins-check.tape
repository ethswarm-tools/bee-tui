# :pins-check demo — invoke from the command bar, see the file
# path appear in the status line, switch to a tail in another pane.
#
# Render: vhs pins-check.tape
# Output: pins-check.gif

Output pins-check.gif

Set FontSize 14
Set Width 1200
Set Height 720
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Type "bee-tui"
Enter
Sleep 2s

# Open the command bar and type the command
Type ":pins-check"
Sleep 1s
Enter

# Status line shows the destination file path; linger to read
Sleep 5s

Type "q"
