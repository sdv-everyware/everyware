#!/bin/bash
# A change lands in three steps: its own branch, a review by another agent, the user's word.
# This script does the mechanical part in the repositories of the project (everyware, ui-lib,
# third_party/rust, dnsproxyd, the apps); AGENTS.md of everyware, "How a change lands", has the
# rules. A repository is named by its folder: everyware, ui-lib, rust, dnsproxyd, <app>-app.
#
#   feature.sh start <name> <repo>...    # branch feature/<name> off main in these repositories;
#                                        # again later for one more. Refuses a repository that
#                                        # has another feature open (one at a time per repository)
#                                        # or whose guard of main is not whole
#   feature.sh status                    # per repository: every feature branch with its commits and its
#                                        # review, and what is wrong with the guard of main, if anything
#   feature.sh diff <name>               # what the branch changes against main, all repositories
#   feature.sh reviewed <name> ok|blocked   # the reviewer's verdict on the branch as it is now
#   feature.sh accept-main <repo>...     # main was moved another way and the user decided that it is
#                                        # right as it is now: take it as the place land left it
#   feature.sh land <name> "<summary>"   # merge into main (a merge commit), delete the branch:
#                                        # only after the user said so. Checks every repository
#                                        # first (a review "ok" of the branch's last commit on
#                                        # the main it was reviewed against, a clean merge) and
#                                        # merges none if one fails
#
# scripts/test-feature.sh runs all of this in a throwaway tree.
set -euo pipefail
# (EVERYWARE_TOP: another tree, for the test)
TOP=${EVERYWARE_TOP:-$(cd "$(dirname "$0")/../../../.." && pwd)}
die() { echo "feature.sh: $*" >&2; exit 1; }
repos() {
    local r
    for r in "$TOP"/vendor/sdv-everyware/everyware "$TOP"/vendor/sdv-everyware/ui-lib "$TOP"/vendor/sdv-everyware/third_party/rust \
             "$TOP"/vendor/sdv-everyware/dnsproxyd "$TOP"/packages/services/display_safety/utils/*-app; do
        [ -d "$r/.git" ] && echo "$r"
    done
    return 0
}
short() { echo "${1#"$TOP"/}"; }
resolve() {   # a repository by its folder name
    local r
    for r in $(repos); do [ "$(basename "$r")" = "$1" ] && { echo "$r"; return; }; done
    die "no repository '$1' (one of: $(for r in $(repos); do basename "$r"; done | tr '\n' ' '))"
}
has() { git -C "$1" show-ref --verify --quiet "refs/heads/$2"; }
current() { git -C "$1" symbolic-ref --short -q HEAD || true; }
dirty() { [ -n "$(git -C "$1" status --porcelain)" ]; }
# (the prefix, not a glob: a glob's * stops at a slash and would miss feature/a/b)
features() { git -C "$1" for-each-ref --format='%(refname:short)' refs/heads/feature/; }
# the verdict on a branch: "<the branch's commit> <main's commit then> <ok|blocked>"
record() { echo "$1/.git/everyware-review/$(echo "$2" | sed 's|/|%|g')"; }
# every repository that has the branch, or an error: a mistyped name must not look like success
with_branch() {
    local r found=0
    for r in $(repos); do has "$r" "$1" && { echo "$r"; found=1; }; done
    [ $found = 1 ] || die "no repository has the branch $1 (feature.sh status)"
}
review_of() {   # the state of a branch's review, in words
    local r=$1 b=$2 f sha msha verdict
    f=$(record "$r" "$b")
    [ -f "$f" ] || { echo none; return; }
    read -r sha msha verdict < "$f"
    if [ "$sha" != "$(git -C "$r" rev-parse "$b")" ]; then echo "$verdict, of an older commit"
    elif [ "$msha" != "$(git -C "$r" rev-parse main)" ]; then echo "$verdict, against an older main"
    else echo "$verdict"; fi
}

# what is wrong with the guard of main in a repository, in words (nothing when all is well): the
# installed hooks are main's version (the working tree's while main has none), no core.hooksPath
# takes git elsewhere, and main is where land or the installation left it (a rename onto main or
# a delete and create is not something a hook can refuse)
guard() {
    local r=$1 gitdir hooks h out= was
    gitdir=$(git -C "$r" rev-parse --absolute-git-dir); hooks=$gitdir/hooks
    if git -C "$r" cat-file -e main:scripts/git-hooks/pre-commit 2>/dev/null; then
        for h in $(git -C "$r" ls-tree --name-only main scripts/git-hooks/ | sed 's|.*/||'); do
            git -C "$r" show "main:scripts/git-hooks/$h" | cmp -s - "$hooks/$h" || { out="$out, HOOKS ARE NOT MAIN'S (scripts/setup-agents.sh)"; break; }
        done
    elif [ -d "$r/scripts/git-hooks" ]; then
        for h in "$r"/scripts/git-hooks/*; do cmp -s "$h" "$hooks/$(basename "$h")" || { out="$out, HOOKS NOT INSTALLED (scripts/setup-agents.sh)"; break; }; done
    fi
    [ -z "$(git -C "$r" config core.hooksPath || true)" ] || out="$out, core.hooksPath IS SET: the installed hooks do not run (unset it; setup-agents.sh unsets the repository's own, not a global one)"
    if [ -f "$gitdir/everyware-main" ]; then
        was=$(cat "$gitdir/everyware-main")
        [ "$was" = "$(git -C "$r" rev-parse main)" ] || out="$out, MAIN WAS MOVED WITHOUT land (it was at ${was:0:7})"
    fi
    echo "$out"
}

# nothing starts and nothing lands in a repository whose guard is not whole
whole() {
    local g; g=$(guard "$1")
    [ -z "$g" ] || die "$(short "$1"): the guard of main is not whole${g}. Hooks: scripts/setup-agents.sh. A moved main: look at what it holds now (git log), tell the user, and after the user's decision feature.sh accept-main $(basename "$1")"
}

cmd=${1:-}; shift || true
case "$cmd" in
start)
    name=${1:-}; [ -n "$name" ] && shift || die "usage: feature.sh start <name> <repo>..."
    [ $# -gt 0 ] || die "usage: feature.sh start <name> <repo>...   (the repositories the change touches)"
    b=feature/$name; list=()
    for n in "$@"; do   # every check first: all of them start or none
        r=$(resolve "$n"); list+=("$r")
        has "$r" main || die "$(short "$r") has no branch main: the process needs one (git branch -m main)"
        whole "$r"
        cur=$(current "$r")
        [ "$cur" = main ] || [ "$cur" = "$b" ] || die "$(short "$r") is on '${cur:-a detached HEAD}': another change is open there. One feature at a time per repository: finish that one first (a feature/* branch lands with feature.sh land), or ask the session that works on it"
        for other in $(features "$r"); do
            [ "$other" = "$b" ] || die "$(short "$r") has the open feature $other: land or delete it first (one feature at a time per repository)"
        done
        dirty "$r" && die "$(short "$r") has uncommitted changes: commit or drop them first"
    done
    for r in "${list[@]}"; do
        if has "$r" "$b"; then git -C "$r" switch -q "$b"; else git -C "$r" switch -q -c "$b" main; fi
        echo "on $b   $(short "$r")"
    done ;;
status)
    for r in $(repos); do
        cur=$(current "$r")
        has "$r" main || { echo "$(short "$r")   (no branch main)"; continue; }
        extra=; dirty "$r" && extra=", $(git -C "$r" status --porcelain | wc -l) uncommitted"
        echo "$(short "$r")   on ${cur:-a detached HEAD}$extra$(guard "$r")"
        for f in $(features "$r"); do
            mark=' '; [ "$f" = "$cur" ] && mark='*'
            echo "  $mark $f: $(git -C "$r" rev-list --count "main..$f") commits, review: $(review_of "$r" "$f")"
            # what the branch is about: its commits, the oldest first
            git -C "$r" log --reverse --date=format:'%d.%m %H:%M' --format='      %h  %ad  %s' "main..$f"
        done
    done ;;
accept-main)
    [ $# -gt 0 ] || die "usage: feature.sh accept-main <repo>...   (after the user decided that main as it is now is right)"
    for n in "$@"; do
        r=$(resolve "$n")
        git -C "$r" rev-parse main > "$(git -C "$r" rev-parse --absolute-git-dir)/everyware-main"
        echo "main is taken as it is   $(short "$r") at $(git -C "$r" rev-parse --short main)"
    done ;;
diff)
    name=${1:-}; [ -n "$name" ] || die "usage: feature.sh diff <name>"
    b=feature/$name
    # binaries and what a generator writes are named, not printed
    skip=(':!*.dcf' ':!*.fig' ':!docs/site' ':!docs/ai' ':!docs/llms*.txt' ':!docs/assets')
    only=('*.dcf' '*.fig' 'docs/site' 'docs/ai' 'docs/llms*.txt' 'docs/assets')
    list=$(with_branch "$b")
    for r in $list; do
        echo "===================== $(short "$r")   main..$b"
        git -C "$r" log --format='  %h %s' "main..$b"
        git -C "$r" diff --stat=200 "main...$b"
        left=$(git -C "$r" diff --name-status "main...$b" -- "${only[@]}")
        [ -z "$left" ] || { echo "changed and not printed below (a binary or generated; look at it yourself):"; echo "$left" | sed 's/^/  /'; }
        git -C "$r" diff "main...$b" -- . "${skip[@]}"
    done ;;
reviewed)
    name=${1:-}; verdict=${2:-}
    [ -n "$name" ] && { [ "$verdict" = ok ] || [ "$verdict" = blocked ]; } || die "usage: feature.sh reviewed <name> ok|blocked"
    b=feature/$name; list=$(with_branch "$b")
    for r in $list; do   # (uncommitted work on another branch is not this review's)
        [ "$(current "$r")" = "$b" ] && dirty "$r" && die "$(short "$r") has uncommitted changes: they are not part of the review; commit them first"
    done
    for r in $list; do
        mkdir -p "$(dirname "$(record "$r" "$b")")"
        echo "$(git -C "$r" rev-parse "$b") $(git -C "$r" rev-parse main) $verdict" > "$(record "$r" "$b")"
        echo "review $verdict   $(short "$r") at $(git -C "$r" rev-parse --short "$b")"
    done ;;
land)
    name=${1:-}; summary=${2:-}
    [ -n "$name" ] && [ -n "$summary" ] || die "usage: feature.sh land <name> \"<a one-line summary for the merge commits>\""
    b=feature/$name; list=$(with_branch "$b"); msg="Merge $b: $summary"
    again="on the branch: git merge main, resolve, commit; then review it again"
    for r in $list; do   # every check first: all repositories land or none
        s=$(short "$r"); cur=$(current "$r")
        whole "$r"
        [ "$cur" = "$b" ] || [ "$cur" = main ] || die "$s is on '${cur:-a detached HEAD}': land needs it on $b or main"
        dirty "$r" && die "$s has uncommitted changes"
        f=$(record "$r" "$b")
        [ -f "$f" ] || die "$s: $b has no review (the review skill, then feature.sh reviewed $name ok)"
        read -r sha msha verdict < "$f"
        [ "$sha" = "$(git -C "$r" rev-parse "$b")" ] || die "$s: $b changed after its review: review it again"
        [ "$msha" = "$(git -C "$r" rev-parse main)" ] || die "$s: main moved after the review of $b; $again"
        [ "$verdict" = ok ] || die "$s: the review of $b says blocked"
        git -C "$r" merge-tree --write-tree main "$b" >/dev/null 2>&1 || die "$s: $b does not merge into main without a conflict; $again"
        hook=$(git -C "$r" rev-parse --absolute-git-dir)/hooks/commit-msg   # the one the merge will run
        if [ -x "$hook" ]; then
            t=$(mktemp); echo "$msg" > "$t"
            (cd "$r" && "$hook" "$t") || { rm -f "$t"; die "the summary is refused as a commit message"; }
            rm -f "$t"
        fi
    done
    for r in $list; do
        s=$(short "$r")
        git -C "$r" switch -q main
        if [ "$(git -C "$r" rev-list --count "main..$b")" != 0 ]; then
            EVERYWARE_LAND=1 git -C "$r" merge -q --no-ff "$b" -m "$msg" || {
                EVERYWARE_LAND=1 git -C "$r" merge --abort 2>/dev/null || true
                die "$s: the merge failed and was undone there; the repositories listed above as landed are landed. Look at $s, then run land again"
            }
            echo "landed   $s   $(git -C "$r" log --format='%h %s' -1)"
        else
            echo "nothing to land   $s"
        fi
        git -C "$r" branch -q -d "$b"
        rm -f "$(record "$r" "$b")"
        git -C "$r" rev-parse main > "$(git -C "$r" rev-parse --absolute-git-dir)/everyware-main"   # where land left main
    done
    # the hooks in force are main's: a landed change of them takes effect now
    "$(dirname "$0")/setup-agents.sh" >/dev/null ;;
*) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 2 ;;
esac
