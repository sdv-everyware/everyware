#!/bin/bash
# The test of feature.sh, setup-agents.sh and the hooks in scripts/git-hooks: a throwaway tree
# with the project's layout, the files as they are in this checkout, the rules tried case by
# case, and what must keep working under the hooks. Nothing in the real tree is written. Not
# tried: the undo of a merge that fails after land's checks passed. From anywhere:
#
#   vendor/sdv-everyware/everyware/scripts/test-feature.sh     # "ok" per case; exit 1 on a failure
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
# (without its own tree the test would act on the real one)
T=$(mktemp -d) || exit 1
[ -n "$T" ] && [ -d "$T" ] || exit 1
trap 'rm -rf "$T"' EXIT
export EVERYWARE_TOP=$T
# (the person's own file of private words stays out of it: the test writes its own)
export EVERYWARE_PRIVATE_WORDS=$T/private-words
FS=$HERE/feature.sh
EW=$T/vendor/sdv-everyware/everyware UI=$T/vendor/sdv-everyware/ui-lib A=$T/packages/services/display_safety/utils/a-app B=$T/packages/services/display_safety/utils/b-app
fails=0
ok()   { echo "  ok    $1"; }
bad()  { echo "  FAIL  $1"; [ -z "${2:-}" ] || echo "$2" | sed 's/^/        /' | head -6; fails=$((fails + 1)); }
# refused "<case>" "<words the message must have>" cmd...: the command must fail and say why
refused() { local name=$1 words=$2 out; shift 2; if out=$("$@" 2>&1); then bad "$name: was not refused" "$out"; elif [[ "$out" == *"$words"* ]]; then ok "$name"; else bad "$name: refused without '$words'" "$out"; fi; }
passes()  { local name=$1 out; shift; if out=$("$@" 2>&1); then ok "$name"; else bad "$name" "$out"; fi; }
is()      { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1: '$2', expected '$3'"; fi; }
has()     { if [[ "$2" == *"$3"* ]]; then ok "$1"; else bad "$1: no '$3'" "$2"; fi; }
mk() {   # a new repository as new-app.sh makes it: the hooks installed from its files, then the first commit
    mkdir -p "$1/scripts" && cp -r "$HERE/git-hooks" "$1/scripts/" && echo one > "$1/f.txt" &&
        (cd "$1" && git init -q -b main && git config user.name 'Daniel Georg' && git config user.email passenger6@gmail.com &&
         mkdir -p .git/hooks && cp scripts/git-hooks/* .git/hooks/ && chmod +x .git/hooks/* && git add -A && git commit -q -m first)
}
commit() { (cd "$1" && echo "$2" >> "${3:-f.txt}" && git add -A && git commit -q -m "$2"); }
head_of() { git -C "$1" rev-parse "$2"; }
records() { find "$T" -path '*everyware-review*' -type f | wc -l; }

echo "== the hooks: what is refused"
passes "the first commit of a new repository is main's" mk "$EW"
mk "$UI"; mk "$A"; mk "$B"
passes "setup-agents.sh installs main's hooks and notes where main is" "$HERE/setup-agents.sh"
is "setup-agents.sh wrote the file of private words, comments only" "$(grep -vc '^#' "$EVERYWARE_PRIVATE_WORDS")" 0
is "the file of private words is the person's alone" "$(stat -c %a "$EVERYWARE_PRIVATE_WORDS")" 600
echo "# a word" > "$EVERYWARE_PRIVATE_WORDS"; "$HERE/setup-agents.sh" >/dev/null
is "setup-agents.sh does not write the file again" "$(cat "$EVERYWARE_PRIVATE_WORDS")" "# a word"
is "status has nothing to say about the guard of main" "$("$FS" status | grep ui-lib)" "vendor/sdv-everyware/ui-lib   on main"
m0=$(head_of "$UI" main)
refused "a commit on main" "no commits on main" commit "$UI" x
echo "== the hooks: the private words"
(cd "$UI" && git switch -q -c words)
printf '# the person'"'"'s words\r\n  secretu[s]er  \r\n\r\n   \r\nwork\\.example\\.com\r\n' > "$EVERYWARE_PRIVATE_WORDS"
# (a refused line stays in the file: taken out before the next case)
undo() { git -C "$UI" checkout -q HEAD -- f.txt; }
refused "a word of the file in a change (the file with CRLF, spaces, a blank line)" "something private" commit "$UI" "by secretuser"; undo
refused "a second word of the file" "something private" commit "$UI" "mail me at work.example.com"; undo
# (the name split in two: the hook must not see a token in this file)
refused "a Figma token in a change" "something private" commit "$UI" "FIGMA_TOKE""N=figd_0123456789abcdefghijklmnop"; undo
passes "a change with three spaces in a row, and nothing private" commit "$UI" "a   line"
mv "$EVERYWARE_PRIVATE_WORDS" "$T/words-aside"
w=$(commit "$UI" "without the file" 2>&1); has "without the file the hook warns and lets the commit through" "$w" "is missing, the private words are not checked"
is "the commit went through" "$(git -C "$UI" log -1 --format=%s)" "without the file"
mv "$T/words-aside" "$EVERYWARE_PRIVATE_WORDS"
(cd "$UI" && git switch -q main && git branch -q -D words)
git -C "$UI" reset -q --hard
(mkdir -p "$UI/sub" && cd "$UI/sub" && echo y > g.txt && git add -A)
refused "a commit on main from a subdirectory" "no commits on main" git -C "$UI/sub" commit -q -m x
git -C "$UI" reset -q --hard; rm -rf "$UI/sub"
refused "a commit on main with --no-verify" "main moves only through" git -C "$UI" commit -q --no-verify --allow-empty -m x
git -C "$UI" switch -q -c feature/h; commit "$UI" h1; h1=$(head_of "$UI" feature/h); git -C "$UI" switch -q main
echo mine > "$UI/other.txt"
refused "a fast-forward merge into main" "main moves only through" git -C "$UI" merge -q feature/h
git -C "$UI" reset -q --merge HEAD
is "the advice (reset --merge HEAD) keeps other uncommitted work" "$(cat "$UI/other.txt" 2>/dev/null)" mine
rm -f "$UI/other.txt"
refused "a merge commit on main" "feature.sh land" git -C "$UI" merge -q --no-ff feature/h -m x
git -C "$UI" merge --abort 2>/dev/null
refused "a cherry-pick onto main" "main moves only through" git -C "$UI" cherry-pick "$h1"
git -C "$UI" cherry-pick --abort 2>/dev/null; git -C "$UI" reset -q --hard
refused "a revert on main" "main moves only through" git -C "$UI" revert --no-edit HEAD
git -C "$UI" reset -q --merge HEAD
is "the advice (reset --merge HEAD) clears a refused revert" "$(git -C "$UI" status --porcelain | wc -l)" 0
refused "a reset of main to another commit" "main moves only through" git -C "$UI" reset -q --hard "$h1"
git -C "$UI" reset -q --hard
git -C "$UI" switch -q feature/h
refused "branch -f main" "main moves only through" git -C "$UI" branch -f main "$h1"
refused "update-ref of main" "main moves only through" git -C "$UI" update-ref refs/heads/main "$h1"
refused "a push of a branch into main of the same repository" "main moves only through" git -C "$UI" push -q . feature/h:main
(cd "$UI" && git switch -q -c old "$m0" && git rm -q -r scripts/git-hooks && git commit -q -m "a branch without the hook files")
refused "main is held on a branch that has no hook files" "main moves only through" git -C "$UI" branch -f main "$h1"
is "main did not move" "$(head_of "$UI" main)" "$m0"
git -C "$UI" switch -q feature/h; git -C "$UI" branch -q -D old
# what the hook cannot refuse (git shows it only a deletion): status must say it
git -C "$UI" switch -q -c stray; git -C "$UI" branch -q -M stray main
has "a rename onto main is not refused, and status says that main was moved" "$("$FS" status | grep ui-lib)" "MAIN WAS MOVED WITHOUT land"
refused "nothing starts while main was moved without land" "the guard of main is not whole" "$FS" start blocked ui-lib
EVERYWARE_LAND=1 git -C "$UI" branch -q -f feature/h "$h1"; EVERYWARE_LAND=1 git -C "$UI" switch -q feature/h; EVERYWARE_LAND=1 git -C "$UI" branch -q -f main "$m0"
is "main back where land left it: status is quiet again" "$("$FS" status | grep ui-lib)" "vendor/sdv-everyware/ui-lib   on feature/h"
git -C "$UI" config core.hooksPath /nowhere
has "status says when core.hooksPath takes git away from the installed hooks" "$("$FS" status | grep ui-lib)" "core.hooksPath IS SET"
git -C "$UI" config --unset core.hooksPath
(cd "$UI" && printf '#!/bin/bash\nexit 0\n' > scripts/git-hooks/reference-transaction)
"$HERE/setup-agents.sh" >/dev/null
refused "setup-agents.sh on a branch that changed a hook keeps main's hook" "main moves only through" git -C "$UI" branch -f main "$h1"
git -C "$UI" checkout -q -- scripts/git-hooks/reference-transaction
refused "a commit by another author" "the author is" git -C "$UI" -c user.email=someone@example.com commit -q --allow-empty -m x
refused "a co-author trailer" "no AI attribution" git -C "$UI" commit -q --allow-empty -m "x

Co-Authored-B""y: Someone <a@b.c>"

echo "== the hooks: what keeps working"
passes "a commit on a feature branch" commit "$UI" h2
passes "git pack-refs" git -C "$UI" pack-refs --all
passes "git gc" git -C "$UI" gc -q
is "main is still there after packing" "$(head_of "$UI" main)" "$m0"
passes "a stash and its pop" bash -c "cd '$UI' && echo w >> f.txt && git stash -q && git stash pop -q && git checkout -q -- f.txt"
passes "a rebase of a feature branch" bash -c "cd '$UI' && git switch -q -c feature/r '$h1' && echo r > r.txt && git add -A && git commit -q -m r && git rebase -q feature/h && git switch -q feature/h && git branch -q -D feature/r"
git -C "$UI" switch -q main; git -C "$UI" branch -q -D feature/h

echo "== start, status"
refused "start without a repository" "usage" "$FS" start one
refused "start in an unknown repository" "no repository" "$FS" start one nowhere
passes "start in two repositories" "$FS" start one ui-lib a-app
is "the branch is checked out" "$(git -C "$UI" branch --show-current)" feature/one
refused "start of a second feature in a repository that has one" "another change is open" "$FS" start two a-app
git -C "$A" switch -q main
refused "the same when the other feature is not checked out" "has the open feature feature/one" "$FS" start two a-app
git -C "$A" switch -q --detach
refused "start on a detached HEAD" "a detached HEAD" "$FS" start one a-app
git -C "$A" switch -q feature/one
echo dirt >> "$B/f.txt"
refused "start in a repository with uncommitted changes" "uncommitted" "$FS" start one b-app
git -C "$B" reset -q --hard
passes "start again adds a repository" "$FS" start one b-app
has "status shows the branch and its review" "$("$FS" status)" "* feature/one: 0 commits, review: none"
git -C "$B" switch -q main
has "status shows a feature that is not checked out" "$("$FS" status | grep -A1 b-app)" "  feature/one:"
git -C "$B" switch -q feature/one
echo "# changed" >> "$B/.git/hooks/commit-msg"
has "status says when the installed hooks are not main's" "$("$FS" status | grep b-app)" "HOOKS ARE NOT MAIN'S"
"$HERE/setup-agents.sh" >/dev/null
commit "$UI" s1
has "status lists a branch's commits" "$("$FS" status)" "  s1"
(mkdir -p "$T/packages/services/display_safety/utils/old-app" && cd "$T/packages/services/display_safety/utils/old-app" && git init -q -b master && git -c user.name=x -c user.email=x@y.z commit -q --allow-empty -m first)
refused "start in a repository without main" "has no branch main" "$FS" start one old-app
has "status goes on past a repository without main" "$("$FS" status)" "(no branch main)"

echo "== diff, reviewed"
refused "diff of an unknown feature" "no repository has the branch" "$FS" diff nothing
refused "reviewed of an unknown feature" "no repository has the branch" "$FS" reviewed nothing ok
refused "reviewed with another verdict" "usage" "$FS" reviewed one fine
commit "$UI" u1; commit "$A" a1; echo binary > "$A/x.dcf"; (cd "$A" && git add -A && git commit -q -m dcf)
d=$("$FS" diff one)
[[ "$d" == *"+u1"* && "$d" == *"x.dcf"* && "$d" == *"not printed below"* ]] && ok "diff prints the change and names what it does not print" || bad "diff" "$d"
echo dirt >> "$UI/f.txt"
refused "reviewed with uncommitted changes on the branch" "not part of the review" "$FS" reviewed one ok
git -C "$UI" reset -q --hard

echo "== land"
refused "land without a review" "has no review" "$FS" land one "a summary"
"$FS" reviewed one blocked >/dev/null
refused "land of a blocked branch" "says blocked" "$FS" land one "a summary"
"$FS" reviewed one ok >/dev/null
commit "$UI" u2
refused "land after a commit that was not reviewed" "changed after its review" "$FS" land one "a summary"
"$FS" reviewed one ok >/dev/null
refused "land with an attribution in the summary" "refused as a commit message" "$FS" land one "x Co-Authored-B""y: a <a@b.c>"
is "nothing landed by the refused land" "$(git -C "$UI" log --oneline main | wc -l)" 1
echo dirt >> "$A/f.txt"
refused "land with uncommitted changes" "uncommitted" "$FS" land one "a summary"
git -C "$A" reset -q --hard
git -C "$A" switch -q -c side
refused "land with a repository on another branch" "land needs it on" "$FS" land one "a summary"
git -C "$A" switch -q feature/one; git -C "$A" branch -q -D side
echo "# a hook changed on the branch" >> "$A/scripts/git-hooks/commit-msg"; (cd "$A" && git add -A && git commit -q -m "hook"); "$FS" reviewed one ok >/dev/null
passes "land of a reviewed branch" "$FS" land one "the first feature"
is "no alarm after a land" "$("$FS" status | grep ui-lib)" "vendor/sdv-everyware/ui-lib   on main"
is "main has a merge commit" "$(git -C "$UI" log --format=%s -1 main)" "Merge feature/one: the first feature"
is "the branch is gone" "$(git -C "$UI" branch --list feature/one | wc -l)" 0
is "a repository with no commits on the branch is back on main" "$(git -C "$B" branch --show-current)" main
is "no record is left in any repository" "$(records)" 0
passes "land installed the hook that changed, once it was on main" cmp -s "$A/scripts/git-hooks/commit-msg" "$A/.git/hooks/commit-msg"

echo "== land: all or none, and a main that moved"
"$FS" start two ui-lib a-app >/dev/null; commit "$UI" t1; commit "$A" t-conflict
EVERYWARE_LAND=1 bash -c "cd '$A' && git switch -q main && echo other >> f.txt && git commit -q --no-verify -am 'main moves' && git switch -q feature/two"
"$FS" reviewed two ok >/dev/null
refused "nothing lands while main was moved without land" "the guard of main is not whole" "$FS" land two "x"
passes "accept-main, after the user's decision, takes main as it is" "$FS" accept-main a-app
u=$(head_of "$UI" main)
refused "land when one repository would conflict" "does not merge into main without a conflict" "$FS" land two "x"
is "no repository landed" "$(head_of "$UI" main)" "$u"
(cd "$A" && git merge -q main >/dev/null 2>&1; printf 'one\na1\nother\nt-conflict\n' > f.txt; git add -A; git commit -q -m "merge main")
"$FS" reviewed two ok >/dev/null
EVERYWARE_LAND=1 bash -c "cd '$UI' && git switch -q main && echo elsewhere > e.txt && git add -A && git commit -q --no-verify -m 'main moves without a conflict' && git switch -q feature/two"
"$FS" accept-main ui-lib >/dev/null
refused "land when main moved after the review" "main moved after the review" "$FS" land two "x"
(cd "$UI" && git merge -q main -m "merge main" >/dev/null 2>&1)
"$FS" reviewed two ok >/dev/null
passes "land after main was merged into the branch and it was reviewed again" "$FS" land two "the second feature"
"$FS" start three ui-lib >/dev/null; commit "$UI" r1; "$FS" reviewed three ok >/dev/null
"$FS" start four a-app >/dev/null; commit "$A" f1; "$FS" reviewed four ok >/dev/null; "$FS" land four "the fourth" >/dev/null
passes "a feature in another repository does not disturb this one" "$FS" land three "the third"
# two branches whose names differ only by / and _, open at once (made by hand: start allows one)
(cd "$UI" && git switch -q -c feature/a/b && echo ab >> f.txt && git add -A && git commit -q -m ab)
"$FS" reviewed a/b ok >/dev/null
(cd "$UI" && git switch -q -c feature/a_b main && echo a_b >> f.txt && git add -A && git commit -q -m a_b)
s=$("$FS" status)
[[ "$s" == *"feature/a/b: 1 commits, review: ok"* && "$s" == *"feature/a_b: 1 commits, review: none"* ]] && ok "a/b and a_b have records of their own" || bad "records of a/b and a_b" "$s"

echo "== the installation"
git -C "$UI" switch -q main 2>/dev/null; EVERYWARE_LAND=1 git -C "$UI" branch -q -D feature/a/b feature/a_b
"$FS" start five a-app >/dev/null
(cd "$A" && git rm -q scripts/git-hooks/pre-merge-commit && git commit -q -m "one hook less")
"$FS" reviewed five ok >/dev/null; "$FS" land five "one hook less" >/dev/null
[ ! -e "$A/.git/hooks/pre-merge-commit" ] && ok "a hook that left main is removed from the installed ones" || bad "a removed hook stays installed"
M=$T/packages/services/display_safety/utils/m-app
(mkdir -p "$M/scripts" && cp -r "$HERE/git-hooks" "$M/scripts/" && cd "$M" && git init -q -b master && git config user.name 'Daniel Georg' && git config user.email passenger6@gmail.com && git add -A && git commit -q -m first)
"$HERE/setup-agents.sh" >/dev/null
git -C "$M" branch -m main
is "no false alarm in a repository that got its main after the installation" "$("$FS" status | grep m-app)" "packages/services/display_safety/utils/m-app   on main"

echo
[ $fails = 0 ] && echo "all ok" || echo "$fails failed"
[ $fails = 0 ]
