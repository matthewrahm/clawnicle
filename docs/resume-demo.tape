# VHS tape for docs/resume-demo.gif
#
# Generate with:  vhs docs/resume-demo.tape
# Requires:       cargo build --release -p resume-demo

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
Type "cd /tmp && rm -f research.clawnicle.db*"
Enter
Type "alias research=$HOME/Documents/CODE/clawnicle/target/release/resume-demo"
Enter
Type "clear"
Enter
Show
Sleep 500ms

# ---- Act 1 — crash --------------------------------------------------
Type "# run the agent — step 3 crashes"
Enter
Sleep 900ms
Type "research"
Enter
Sleep 4.5s

# ---- Act 2 — resume -----------------------------------------------
Type "# rerun — finished steps are skipped, the rest picks up from where we left off"
Enter
Sleep 900ms
Type "CLAWNICLE_HEAL=1 research"
Enter
Sleep 5s

# ---- silent teardown -----------------------------------------------
Hide
Type "rm -f research.clawnicle.db*"
Enter
