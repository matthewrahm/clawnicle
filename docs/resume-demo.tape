# VHS tape for docs/resume-demo.gif
#
# Generate with:  vhs docs/resume-demo.tape
# Requires: cargo build --release -p resume-demo -p clawnicle-cli

Output docs/resume-demo.gif

Set Shell      "zsh"
Set Theme      "TokyoNight"
Set FontSize   16
Set Width      1200
Set Height     720
Set TypingSpeed 35ms
Set Padding    24

# ---- silent setup --------------------------------------------------
Hide
Type "cd /tmp && rm -f resume-demo.clawnicle.db*"
Enter
Type "alias demo=$HOME/Documents/CODE/clawnicle/target/release/resume-demo"
Enter
Type "alias cl=$HOME/Documents/CODE/clawnicle/target/release/clawnicle"
Enter
Type "clear"
Enter
Show
Sleep 500ms

# ---- Act 1 — crash --------------------------------------------------
Type "# run 1 — step-b fails mid-workflow"
Enter
Sleep 900ms
Type "demo"
Enter
Sleep 3.2s

# ---- Act 2 — resume from cached step-a -----------------------------
Type "# heal and rerun — step-a is cached from the journal"
Enter
Sleep 900ms
Type "CLAWNICLE_HEAL=1 demo"
Enter
Sleep 3.2s

# ---- Act 3 — inspect the journal -----------------------------------
Type "# inspect the workflow — journal survived the crash"
Enter
Sleep 900ms
Type "cl --db resume-demo.clawnicle.db show demo-1"
Enter
Sleep 4.5s

# ---- silent teardown -----------------------------------------------
Hide
Type "rm -f resume-demo.clawnicle.db*"
Enter
