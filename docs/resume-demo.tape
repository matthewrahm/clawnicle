# VHS tape for docs/resume-demo.gif
#
# Generate with:  vhs docs/resume-demo.tape
# Requires: cargo build --release -p resume-demo -p clawnicle-cli

Output docs/resume-demo.gif

Set Shell   "zsh"
Set FontSize 14
Set Width   1100
Set Height   720
Set Theme   "Dracula"
Set TypingSpeed 40ms
Set Padding 20

# ---- silent setup --------------------------------------------------
Hide
Type "cd /tmp"                                               Enter
Type "rm -f resume-demo.clawnicle.db*"                       Enter
Type "alias demo=$HOME/Documents/CODE/clawnicle/target/release/resume-demo"  Enter
Type "alias cl=$HOME/Documents/CODE/clawnicle/target/release/clawnicle"      Enter
Type "clear"                                                 Enter
Show

# ---- Act 1 — crash --------------------------------------------------
Type "# step-b fails on first run — workflow crashes"
Enter
Sleep 800ms
Type "demo"
Enter
Sleep 3s

# ---- Act 2 — resume from last successful step ----------------------
Type "# heal and rerun — step-a is cached, step-b retries, step-c runs"
Enter
Sleep 800ms
Type "CLAWNICLE_HEAL=1 demo"
Enter
Sleep 3s

# ---- Act 3 — inspect the journal -----------------------------------
Type "# inspect the journal — step-b appears twice (failed, then retried)"
Enter
Sleep 800ms
Type "cl --db resume-demo.clawnicle.db events demo-1"
Enter
Sleep 5s

# ---- silent teardown ------------------------------------------------
Hide
Type "rm -f resume-demo.clawnicle.db*"
Enter
