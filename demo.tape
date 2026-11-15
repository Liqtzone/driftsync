# VHS Terminal Demo Script (https://github.com/charmbracelet/vhs)
Output demo.gif

Set FontSize 16
Set Width 1100
Set Height 650
Set Theme "Catppuccin Mocha"

Type "# 1. Scaffold a multi-repo demo workspace (3 target services)"
Enter
Type "driftsync --root demo-workspace init --demo"
Enter
Sleep 2s

Type "# 2. Classify configuration drift across the fleet"
Enter
Type "driftsync --root demo-workspace status"
Enter
Sleep 2.5s

Type "# 3. Preview proposed 3-way structural merges (unified diff)"
Enter
Type "driftsync --root demo-workspace diff"
Enter
Sleep 3s

Type "# 4. Apply changes onto 'chore/config-sync' branch and commit per repo"
Enter
Type "driftsync --root demo-workspace apply --write --branch chore/config-sync"
Enter
Sleep 3s

Type "# 5. Verify with CI/CD audit check (--fail-on-drift)"
Enter
Type "driftsync --root demo-workspace check"
Enter
Sleep 2s
