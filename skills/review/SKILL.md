---
name: review
description: Review a feature branch of SDV EveryWare before the user sees it - the reviewer is another agent than the author, reads the branch's diff, the files it touches and the project's rules, checks by the paths the branch touches (a component's role in ui-lib, the names contract of a design, a bundle that only sends information, an app that owns its content), runs the tests, and reports findings as blocker, fix or note with file and line; then records the verdict with feature.sh. Use when a feature branch is ready (feature.sh status shows commits and no review), when the user asks for a review of pending work, or before feature.sh land.
---

# Review a feature branch

Every change of the project lands the same way (`vendor/sdv-everyware/everyware/AGENTS.md`, "How
a change lands"): its own branch, this review, then the user's own look and word. The review
exists so that the user does not have to catch the author's mistakes.

**You are not the author.** A review by the agent that wrote the change repeats its blind spots:
it is done by another agent with a fresh context. If you wrote any of the branch, say so and hand
the review to another agent. **You do not fix anything:** you report; the author fixes.

Paths are from the tree's top; `FS=vendor/sdv-everyware/everyware/scripts/feature.sh`.

## 1. Read

```bash
$FS status                 # which repositories have the branch, its commits; and the guard of main:
                           # a line in capitals after a repository (HOOKS ..., core.hooksPath ...,
                           # MAIN WAS MOVED ...) is a blocker of its own, whatever the branch does
$FS diff <name>            # the commits and the change against main, every repository; it names
                           # the files it does not print (binaries, generated docs): look at those yourself
```

You were given, with the branch's name, **what the user asked for in the user's own words**. If
not, ask the author for it before you start: without it "does it do what was asked" cannot be
answered.

Then the rules the change is measured against: `vendor/sdv-everyware/everyware/AGENTS.md`
("Rules for every change") and the `AGENTS.md` of every repository the branch touches. Read the
changed files whole where the diff alone does not show whether a claim is true.

## 2. Check by path

| The branch touches | Check |
|---|---|
| `ui-lib/core/src`, `ui-lib/harry/src`, `ui-lib/components.json` | Is every component named and shaped by its **role**? No data of a domain, no mapping from data to a look, no domain text, no data source, no picture or name that belongs to one app, also not under a generic name. `scripts/check-roles.py` catches words; you catch the meaning. Does a new module have tests of its own? |
| `ui-lib/design` | Does the Library build (`design/build-library.sh`) and pass `everyware/design/figma/check-figma.sh` (no set over another)? Does the drawing match `components.json` (variants, the `#name/part` layers in every variant, nothing hidden)? |
| an app's `design/*-patch.js` | Library components only (no control drawn by hand)? Every `#<app>/...` name the code uses is in the patch and in `src/ui_model.rs`, spelled the same? Nothing hidden? Is the committed `data/<app>.dcf` built from this patch (`design/build.sh` reports the names)? |
| an app's `src` | Is what the app shows decided in the app (presenter, its own modules), not pushed into the library or a bundle? Are rules that need no screen in a module with host tests, and do the tests cover the new behaviour? |
| `everyware/bundles`, `catalog` | Does the bundle only send information (no decision about how it looks)? Are the catalog, the vsidl and the permissions changed together? SELinux, signing or security touched: was the user asked? |
| `everyware/design/figma`, any `scripts/` | Does the tool still give the same output for an unchanged input (build a design before and after, compare)? Is a script that agents run tested by a run, with its output in the report? |
| docs, `AGENTS.md`, skills | Is every statement true for the code as it is on the branch (commands run, paths exist, names spelled as in the code)? Current state only, English, no em dashes. |
| commit messages | Do they say what changed and why, and claim only what the diff does? |

## 3. Check always

- **Does it do what was asked,** all of it and nothing else? Name what is missing and what is
  beyond the request (another feature, a refactor nobody asked for, a file of another session).
- **Does it work?** Run what proves it and put the output in the report:
  `vendor/sdv-everyware/ui-lib/scripts/check.sh --all` for ui-lib (it regenerates the docs in
  place: files it changes are a finding, "the committed docs were stale", and you put them back
  with `git checkout -- docs` so that the tree is as you found it),
  `vendor/sdv-everyware/everyware/scripts/test-feature.sh` when the process's scripts or hooks
  changed, the host tests of a changed module, the build of a changed app
  (`m <app>_app`, after `ps -C soong_ui` shows no other build), the design builds. On the board
  only read; ask the author for the screenshots, do not stop what runs there.
- **Bugs:** wrong conditions, unhandled empty or missing data, a state that cannot be left, a
  name changed on one side of a contract, a script that fails silently.
- **Claims:** "verified", "identical", "all tests pass" in a message or a doc: is there a run
  behind each? What was not verified must be said as not verified.
- **Private:** no personal user id, work address, absolute home or mount path, Figma key or
  token (the hooks check the diff; you check new files and binaries' names too).

## 4. Report

One list, most severe first; every finding with the repository, `file:line`, what is wrong, the
rule or the reason, and what would fix it:

| Severity | Meaning | Lands? |
|---|---|---|
| **blocker** | breaks a rule of the project, a bug, an untrue claim, a missing test for new behaviour | no |
| **fix** | should change before the user looks: unclear name, a doc that lags, a weak test | after it is fixed or the user accepts it |
| **note** | an observation, a follow-up | yes |

Then what you ran and its result, and what you could not check. No praise, no summary of the
change: the author writes that for the user.

## 5. Record the verdict

```bash
$FS reviewed <name> blocked     # at least one blocker
$FS reviewed <name> ok          # none
```

The verdict is for the branch as it is at this moment: a new commit needs a new review (`land`
refuses otherwise). After the author's fixes, review the new commits and the findings they
answer, then record again.
