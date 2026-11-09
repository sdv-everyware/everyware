#!/bin/bash
# Installs the hooks in every repository of the project that has them (scripts/git-hooks/): no
# commit on main and no move of main except by feature.sh land, the project's author, nothing
# private in a change, no AI attribution in a message; ui-lib also runs its scripts/check.sh.
#
# What is installed is main's version of the hooks, copied into the repository's .git/hooks: a
# hook is in force once it landed, the same on every branch, whatever a branch does to the files.
# Only a repository whose main has no hooks yet (a new one) gets them from the working tree. It
# also notes where main is, so that feature.sh status can say when main was moved another way.
# Once per checkout; feature.sh land runs it again, feature.sh status says when a copy is old:
#
#   vendor/sdv-everyware/everyware/scripts/setup-agents.sh
set -euo pipefail
# (EVERYWARE_TOP: another tree, for the test)
TOP=${EVERYWARE_TOP:-$(cd "$(dirname "$0")/../../../.." && pwd)}
# The person's private words (a user id, a work address) that pre-commit refuses: a file outside
# git, written by hand once. Here only the empty file with its comment, so the hook finds it.
words=${EVERYWARE_PRIVATE_WORDS:-$HOME/.config/everyware/private-words}
if [ ! -f "$words" ]; then
    mkdir -p "$(dirname "$words")"
    printf '# One extended regex per line: what must never be in a commit of SDV EveryWare (a user\n# id, a work e-mail address). The pre-commit hook of every repository reads this file.\n' > "$words"
    chmod 600 "$words"
    echo "private words     $words written empty: add your own (one regex per line)"
fi
for repo in "$TOP"/vendor/sdv-everyware/everyware "$TOP"/vendor/sdv-everyware/ui-lib "$TOP"/vendor/sdv-everyware/third_party/rust \
            "$TOP"/vendor/sdv-everyware/dnsproxyd "$TOP"/packages/services/display_safety/utils/*-app; do
    [ -d "$repo/.git" ] || continue
    name=${repo#"$TOP"/}
    gitdir=$(git -C "$repo" rev-parse --absolute-git-dir); hooks=$gitdir/hooks
    if git -C "$repo" cat-file -e main:scripts/git-hooks/pre-commit 2>/dev/null; then
        from="main"; list=$(git -C "$repo" ls-tree --name-only main scripts/git-hooks/ | sed 's|.*/||')
    elif [ -f "$repo/scripts/git-hooks/pre-commit" ]; then
        from="the working tree (main has none yet)"; list=$(ls "$repo/scripts/git-hooks")
    else
        echo "no hooks          $name   (neither main nor this branch has scripts/git-hooks/pre-commit)"; continue
    fi
    mkdir -p "$hooks"
    # a hook this script installed before and that is gone now goes
    if [ -f "$hooks/.everyware" ]; then while read -r old; do echo "$list" | grep -qx "$old" || rm -f "$hooks/$old"; done < "$hooks/.everyware"; fi
    for h in $list; do
        if [ "$from" = main ]; then git -C "$repo" show "main:scripts/git-hooks/$h" > "$hooks/$h"; else cp "$repo/scripts/git-hooks/$h" "$hooks/$h"; fi
        chmod +x "$hooks/$h"
    done
    echo "$list" > "$hooks/.everyware"
    git -C "$repo" config --unset core.hooksPath 2>/dev/null || true
    # (only a commit goes into the note: a repository without main gets it when main lands)
    if [ ! -f "$gitdir/everyware-main" ] && at=$(git -C "$repo" rev-parse -q --verify main 2>/dev/null); then echo "$at" > "$gitdir/everyware-main"; fi
    echo "hooks installed   $name   from $from"
done
