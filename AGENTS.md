# SDV EveryWare: the guide for coding agents

EveryWare is SDV Media for embedded devices that are not a car: a smart-home panel first. It
lives in the AOSP tree as several git repositories; this one, `everyware`, is
the platform side (service bundles, the catalog, docs, the Figma tools). Paths below are from
the tree's top.

Read this file, then the `AGENTS.md` nearest to your change, then the skill for your task.

## The repositories

| Repository | What it is | Its guide |
|---|---|---|
| `vendor/sdv-everyware/everyware` | service bundles, the catalog, the product, the docs (`docs/`), the Figma tools (`design/figma/`), scripts | this file |
| `vendor/sdv-everyware/ui-lib` | the UI library: components and nothing else | `ui-lib/AGENTS.md` |
| `packages/services/display_safety/utils/<name>-app` | one app each (smarthome, hub, tv, pairs, ...): its code, its design, its content | the app's `AGENTS.md` |
| `vendor/sdv-everyware/third_party/rust` | vendored Rust crates (rs-matter) | |
| `vendor/sdv-everyware/dnsproxyd` | the DNS proxy for the bundles | |
| `device/qcom/rb3` (sdv_media_db845c) | the board | |

Who owns what, in one line each: a **bundle** sends information; an **app** decides what it
looks like (composition, content, its own pictures); the **UI library** has components that
know no use case.

## Skills

Each is a `SKILL.md` with the steps; load the one whose "use when" fits.

| Skill | Where | Use when |
|---|---|---|
| `new-app` | `vendor/sdv-everyware/ui-lib/skills/new-app` | a new app (a new screen as its own harry app) from one sentence |
| `use-component` | `vendor/sdv-everyware/ui-lib/skills/use-component` | putting a library component on a screen |
| `ui-component` | `vendor/sdv-everyware/ui-lib/skills/ui-component` | a new library component, a variant, a transition; adopting or changing a component a designer drew in Figma |
| `sheet` | `vendor/sdv-everyware/ui-lib/skills/sheet` | a sheet, a dialog or a side panel over the screen |
| `figma-design` | `vendor/sdv-everyware/everyware/skills/figma-design` | a screen, a sheet or the designer's prototype of the panel |
| `service-bundle` | `vendor/sdv-everyware/everyware/skills/service-bundle` | a new SDV service bundle (publisher, subscriber, RPC, a bridge to the screen) |
| `harry-app` | `packages/services/display_safety/utils/smarthome-app/skills/harry-app` | writing, building, running or debugging a harry app |
| `review` | `vendor/sdv-everyware/everyware/skills/review` | a feature branch is ready: the review by another agent, before the user looks |

More to read: `docs/README.md` (the platform, page by page), `design/figma/README.md` (designs
as code), `vendor/sdv-everyware/ui-lib/design/WORKFLOW.md` (agents and designers on one
Library), `vendor/sdv-everyware/ui-lib/docs/llms.txt` (the library's rules).

## How a change lands

Every change, a line of a doc as well, goes the same way:

```
the user asks ──► 1 a branch feature/<name> in every repository the change touches
              ──► 2 the author works and commits on the branch only
              ──► 3 another agent reviews the branch (the review skill): blocker, fix, note
              ──► 4 the author fixes; the reviewer looks again; the verdict is recorded
              ──► 5 the author gives the user the package
              ──► 6 the user looks and says "land it" ──► merged into main, branch deleted
```

`scripts/feature.sh` does the mechanical part in every repository the change touches:

```bash
FS=vendor/sdv-everyware/everyware/scripts/feature.sh
$FS start <name> ui-lib pairs-app     # 1: the repositories the change touches (more later the same way)
$FS status                            # where every repository stands
$FS diff <name>                       # 3: what the reviewer reads
$FS reviewed <name> ok                # 4: the reviewer's verdict (ok or blocked)
$FS land <name> "<summary>"           # 6: only after the user's word
$FS accept-main <repo>                # main was moved another way and the user decided it is right
```

- **The reviewer is not the author.** The author starts another agent with a fresh context and
  gives it three things: the branch's name, what the user asked for in the user's own words, and
  the review skill. Not the author's explanation of why the change is right.
- **The package for the user** (step 5), in the author's reply: what was asked and what was
  done; the reviewer's findings and what became of each; what was run and its result, what was
  not verified; the built `.fig` files with their full paths; screenshots from the board when
  the screen changed; the commits per repository (`feature.sh status`).
- **`land` is run only after the user said so in the conversation.** The script checks every
  repository first and merges none if one fails: a review "ok" of the branch's last commit, made
  against the `main` of now, and a merge without a conflict. A branch that changed after its
  review, or whose `main` moved, is reviewed again; when it does not merge cleanly, `git merge
  main` on the branch, resolve, commit, review again.
- **One feature at a time per repository:** the tree, the build output and the board are
  shared. `start` refuses a repository that has another feature open, and `feature.sh status`
  lists every open feature; do not switch a branch another session works on.

What the process covers: `everyware`, `ui-lib`, `third_party/rust`, `dnsproxyd` and the apps
(`feature.sh` and the hooks). In the other repositories of the tree a change of ours goes the
same way by hand: a branch off that repository's base branch, the review, the user's word.

The hooks hold the frame: a commit on `main` is refused, and so is a move of `main` to another
commit that does not come from `feature.sh land`, however it is made (a merge, a fast-forward, a
cherry-pick, a revert, a reset, `branch -f`, a push into the repository).

- **Which hooks are in force:** `main`'s version, copied into each repository's `.git/hooks` by
  `scripts/setup-agents.sh` (a new repository, whose `main` has none yet, gets its own files).
  So they are the same on every branch, also one without hook files, and a change of a hook
  takes effect when it landed, not while it is on a branch.
- **What they do not hold:** the deletion of `main`, which git cannot tell from packing the refs;
  therefore also its replacement by a rename onto it (`git branch -M`) or by a delete and a
  create. `feature.sh status` says when `main` is not where `land` left it, when the installed
  hooks are not `main`'s, and when `core.hooksPath` takes git away from them.
- **When `status` reports one of these,** nothing starts and nothing lands in that repository
  (`start` and `land` refuse). Hooks that are not `main`'s or not installed: `setup-agents.sh`. A
  `main` that was moved: do not go on. Look at what `main` holds now (`git log`), tell the user
  what is there that no review saw, and only after the user's decision either put `main` back or
  `feature.sh accept-main <repo>`.
- **They guard against a mistake, not against intent.** `--no-verify` skips the checks of a
  commit (the author, the private strings, the attribution) but not the guard of `main`;
  `EVERYWARE_LAND=1` gets past that guard; neither is used without the user's word.
- **What no script here can know:** whether the reviewer was another agent, and whether the user
  said "land it". Both are rules.

`scripts/test-feature.sh` tries the rules of the scripts and the hooks case by case in a
throwaway tree, and what must keep working under the hooks (git gc, a rebase, a stash).

## Rules for every change

Working with the user:

- **Plan first.** Before changing anything that is not trivial: the plan, the options with
  their advantages, disadvantages and trade-offs, a recommendation; then wait for the choice.
- **The user decides** layouts and names of a design, security, signing and SELinux, and what
  belongs in the UI library. Ask; do not pick.
- **On the board** (the RB3 is shared): say before each command what it does and whether it
  changes the board's state; do not stop what you did not start; runtime changes only, never the
  image, unless asked.
- **After a design build** give the full path of the built `.fig`, what changed and where to
  look (the build script prints the path as its last line).
- **Report as it is.** A failed test with its output, a skipped step as skipped, what was not
  verified as not verified.

In the code:

- **Production, not workarounds:** remove the cause; a patch is a real fix or it is not made.
- **The UI library knows no use case** (`ui-lib/AGENTS.md`): what an app shows is the app's.
- **A design's names are a contract** with the app's `ui_model.rs`: change both in one commit.
- **Docs describe what is,** in English, without the history of how it came to be; no em dashes.

In git:

- **Commit on the feature branch** as the work goes, never on `main`; never push. What reaches
  `main` is the user's decision.
- **Read the staged diff before a commit:** bugs, claims that are not true, files of another
  session, anything private.
- Author `Daniel Georg <passenger6@gmail.com>`; no AI attribution and no co-author trailer.
- Nothing private in the history: no personal user id, no work e-mail address, no absolute path
  of a home or a mount, no Figma file key, no token. A Figma token is only ever exported by the
  user in their own shell.
- Messages in English: what changed and why.

The hooks in `scripts/git-hooks/` of each repository check the branch, the author, the private
strings and the attribution: a commit that breaks them is refused. The private words of the
person (a user id, a work address) are not in the hook but in `~/.config/everyware/private-words`,
outside git, one extended regex per line; `setup-agents.sh` writes the empty file, the person
fills it. `scripts/setup-agents.sh`
installs the hooks in every repository of the project (once per checkout, and after a hook
changed; `feature.sh land` does that, `feature.sh status` says when a copy is old).
